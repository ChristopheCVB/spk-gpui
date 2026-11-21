mod avatars;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use async_channel::{Receiver, Sender};
use cookie_store::{CookieStore, serde::json};
use gpui_kit::RenderImage;
use tokio::task::{JoinSet, spawn_blocking};
use url::Url;

use crate::api::{
    Channel, Connection, Failure, Frame, Member, Profile, SocketEvent, SpkClient, User, VoiceFlags,
    VoiceSocket,
};
use crate::sounds::{Sound, SoundSettings, VoiceSounds};
use crate::voice::{
    self, Call, Device, DeviceKind, DevicePreferences, Devices, Intent, Issue as VoiceIssue,
    Report as VoiceReport, Status as VoiceStatus, Update as VoiceUpdate,
};
use avatars::AvatarLoads;

#[cfg(test)]
pub(crate) mod tests;

const SESSION_EXPIRED: &str = "Your session expired. Sign in again.";
const STORE_UNAVAILABLE: &str =
    "Secure storage is unavailable. Your sign-in lasts until you close SPK.";
const DELETE_FAILED: &str = "Could not remove the saved session from secure storage.";

pub enum Command {
    SignIn { email: String, password: String },
    Restore,
    SignOut,
    Refresh,
    JoinVoice(String),
    LeaveVoice,
    ToggleMute,
    ToggleDeafen,
    RetryMicrophone,
    RefreshDevices,
    ChooseDevice(DeviceKind, Option<Device>),
}

pub enum View {
    Busy(&'static str),
    SignIn {
        message: Option<&'static str>,
        retry_restore: bool,
    },
    SignedIn(Box<Sidebar>),
    RetrySignOut,
}

#[derive(Clone)]
pub struct Sidebar {
    pub profile: Profile,
    pub channels: Option<Vec<Channel>>,
    pub connection: Connection,
    pub notice: Option<&'static str>,
    pub load_failed: bool,
    pub voice: Option<VoiceView>,
    pub switching_to: Option<String>,
    pub voice_notice: Option<&'static str>,
    voice_rejoin: Option<String>,
    pub voice_intent: Intent,
    pub devices: Arc<Devices>,
    pub device_notice: Option<&'static str>,
    pub avatars: Arc<HashMap<String, Arc<RenderImage>>>,
    users: Arc<HashMap<String, User>>,
    rooms: HashMap<String, RoomMember>,
}

#[derive(Clone)]
pub struct VoiceView {
    pub channel_id: String,
    pub status: VoiceStatus,
    pub media: Option<Arc<VoiceReport>>,
    play_leave: bool,
}

impl VoiceView {
    fn joining(channel_id: String) -> Self {
        Self {
            channel_id,
            status: VoiceStatus::Joining,
            media: None,
            play_leave: false,
        }
    }

    fn listening(&self) -> bool {
        matches!(
            self.status,
            VoiceStatus::Connected | VoiceStatus::ListenOnly | VoiceStatus::Reconnecting
        )
    }

    fn begin_leave(&mut self, explicit: bool) -> bool {
        if self.status == VoiceStatus::Leaving {
            return false;
        }
        self.play_leave = explicit && self.listening();
        self.status = VoiceStatus::Leaving;
        self.media = None;
        true
    }

    fn update_media(&mut self, report: Arc<VoiceReport>) -> bool {
        if self.status == VoiceStatus::Leaving {
            return false;
        }
        let was_listening = self.listening();
        self.status = report.status;
        self.media = Some(report);
        !was_listening && self.listening()
    }
}

#[derive(Clone)]
struct RoomMember {
    channel_id: String,
    flags: VoiceFlags,
}

pub struct Participant<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub flags: VoiceFlags,
}

impl Sidebar {
    fn new(profile: Profile, notice: Option<&'static str>) -> Self {
        Self {
            profile,
            channels: None,
            connection: Connection::Connecting,
            notice,
            load_failed: false,
            voice: None,
            switching_to: None,
            voice_notice: None,
            voice_rejoin: None,
            voice_intent: Intent::default(),
            devices: Arc::default(),
            device_notice: None,
            avatars: Arc::default(),
            users: Arc::default(),
            rooms: HashMap::new(),
        }
    }

    fn has_voice_channel(&self, channel_id: &str) -> bool {
        self.channels
            .iter()
            .flatten()
            .any(|channel| channel.kind == "voice" && channel.id == channel_id)
    }

    fn request_voice(&mut self, call: &mut Option<Call>, channel_id: String) {
        if self.has_voice_channel(&channel_id)
            && self
                .voice
                .as_ref()
                .is_none_or(|voice| voice.channel_id != channel_id)
        {
            self.switching_to = Some(channel_id);
            self.cancel_voice(call, None);
        }
    }

    fn cancel_voice(&mut self, call: &mut Option<Call>, notice: Option<&'static str>) {
        if let (Some(call), Some(voice)) = (call, &mut self.voice)
            && voice.begin_leave(notice.is_none())
        {
            call.cancel();
            self.voice_notice = notice;
        }
    }

    fn finish_voice(&mut self, result: Result<(), VoiceIssue>) -> bool {
        let voice = self.voice.take();
        self.voice_rejoin = voice
            .as_ref()
            .filter(|voice| {
                voice.status != VoiceStatus::Leaving
                    && matches!(result, Err(VoiceIssue::LiveKitDisconnected))
            })
            .map(|voice| voice.channel_id.clone());
        if let Err(issue) = result {
            self.voice_notice.get_or_insert_with(|| issue.message());
        }
        result.is_ok() && voice.is_some_and(|voice| voice.play_leave)
    }

    pub fn rejoin_channel(&self) -> Option<&str> {
        let id = self.voice_rejoin.as_deref()?;
        (matches!(self.connection, Connection::Online) && self.has_voice_channel(id)).then_some(id)
    }

    pub fn participants(&self, channel: &str) -> Vec<Participant<'_>> {
        let mut people = self
            .rooms
            .iter()
            .filter(|(_, member)| member.channel_id == channel)
            .map(|(user, member)| Participant {
                id: user,
                name: self
                    .users
                    .get(user)
                    .filter(|user| !user.banned)
                    .and_then(User::name)
                    .unwrap_or("Unknown user"),
                flags: member.flags,
            })
            .collect::<Vec<_>>();
        people.sort_unstable_by(|left, right| (left.name, left.id).cmp(&(right.name, right.id)));
        people
    }

    pub fn people(&self) -> Vec<(&str, &str)> {
        let Some(me) = self.profile.id.as_deref() else {
            return Vec::new();
        };
        let mut people = self
            .users
            .values()
            .filter(|user| user.id != me && !user.banned)
            .filter_map(|user| Some((user.id.as_str(), user.name()?)))
            .collect::<Vec<_>>();
        people.sort_by_cached_key(|(id, name)| (name.to_lowercase(), *id));
        people
    }

    pub fn avatar(&self, id: Option<&str>) -> Option<&Arc<RenderImage>> {
        self.avatars.get(id?)
    }

    fn avatar_key(&self, id: &str) -> Option<&str> {
        match self.users.get(id) {
            Some(user) if !user.banned => user.avatar_key.as_deref(),
            None if self.profile.id.as_deref() == Some(id) => self.profile.avatar_key.as_deref(),
            _ => None,
        }
        .filter(|key| !key.is_empty())
    }
}

struct DeviceSettings {
    path: Option<PathBuf>,
    preferred: DevicePreferences,
    notice: Option<&'static str>,
}

impl DeviceSettings {
    fn load() -> Self {
        let path = DevicePreferences::path();
        let (preferred, notice) = match path.as_deref().map(DevicePreferences::load) {
            Some(Ok(preferred)) => (preferred, None),
            Some(Err(_)) => (
                DevicePreferences::default(),
                Some("Could not read saved audio devices. Using System default."),
            ),
            None => (
                DevicePreferences::default(),
                Some("Audio device choices will only last until you close SPK."),
            ),
        };
        Self {
            path,
            preferred,
            notice,
        }
    }

    async fn save(&mut self, preferred: &DevicePreferences) {
        self.preferred = preferred.clone();
        let path = self.path.clone();
        let preferred = preferred.clone();
        let saved = spawn_blocking(move || path.is_some_and(|path| preferred.save(&path).is_ok()))
            .await
            .unwrap_or(false);
        self.notice = (!saved).then_some(
            "Could not save audio devices. Your choices will only last until you close SPK.",
        );
    }
}

struct SavedSession {
    origin: String,
    persisted: Option<Vec<u8>>,
}

impl SavedSession {
    async fn read(&mut self) -> Result<Option<CookieStore>, &'static str> {
        let origin = self.origin.clone();
        let bytes =
            spawn_blocking(
                move || match keyring::Entry::new("spk-gpui", &origin)?.get_secret() {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(error) => Err(error),
                },
            )
            .await
            .map_err(|_| STORE_UNAVAILABLE)?
            .map_err(|_| STORE_UNAVAILABLE)?;
        let Some(bytes) = bytes else { return Ok(None) };
        match json::load(bytes.as_slice()) {
            Ok(cookies) => {
                self.persisted = Some(bytes);
                Ok(Some(cookies))
            }
            Err(_) => {
                self.clear().await?;
                Err("The saved session could not be read. Sign in again.")
            }
        }
    }

    async fn save(&mut self, client: &SpkClient) -> Result<(), &'static str> {
        let bytes = {
            let cookies = client.cookies().lock().map_err(|_| STORE_UNAVAILABLE)?;
            let mut bytes = Vec::new();
            json::save(&cookies, &mut bytes).map_err(|_| STORE_UNAVAILABLE)?;
            bytes
        };

        if self.persisted.as_ref() == Some(&bytes) {
            return Ok(());
        }
        if bytes == b"[]" {
            self.clear().await?;
            return Err(
                "SPK did not issue a persistent session. Sign in again after closing the app.",
            );
        }

        let origin = self.origin.clone();
        self.persisted = Some(
            spawn_blocking(move || {
                keyring::Entry::new("spk-gpui", &origin)?.set_secret(&bytes)?;
                Ok::<_, keyring::Error>(bytes)
            })
            .await
            .map_err(|_| STORE_UNAVAILABLE)?
            .map_err(|_| STORE_UNAVAILABLE)?,
        );
        Ok(())
    }

    async fn clear(&mut self) -> Result<(), &'static str> {
        let origin = self.origin.clone();
        spawn_blocking(move || {
            match keyring::Entry::new("spk-gpui", &origin)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(|_| DELETE_FAILED)?
        .map_err(|_| DELETE_FAILED)?;
        self.persisted = None;
        Ok(())
    }
}

type StartCall = fn(String, SpkClient, VoiceSocket, Intent) -> Call;

struct VoiceSettings {
    socket_endpoint: String,
    start_call: StartCall,
    devices: DeviceSettings,
    sounds: VoiceSounds,
}

pub async fn run(
    commands: Receiver<Command>,
    views: Sender<View>,
    sound_settings: tokio::sync::watch::Receiver<SoundSettings>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let base = Url::parse(env!("SPK_URL")).expect("validated SPK URL");
    let mut saved = SavedSession {
        origin: base.origin().ascii_serialization(),
        persisted: None,
    };
    let mut next = Command::Restore;
    let mut settings = VoiceSettings {
        socket_endpoint: env!("SPK_WS_URL").to_owned(),
        start_call: Call::start,
        devices: DeviceSettings::load(),
        sounds: VoiceSounds::new(sound_settings),
    };
    loop {
        let auth = match next {
            Command::Restore => restore(&base, &mut saved).await,
            Command::SignIn { email, password } => sign_in(&base, email, password)
                .await
                .map(Some)
                .map_err(|failure| (false, failure.sign_in_message())),
            _ => return,
        };

        let (message, retry_restore) = match auth {
            Ok(Some((client, profile))) => {
                let notice = saved.save(&client).await.err();
                let end = watch_session(
                    &client,
                    profile,
                    notice,
                    &mut saved,
                    &mut settings,
                    &commands,
                    &views,
                )
                .await;
                let warning = match end {
                    SessionEnd::Closed => return,
                    SessionEnd::Invalidated(reason) => {
                        if views
                            .send(View::Busy("Clearing saved session…"))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        Some(saved.clear().await.err().unwrap_or(reason))
                    }
                    SessionEnd::SignOut => {
                        let Some(warning) = sign_out(&client, &mut saved, &commands, &views).await
                        else {
                            return;
                        };
                        warning
                    }
                };
                (warning, false)
            }
            Ok(None) => (None, false),
            Err((retry_restore, message)) => (Some(message), retry_restore),
        };
        let Some(command) = login(&commands, &views, message, retry_restore).await else {
            return;
        };
        next = command;
    }
}

async fn restore(
    base: &Url,
    saved: &mut SavedSession,
) -> Result<Option<(SpkClient, Profile)>, (bool, &'static str)> {
    const RETRY: (bool, &str) = (true, "Could not verify your saved session. Try again.");
    let Some(cookies) = saved.read().await.map_err(|message| (false, message))? else {
        return Ok(None);
    };
    let client = SpkClient::new(base.clone(), cookies).map_err(|_| RETRY)?;
    match client.get("/api/profile/me").await {
        Ok(profile) => Ok(Some((client, profile))),
        Err(failure) if failure.unauthorized() => {
            let warning = saved.clear().await.err().unwrap_or(SESSION_EXPIRED);
            Err((false, warning))
        }
        Err(_) => Err(RETRY),
    }
}

async fn login(
    commands: &Receiver<Command>,
    views: &Sender<View>,
    message: Option<&'static str>,
    retry_restore: bool,
) -> Option<Command> {
    views
        .send(View::SignIn {
            message,
            retry_restore,
        })
        .await
        .ok()?;
    while let Ok(command) = commands.recv().await {
        if matches!(command, Command::SignIn { .. } | Command::Restore) {
            return Some(command);
        }
    }
    None
}

async fn sign_in(
    base: &Url,
    email: String,
    password: String,
) -> Result<(SpkClient, Profile), Failure> {
    let client = SpkClient::new(base.clone(), CookieStore::default())?;
    client.sign_in(email, password).await?;
    let profile = client.get("/api/profile/me").await?;
    Ok((client, profile))
}

async fn sign_out(
    client: &SpkClient,
    saved: &mut SavedSession,
    commands: &Receiver<Command>,
    views: &Sender<View>,
) -> Option<Option<&'static str>> {
    loop {
        views.send(View::Busy("Signing out…")).await.ok()?;
        let (remote, local) = tokio::join!(client.sign_out(), saved.clear());
        match (remote, local) {
            (Ok(()), cleared) => return Some(cleared.err()),
            (Err(error), cleared) if error.unauthorized() => return Some(cleared.err()),
            (Err(_), Ok(())) => {
                return Some(Some(
                    "Signed out on this device. SPK could not confirm server sign-out.",
                ));
            }
            (Err(_), Err(_)) => {
                views.send(View::RetrySignOut).await.ok()?;
                loop {
                    if matches!(commands.recv().await.ok()?, Command::SignOut) {
                        break;
                    }
                }
            }
        }
    }
}

enum SessionEnd {
    Closed,
    SignOut,
    Invalidated(&'static str),
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Resource {
    Channels,
    Users,
    Presence,
}

const SIDEBAR_RESOURCES: [Resource; 3] = [Resource::Channels, Resource::Users, Resource::Presence];

enum Data {
    Channels(Vec<Channel>),
    Users(Vec<User>),
    Presence(Vec<Member>),
}

#[derive(Default)]
struct Refresh {
    tasks: JoinSet<(Resource, Result<Data, Failure>)>,
    in_flight: HashSet<Resource>,
    dirty: HashSet<Resource>,
    failed: HashSet<Resource>,
    presence_updates: Option<HashMap<String, Option<RoomMember>>>,
    presence_ready: bool,
}

impl Refresh {
    fn fetch(&mut self, client: &SpkClient, resource: Resource) {
        if !self.in_flight.insert(resource) {
            self.dirty.insert(resource);
            return;
        }
        if resource == Resource::Presence {
            self.presence_updates = Some(HashMap::new());
        }
        let client = client.clone();
        self.tasks.spawn(async move {
            let data = match resource {
                Resource::Channels => client.get("/api/channels").await.map(Data::Channels),
                Resource::Users => client.get("/api/users").await.map(Data::Users),
                Resource::Presence => client.get("/api/voice").await.map(Data::Presence),
            };
            (resource, data)
        });
    }

    fn fetch_all(&mut self, client: &SpkClient) {
        for resource in SIDEBAR_RESOURCES {
            self.fetch(client, resource);
        }
    }

    fn update_presence(
        &mut self,
        sidebar: &mut Sidebar,
        user_id: String,
        channel_id: Option<String>,
        flags: VoiceFlags,
    ) -> Option<Sound> {
        let cue = sidebar
            .voice
            .as_ref()
            .filter(|voice| self.presence_ready && voice.listening())
            .filter(|_| {
                sidebar
                    .profile
                    .id
                    .as_deref()
                    .is_some_and(|me| me != user_id)
            })
            .and_then(|voice| {
                let was_here = sidebar
                    .rooms
                    .get(&user_id)
                    .is_some_and(|member| member.channel_id == voice.channel_id);
                let is_here = channel_id.as_deref() == Some(voice.channel_id.as_str());
                match (was_here, is_here) {
                    (false, true) => Some(Sound::Join),
                    (true, false) => Some(Sound::Leave),
                    _ => None,
                }
            });
        let member = channel_id.map(|channel_id| RoomMember { channel_id, flags });
        if let Some(updates) = &mut self.presence_updates {
            updates.insert(user_id.clone(), member.clone());
        }
        apply_presence(&mut sidebar.rooms, user_id, member);
        cue
    }

    fn complete(&mut self, sidebar: &mut Sidebar, resource: Resource, data: Result<Data, Failure>) {
        self.in_flight.remove(&resource);
        match data {
            Ok(data) => {
                self.failed.remove(&resource);
                match data {
                    Data::Channels(channels) => sidebar.channels = Some(channels),
                    Data::Users(users) => {
                        sidebar.users = Arc::new(
                            users
                                .into_iter()
                                .map(|user| (user.id.clone(), user))
                                .collect(),
                        );
                    }
                    Data::Presence(members) => {
                        self.presence_ready = true;
                        sidebar.rooms = members
                            .into_iter()
                            .map(|member| {
                                (
                                    member.user_id,
                                    RoomMember {
                                        channel_id: member.channel_id,
                                        flags: member.flags,
                                    },
                                )
                            })
                            .collect();
                        for (user, member) in self.presence_updates.take().into_iter().flatten() {
                            apply_presence(&mut sidebar.rooms, user, member);
                        }
                    }
                }
            }
            Err(_) => {
                self.failed.insert(resource);
                if resource == Resource::Presence {
                    self.presence_updates = None;
                }
            }
        }
        sidebar.load_failed = !self.failed.is_empty();
    }
}

fn apply_presence(
    rooms: &mut HashMap<String, RoomMember>,
    user_id: String,
    member: Option<RoomMember>,
) {
    if let Some(member) = member {
        rooms.insert(user_id, member);
    } else {
        rooms.remove(&user_id);
    }
}

async fn watch_session(
    client: &SpkClient,
    profile: Profile,
    notice: Option<&'static str>,
    saved: &mut SavedSession,
    settings: &mut VoiceSettings,
    commands: &Receiver<Command>,
    views: &Sender<View>,
) -> SessionEnd {
    let (events, socket) = async_channel::bounded(32);
    let mut workers = JoinSet::new();
    let connection = client.clone();
    let socket_endpoint = settings.socket_endpoint.clone();
    workers.spawn(async move { connection.socket(&socket_endpoint, events).await });

    let mut refresh = Refresh::default();
    refresh.fetch_all(client);
    let mut sidebar = Sidebar::new(profile, notice);
    let mut avatars = AvatarLoads::default();
    avatars.fill(client, &sidebar);
    sidebar.voice_intent.preferred = settings.devices.preferred.clone();
    sidebar.device_notice = settings.devices.notice;
    sidebar.devices = idle_devices();
    let mut voice_socket: Option<VoiceSocket> = None;
    let mut call = None;
    let end = loop {
        if commands.is_closed() {
            break SessionEnd::Closed;
        }
        if call.is_none()
            && let Some(channel_id) = sidebar.switching_to.take()
        {
            if let Some(socket) = &voice_socket
                && sidebar.has_voice_channel(&channel_id)
            {
                call = Some((settings.start_call)(
                    channel_id.clone(),
                    client.clone(),
                    socket.clone(),
                    sidebar.voice_intent.clone(),
                ));
                sidebar.voice = Some(VoiceView::joining(channel_id));
                sidebar.voice_notice = None;
                sidebar.voice_rejoin = None;
            } else {
                sidebar.voice_notice = Some(if voice_socket.is_none() {
                    VoiceIssue::Socket.message()
                } else {
                    "This voice channel is no longer available."
                });
            }
        }
        if views
            .send(View::SignedIn(Box::new(sidebar.clone())))
            .await
            .is_err()
        {
            break SessionEnd::Closed;
        }
        tokio::select! {
            command = commands.recv() => match command {
                Err(_) => break SessionEnd::Closed,
                Ok(Command::SignOut) => break SessionEnd::SignOut,
                Ok(Command::Refresh) => refresh.fetch_all(client),
                Ok(Command::JoinVoice(channel_id)) if voice_socket.is_some() => {
                    sidebar.request_voice(&mut call, channel_id);
                }
                Ok(Command::LeaveVoice) => {
                    sidebar.switching_to = None;
                    sidebar.cancel_voice(&mut call, None);
                }
                Ok(command @ (Command::ToggleMute | Command::ToggleDeafen | Command::RetryMicrophone | Command::RefreshDevices | Command::ChooseDevice(..))) => {
                    match command {
                        Command::ToggleMute => {
                            sidebar.voice_intent.mute.toggle_mic();
                            settings.sounds.play(Sound::Mute, !sidebar.voice_intent.mute.effective_muted());
                        }
                        Command::ToggleDeafen => {
                            sidebar.voice_intent.mute.toggle_deafen();
                            settings.sounds.play(Sound::Deafen, !sidebar.voice_intent.mute.deafened);
                        }
                        Command::RetryMicrophone => sidebar.voice_intent.retry_mic += 1,
                        Command::RefreshDevices => {
                            if call.is_none() {
                                sidebar.devices = idle_devices();
                            } else {
                                sidebar.voice_intent.refresh_devices += 1;
                            }
                        }
                        Command::ChooseDevice(kind, device) => {
                            let choice = sidebar.voice_intent.preferred.get_mut(kind);
                            let changed = *choice != device;
                            *choice = device;
                            match kind {
                                DeviceKind::Microphone => sidebar.voice_intent.retry_mic += 1,
                                DeviceKind::Speakers => sidebar.voice_intent.retry_speakers += 1,
                            }
                            if let Some(call) = &call {
                                call.apply(&sidebar.voice_intent);
                            }
                            if changed || settings.devices.notice.is_some() {
                                settings.devices.save(&sidebar.voice_intent.preferred).await;
                                sidebar.device_notice = settings.devices.notice;
                            }
                            continue;
                        }
                        _ => unreachable!(),
                    }
                    if let Some(call) = &call {
                        call.apply(&sidebar.voice_intent);
                    }
                }
                _ => {}
            },
            event = socket.recv() => match event {
                Err(_) => break SessionEnd::Closed,
                Ok(SocketEvent::Unauthorized | SocketEvent::Frame(Frame::SignedOut)) => {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                Ok(SocketEvent::Frame(Frame::Banned)) => {
                    break SessionEnd::Invalidated("SPK removed access for this account.");
                }
                Ok(SocketEvent::Online(socket)) => {
                    sidebar.connection = Connection::Online;
                    if let Some(call) = &call {
                        call.set_socket(Some(socket.clone()));
                    }
                    voice_socket = Some(socket);
                    refresh.fetch_all(client);
                }
                Ok(SocketEvent::Connection(state)) => {
                    sidebar.connection = state;
                    voice_socket = None;
                    refresh.presence_ready = false;
                    if let Some(call) = &call {
                        call.set_socket(None);
                    }
                }
                Ok(SocketEvent::Frame(frame)) => match frame {
                    Frame::ChannelsChanged => refresh.fetch(client, Resource::Channels),
                    Frame::UsersChanged => refresh.fetch(client, Resource::Users),
                    Frame::Presence { user_id, channel_id, flags } => {
                        let new_member = channel_id.is_some() && !sidebar.rooms.contains_key(&user_id);
                        if let Some(cue) = refresh.update_presence(&mut sidebar, user_id, channel_id, flags) {
                            settings.sounds.play(cue, cue == Sound::Join);
                        }
                        if new_member {
                            avatars.fill(client, &sidebar);
                        }
                    }
                    Frame::VoiceJoined { sid, channel_id, nonce: Some(nonce) } => {
                        if let Some(call) = &mut call
                            && sidebar.voice.as_ref().is_some_and(|voice| voice.status != VoiceStatus::Leaving)
                        {
                            call.acknowledge(&sid, &channel_id, &nonce);
                        }
                    }
                    Frame::VoiceKicked { sid } if call.as_ref().is_some_and(|call| !call.owns(&sid)) => {
                        sidebar.cancel_voice(&mut call, Some("You joined voice from another tab or device."));
                    }
                    Frame::VoiceDisconnected { sid } if call.as_ref().is_some_and(|call| call.owns(&sid)) => {
                        sidebar.cancel_voice(&mut call, Some(VoiceIssue::Disconnected.message()));
                    }
                    _ => {}
                },
            },
            update = voice::next_update(call.as_ref()) => match update {
                VoiceUpdate::Media(report) => {
                    if let Some(voice) = &mut sidebar.voice
                        && !matches!(voice.status, VoiceStatus::Leaving)
                    {
                        sidebar.devices = report.devices.clone();
                        if voice.update_media(report) {
                            settings.sounds.play(Sound::Join, true);
                        }
                    }
                }
                VoiceUpdate::Finished(result) => {
                    call = None;
                    if matches!(result, Err(VoiceIssue::Unauthorized)) {
                        break SessionEnd::Invalidated(SESSION_EXPIRED);
                    }
                    if sidebar.finish_voice(result) {
                        settings.sounds.play(Sound::Leave, false);
                    }
                }
            },
            Some(result) = avatars.tasks.join_next() => {
                if let Ok(loaded) = result {
                    avatars.complete(&mut sidebar, loaded);
                }
                avatars.fill(client, &sidebar);
            }
            Some(result) = refresh.tasks.join_next() => {
                let Ok((resource, data)) = result else {
                    break SessionEnd::Closed;
                };
                if data.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                let refresh_avatars = data.is_ok() && matches!(resource, Resource::Users | Resource::Presence);
                refresh.complete(&mut sidebar, resource, data);
                if refresh_avatars {
                    if resource == Resource::Users {
                        avatars.directory_changed(&mut sidebar);
                    }
                    avatars.fill(client, &sidebar);
                }
                if let Some(voice) = &sidebar.voice
                    && sidebar.channels.is_some() && !sidebar.has_voice_channel(&voice.channel_id)
                {
                    sidebar.cancel_voice(&mut call, Some("This voice channel is no longer available."));
                }
                if refresh.dirty.remove(&resource) {
                    refresh.fetch(client, resource);
                }
                sidebar.notice = saved.save(client).await.err();
            }
        }
    };
    if let Some(call) = call {
        call.stop().await;
    }
    end
}

fn idle_devices() -> Arc<Devices> {
    Arc::new(
        livekit::PlatformAudio::new()
            .map_or_else(|_| Devices::default(), |audio| Devices::read(&audio)),
    )
}
