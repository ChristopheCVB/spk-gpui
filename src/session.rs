mod authentication;
mod avatars;
mod presence;
mod radio;
mod soundboard;
mod voice_session;

use std::{collections::HashMap, sync::Arc};

use async_channel::{Receiver, Sender};
use gpui_kit::RenderImage;
use tokio::{
    task::JoinSet,
    time::{Instant, sleep_until},
};
use url::Url;

use crate::api::{Channel, Connection, Failure, Frame, Profile, SocketEvent, SpkClient, User};
use crate::sounds::{Sound, SoundSettings, VoiceSounds};
use crate::voice::{
    Call, Device, DeviceKind, Devices, Intent, Issue as VoiceIssue, ParticipantVolumeChange,
    Status as VoiceStatus, Update as VoiceUpdate, screen, video::CameraResolution,
};
use authentication::{
    SESSION_EXPIRED, SavedSession, login, restore, sign_in, sign_in_message, sign_out,
};
use avatars::AvatarLoads;
use presence::{Refresh, Resource, VoiceMembership};
use radio::{RadioSession, RadioView};
use soundboard::{SoundboardSession, SoundboardView};
use voice_session::{DeviceSettings, VoiceDestination, VoiceSession, VoiceSettings};
pub use {presence::Participant, radio::RadioPlayback, voice_session::VoiceView};

#[cfg(test)]
pub(crate) mod tests;

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
    SearchRadio(String),
    RadioFavorites,
    SetRadioFavorite { id: String, saved: bool },
    SelectRadio(String),
    StopRadio,
    PauseRadio,
    ListenToRadio,
    SetRadioVolume(u8),
    LoadSoundboard,
    PlaySound(String),
    PreviewSound(String),
    SetSoundboardEnabled(bool),
    SetSoundboardVolume(u8),
    SetCameraEnabled(bool),
    ChooseCamera(Option<String>),
    ChooseCameraResolution(CameraResolution),
    SetWatching(String, bool),
    OpenScreenShare,
    ChooseScreenSource(u64, String),
    StartScreenShare(u64),
    StopScreenShare(u64),
    SetScreenAudio(bool),
    SetScreenQuality(screen::Quality),
    SetOutputVolume(u8),
    SetParticipantVolume(String, ParticipantVolumeChange),
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
    pub radio: RadioView,
    pub soundboard: SoundboardView,
    pub channel_radio: HashMap<String, String>,
    switching_to: Option<VoiceDestination>,
    pub voice_notice: Option<&'static str>,
    voice_rejoin: Option<String>,
    pub voice_intent: Intent,
    pub devices: Arc<Devices>,
    pub device_notice: Option<&'static str>,
    pub avatars: Arc<HashMap<String, Arc<RenderImage>>>,
    users: Arc<HashMap<String, User>>,
    voice_presence: HashMap<String, VoiceMembership>,
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
            radio: RadioView::default(),
            soundboard: SoundboardView::default(),
            channel_radio: HashMap::new(),
            switching_to: None,
            voice_notice: None,
            voice_rejoin: None,
            voice_intent: Intent::default(),
            devices: Arc::default(),
            device_notice: None,
            avatars: Arc::default(),
            users: Arc::default(),
            voice_presence: HashMap::new(),
        }
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

pub async fn run(
    commands: Receiver<Command>,
    views: Sender<View>,
    sound_settings: tokio::sync::watch::Receiver<SoundSettings>,
) {
    let base = Url::parse(env!("SPK_URL")).expect("validated SPK URL");
    let saved = SavedSession::new(&base);
    let settings = VoiceSettings {
        socket_endpoint: env!("SPK_WS_URL").to_owned(),
        start_call: Box::new(Call::start),
        enumerate_devices: Devices::enumerate,
        devices: DeviceSettings::load(),
        sounds: VoiceSounds::new(sound_settings),
    };
    run_session(base, saved, settings, commands, views).await;
}

async fn run_session(
    base: Url,
    mut saved: SavedSession,
    mut settings: VoiceSettings,
    commands: Receiver<Command>,
    views: Sender<View>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut next = Command::Restore;
    loop {
        let auth = match next {
            Command::Restore => restore(&base, &mut saved).await,
            Command::SignIn { email, password } => sign_in(&base, email, password)
                .await
                .map(Some)
                .map_err(|failure| (false, sign_in_message(failure))),
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

enum SessionEnd {
    Closed,
    SignOut,
    Invalidated(&'static str),
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
    sidebar.devices = Arc::new((settings.enumerate_devices)());
    let mut voice = VoiceSession::default();
    let mut radio = RadioSession::default();
    let mut soundboard = SoundboardSession::default();
    let end = loop {
        if commands.is_closed() {
            break SessionEnd::Closed;
        }
        radio.sync(&mut sidebar, &voice);
        soundboard.sync(&mut sidebar, &voice);
        voice.start_pending(&mut sidebar, client, &settings.start_call);
        if views
            .send(View::SignedIn(Box::new(sidebar.clone())))
            .await
            .is_err()
        {
            break SessionEnd::Closed;
        }
        let retry_at = voice.retry_at();
        tokio::select! {
            _ = sleep_until(retry_at.unwrap_or_else(Instant::now)), if retry_at.is_some() => {
                voice.retry_presence();
                refresh.fetch(client, Resource::Presence);
            }
            command = commands.recv() => match command {
                Err(_) => break SessionEnd::Closed,
                Ok(Command::SignOut) => break SessionEnd::SignOut,
                Ok(Command::Refresh) => refresh.fetch_all(client),
                Ok(Command::JoinVoice(channel_id)) => voice.join(&mut sidebar, channel_id),
                Ok(Command::LeaveVoice) => voice.leave(&mut sidebar),
                Ok(command @ (Command::SearchRadio(_) | Command::RadioFavorites
                    | Command::SetRadioFavorite { .. } | Command::SelectRadio(_) | Command::StopRadio
                    | Command::PauseRadio | Command::ListenToRadio)) => {
                    radio.command(&mut sidebar, &voice, client, command);
                }
                Ok(command @ (Command::LoadSoundboard | Command::PlaySound(_) | Command::PreviewSound(_))) => {
                    soundboard.command(&mut sidebar, &voice, client, command);
                }
                Ok(command) => {
                    voice.apply_controls(&mut sidebar, settings, command).await;
                }
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
                    voice.connected(socket);
                    refresh.fetch_all(client);
                }
                Ok(SocketEvent::Connection(state)) => {
                    sidebar.connection = state;
                    sidebar.channel_radio.clear();
                    refresh.presence_ready = false;
                    voice.disconnected();
                }
                Ok(SocketEvent::Frame(frame)) => match frame {
                    Frame::ChannelsChanged => refresh.fetch(client, Resource::Channels),
                    Frame::UsersChanged => refresh.fetch(client, Resource::Users),
                    Frame::Presence { user_id, channel_id, flags, video } => {
                        let new_participant = channel_id.is_some() && !sidebar.voice_presence.contains_key(&user_id);
                        let own_update = sidebar.profile.id.as_ref() == Some(&user_id);
                        voice.participant_changed(&mut sidebar, &user_id, channel_id.as_deref());
                        for (cue, rising) in refresh.update_presence(&mut sidebar, user_id, channel_id, flags, video) {
                            settings.sounds.play(cue, rising);
                        }
                        if own_update {
                            voice.own_presence_updated(&mut sidebar);
                        }
                        if new_participant {
                            avatars.fill(client, &sidebar);
                        }
                    }
                    Frame::VoiceJoined { sid, channel_id, nonce: Some(nonce) } => {
                        voice.acknowledge(&sidebar, &sid, &channel_id, &nonce);
                    }
                    Frame::VoiceMove { sid, channel_id } if voice.accepts_move(&sidebar, &sid, &channel_id) => {
                        // The token endpoint checks destinations that are missing from a stale channel list.
                        if !sidebar.has_voice_channel(&channel_id) {
                            refresh.fetch(client, Resource::Channels);
                        }
                        voice.move_to(&mut sidebar, channel_id);
                    }
                    Frame::RadioSnapshot { channels } => {
                        sidebar.channel_radio = channels.into_iter()
                            .filter_map(|channel| Some((channel.channel_id, channel.station?.name)))
                            .collect();
                    }
                    Frame::RadioUpdate(channel) => {
                        if let Some(station) = channel.station {
                            sidebar.channel_radio.insert(channel.channel_id, station.name);
                        } else {
                            sidebar.channel_radio.remove(&channel.channel_id);
                        }
                    }
                    Frame::RadioState { sid, channel_id, revision, station }
                        if voice.radio_scope(&sidebar) == Some((sid.as_str(), channel_id.as_str())) => {
                        radio.state(&mut sidebar, revision, station);
                    }
                    Frame::RadioError { sid, channel_id, error }
                        if voice.radio_scope(&sidebar).is_some_and(|(current_sid, current_channel)| {
                            sid == current_sid && channel_id.as_deref().is_none_or(|channel| channel == current_channel)
                        }) => {
                        sidebar.radio.command_error = Some(error);
                    }
                    Frame::VoiceSound { user_id, sound_id } => {
                        soundboard.relay(&sidebar, client, &user_id, &sound_id);
                    }
                    Frame::VoiceKicked { sid } => voice.kicked(&mut sidebar, &sid),
                    Frame::VoiceDisconnected { sid } => voice.server_disconnected(&mut sidebar, &sid),
                    _ => {}
                },
            },
            update = voice.next_update() => match update {
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
                VoiceUpdate::Video(report) => {
                    if let Some(voice) = &mut sidebar.voice
                        && voice.status != VoiceStatus::Leaving
                    {
                        voice.video = Some(report);
                    }
                }
                VoiceUpdate::Screen(report) => {
                    let request = &sidebar.voice_intent.screen;
                    if let Some(voice) = &mut sidebar.voice
                        && voice.is_participating()
                        && report.attempt == request.attempt
                        && request.action != screen::Action::Stop
                    {
                        let was_live = voice.screen.as_ref().is_some_and(|screen| screen.state.active());
                        if was_live != report.state.active() {
                            settings.sounds.play(Sound::Screen, report.state.active());
                        }
                        voice.screen = Some(report);
                    }
                }
                VoiceUpdate::Finished(result) => {
                    voice.finished();
                    if matches!(result, Err(VoiceIssue::Unauthorized)) {
                        break SessionEnd::Invalidated(SESSION_EXPIRED);
                    }
                    if sidebar.finish_voice(result) {
                        settings.sounds.play(Sound::Leave, false);
                    }
                }
            },
            () = RadioSession::playback_changed(&mut radio.player) => {}
            Some(Ok((id, result))) = radio.tasks.join_next_with_id() => {
                if let Err(error) = radio.complete(&mut sidebar.radio, id, result) {
                    if error.unauthorized() {
                        break SessionEnd::Invalidated(SESSION_EXPIRED);
                    }
                    sidebar.radio.directory_error = Some("Could not load or update radio stations. Try again.");
                }
            }
            Some(Ok((id, result))) = soundboard.tasks.join_next_with_id() => {
                if soundboard.complete(&mut sidebar.soundboard, id, result).is_err_and(|error| error.unauthorized()) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
            }
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
                // A request already in flight when the socket returned cannot establish fresh presence.
                let fresh_presence = resource == Resource::Presence && !refresh.dirty.contains(&resource);
                let presence_loaded = data.is_ok();
                let refresh_avatars = data.is_ok() && matches!(resource, Resource::Users | Resource::Presence);
                let listed_voice_channel = matches!(resource, Resource::Channels)
                    && sidebar.voice.as_ref().is_some_and(|voice| sidebar.has_voice_channel(&voice.channel_id));
                refresh.complete(&mut sidebar, resource, data);
                if fresh_presence {
                    voice.presence_refreshed(&mut sidebar, presence_loaded);
                }
                if refresh_avatars {
                    if resource == Resource::Users {
                        avatars.directory_changed(&mut sidebar);
                    }
                    avatars.fill(client, &sidebar);
                }
                if let Some(view) = &sidebar.voice
                    && listed_voice_channel && !sidebar.has_voice_channel(&view.channel_id)
                {
                    voice.channel_removed(&mut sidebar);
                }
                if refresh.dirty.remove(&resource) {
                    refresh.fetch(client, resource);
                }
                sidebar.notice = saved.save(client).await.err();
            }
        }
    };
    drop(soundboard);
    drop(radio);
    voice.stop().await;
    end
}
