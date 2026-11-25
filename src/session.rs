mod account_admin;
mod accounts;
mod authentication;
mod avatars;
mod channel_admin;
mod chat;
mod presence;
mod profiles;
mod radio;
mod search;
mod soundboard;
mod soundboard_admin;
mod unread;
mod voice_session;
mod webhooks;

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use async_channel::{Receiver, Sender};
use gpui_kit::RenderImage;
use tokio::{
    sync::oneshot,
    sync::watch,
    task::JoinSet,
    time::{Instant, sleep_until},
};
use url::Url;

use crate::api::{
    BOT_USER_ID, Channel, Connection, Failure, Frame, MessageChange, MessageTarget, Profile,
    SocketEvent, SpkClient, UnreadState, User, UserPresence, VoiceModeration,
};
use crate::notifications::{Alert, LiveAlerts};
use crate::sounds::{Sound, VoiceSounds};
#[cfg(any(test, windows))]
use crate::voice::screen;
use crate::voice::{
    Call, Device, DeviceKind, Devices, Intent, Issue as VoiceIssue, NoiseSuppression,
    ParticipantVolumeChange, Status as VoiceStatus, Update as VoiceUpdate,
    video::{CameraResolution, preview_camera},
};
use account_admin::{AccountAdmin, AccountAdminSurface, PendingMutation};
use accounts::{AccountLinkOutcome, choose_nickname, redeem_link};
use authentication::{
    SESSION_EXPIRED, SavedSession, login, restore, sign_in, sign_in_message, sign_out,
};
use avatars::AvatarLoads;
use chat::{ChatSession, DraftStore};
use presence::{PresenceReporter, PresenceSettings, Refresh, Resource, VoiceMembership};
use profiles::Work as ProfileWork;
use radio::{RadioSession, RadioView};
use search::{SearchSession, SearchView};
use soundboard::{SoundboardSession, SoundboardView};
use soundboard_admin::{AdminSoundboard, SoundboardAdminView};
use unread::{Mark, UnreadSession};
use voice_session::{DeviceSettings, StartCall, VoiceDestination, VoiceSession};
pub use {
    crate::api::{MessageAction, UserStatus},
    account_admin::{AdminAccountAction, AdminAccountsView, AdminPage},
    accounts::AccountLinkForm,
    avatars::AvatarPhoto,
    channel_admin::{ChannelAction, ChannelAdmin, ChannelKind},
    chat::{
        AttachmentInput, ChatImage, ChatView, Delivery, GifPickerView, Outgoing, POLL_MAX_OPTIONS,
        POLL_MIN_OPTIONS, POLL_OPTION_LIMIT, POLL_QUESTION_LIMIT, Pins, PollEditError,
        PollEditOutcome, PollEditReply, UploadStatus, image_preview, trim_poll_text, validate_poll,
    },
    presence::Participant,
    profiles::ProfileEdit,
    radio::RadioPlayback,
    soundboard_admin::{SoundboardAdminAction, SoundboardCrop},
    voice_session::VoiceView,
    webhooks::{WebhookAction, WebhookError, WebhookReply},
};

#[cfg(test)]
pub(crate) mod tests;

pub enum Command {
    SignIn {
        email: String,
        password: String,
    },
    OpenAccountLink,
    SubmitAccountLink(String),
    RedeemAccountLink {
        email: String,
        password: String,
    },
    BackToSignIn,
    SetInitialNickname(String),
    Restore,
    SignOut,
    SetUserStatus(UserStatus),
    Refresh,
    ManageChannel(ChannelAction),
    MutateWebhook {
        action: WebhookAction,
        reply: WebhookReply,
    },
    OpenTextChannel(String),
    OpenWebhook(String),
    OpenDirectMessage(String),
    StartDirectMessage(String),
    CloseChat,
    LoadOlderMessages,
    LoadNewerMessages,
    RetryChatHistory,
    ReadVisibleChat(String),
    MarkChatRead(String),
    MarkChatUnread {
        channel_id: String,
        message_id: String,
    },
    JumpToChatMessage {
        channel_id: String,
        message_id: String,
    },
    LoadLatestMessages,
    UpdateChatDraft {
        channel_id: String,
        body: String,
    },
    ResolveChatDraftRestoration {
        channel_id: String,
        nonce: String,
        accept: bool,
    },
    SetChatReply {
        channel_id: String,
        message_id: Option<String>,
    },
    SendChatMessage {
        channel_id: String,
        body: String,
    },
    SendChatPoll {
        channel_id: String,
        question: String,
        options: Vec<String>,
        completed: oneshot::Sender<Result<(), &'static str>>,
    },
    ShowGifs(bool),
    SearchGifs(String),
    ShowGifPreviews(Vec<String>),
    ShowGifFavorites(bool),
    MoreGifs,
    RetryGifs,
    RetryGifFavorites,
    ToggleGifFavorite(String),
    PickGif(String),
    RemoveDraftGif(String),
    ReadMessageImage {
        target: crate::api::MessageTarget,
        url: url::Url,
        body: String,
        reply: oneshot::Sender<Option<Arc<ChatImage>>>,
    },
    AddChatAttachments {
        channel_id: String,
        files: Vec<AttachmentInput>,
    },
    CancelChatAttachment {
        channel_id: String,
        key: uuid::Uuid,
    },
    RetryChatAttachment {
        channel_id: String,
        key: uuid::Uuid,
    },
    ToggleChatAttachmentSpoiler {
        channel_id: String,
        key: uuid::Uuid,
    },
    DownloadChatAttachment {
        channel_id: String,
        message_id: String,
        attachment_id: String,
        destination: tempfile::NamedTempFile,
        reply: oneshot::Sender<Result<tempfile::NamedTempFile, Failure>>,
    },
    ReadChatAttachment {
        channel_id: String,
        message_id: String,
        attachment_id: String,
        reply: oneshot::Sender<Result<Vec<u8>, Failure>>,
    },
    SearchMessages {
        query: String,
        scope: crate::api::SearchScope,
    },
    MoreSearchResults,
    OpenMessageTarget(crate::api::MessageTarget),
    OpenConversationTarget(String),
    RetryChatSend(String),
    ResendChatMessage {
        nonce: String,
        body: String,
        completed: oneshot::Sender<Result<(), &'static str>>,
    },
    MarkChatSendDelivered(String),
    MutateChatMessage {
        channel_id: String,
        message_id: String,
        action: MessageAction,
    },
    EditChatPoll {
        channel_id: String,
        message_id: String,
        question: String,
        options: Vec<crate::api::PollEditOption>,
        reset_votes: Option<bool>,
        completed: PollEditReply,
    },
    RetryChatPins(String),
    SaveNickname(String),
    UploadAvatar(PathBuf),
    InspectProfileEmail(String, oneshot::Sender<Option<String>>),
    AdminAccounts(AdminAccountAction),
    ModerateAccount(AdminAccountAction),
    JoinVoice(String),
    LeaveVoice,
    ModerateVoice {
        user_id: String,
        action: VoiceModeration,
    },
    ToggleMute,
    ToggleDeafen,
    RetryMicrophone,
    RefreshDevices,
    ChooseDevice(DeviceKind, Option<Device>),
    SearchRadio(String),
    RadioFavorites,
    SetRadioFavorite {
        id: String,
        saved: bool,
    },
    SelectRadio(String),
    StopRadio,
    PauseRadio,
    ListenToRadio,
    SetRadioVolume(u8),
    OpenSoundboard,
    LoadSoundboard,
    AdminSoundboard(SoundboardAdminAction),
    PlaySound(String),
    PreviewSound(String),
    SetSoundboardEnabled(bool),
    SetSoundboardVolume(u8),
    SetCameraEnabled(bool),
    PreviewCamera(Sender<Result<Arc<RenderImage>, &'static str>>),
    ChooseCamera(Option<String>),
    ChooseCameraResolution(CameraResolution),
    SetWatching(String, bool),
    OpenScreenShare,
    ChooseScreenShareDisplay(u64),
    ChooseScreenSource(u64, String),
    #[cfg(windows)]
    ChooseWindowsScreenSource(
        u64,
        Result<::windows::Graphics::Capture::GraphicsCaptureItem, screen::CaptureError>,
    ),
    StartScreenShare(u64, bool),
    StopScreenShare(u64),
    SetScreenAudio(bool),
    SetScreenHeight(u32),
    SetScreenFps(u32),
    SetOutputVolume(u8),
    SetParticipantVolume(String, ParticipantVolumeChange),
    SetVoiceProcessing(VoiceProcessingSetting),
}

pub enum VoiceProcessingSetting {
    EchoCancellation(bool),
    NoiseSuppression(NoiseSuppression),
    AutoGain(bool),
    Sensitivity(u8),
}

pub enum View {
    Busy(&'static str),
    SignIn {
        message: Option<&'static str>,
        retry_restore: bool,
    },
    AccountLink {
        form: AccountLinkForm,
        message: Option<&'static str>,
    },
    ChooseNickname {
        message: Option<&'static str>,
        notice: Option<&'static str>,
    },
    SignedIn(Box<Sidebar>),
    RetrySignOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    Success(String),
    Info(String),
    Warning(String),
    Error(String),
}

impl ActionResult {
    pub fn success(message: impl Into<String>) -> Self {
        Self::Success(message.into())
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::Info(message.into())
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::Warning(message.into())
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }
}

#[derive(Clone)]
pub struct Sidebar {
    pub profile: Profile,
    pub profile_edit: ProfileEdit,
    pub admin_accounts: AdminAccountsView,
    pub user_status: UserStatus,
    pub channels: Option<Vec<Channel>>,
    pub channel_admin: ChannelAdmin,
    pub connection: Connection,
    pub load_failed: bool,
    pub chat: Option<ChatView>,
    pub search: SearchView,
    pub chat_notice: Option<&'static str>,
    pub draft_notice: Option<&'static str>,
    pub alerts: Vec<Alert>,
    pub action_results: Vec<ActionResult>,
    unread: Arc<HashMap<String, UnreadState>>,
    pub voice: Option<VoiceView>,
    pub radio: RadioView,
    pub soundboard: SoundboardView,
    pub admin_soundboard: SoundboardAdminView,
    pub channel_radio: HashMap<String, String>,
    switching_to: Option<VoiceDestination>,
    pub voice_notice: Option<&'static str>,
    pub voice_moderation_pending: bool,
    voice_rejoin: Option<String>,
    pub voice_intent: Intent,
    pub devices: Arc<Devices>,
    pub device_notice: Option<&'static str>,
    pub avatars: Arc<HashMap<String, Arc<AvatarPhoto>>>,
    users: Arc<HashMap<String, User>>,
    user_order: Arc<[String]>,
    voice_presence: HashMap<String, VoiceMembership>,
    user_presence: Arc<HashMap<String, UserPresence>>,
}

impl Sidebar {
    fn new(profile: Profile, session_save_error: Option<&'static str>) -> Self {
        Self {
            profile,
            profile_edit: ProfileEdit::Idle,
            admin_accounts: AdminAccountsView::default(),
            user_status: UserStatus::default(),
            channels: None,
            channel_admin: ChannelAdmin::default(),
            connection: Connection::Connecting,
            load_failed: false,
            chat: None,
            search: SearchView::default(),
            chat_notice: None,
            draft_notice: None,
            alerts: Vec::new(),
            action_results: session_save_error
                .into_iter()
                .map(ActionResult::error)
                .collect(),
            unread: Arc::default(),
            voice: None,
            radio: RadioView::default(),
            soundboard: SoundboardView::default(),
            admin_soundboard: SoundboardAdminView::default(),
            channel_radio: HashMap::new(),
            switching_to: None,
            voice_notice: None,
            voice_moderation_pending: false,
            voice_rejoin: None,
            voice_intent: Intent::default(),
            devices: Arc::default(),
            device_notice: None,
            avatars: Arc::default(),
            users: Arc::default(),
            user_order: Arc::default(),
            voice_presence: HashMap::new(),
            user_presence: Arc::default(),
        }
    }

    pub fn webhook_feeds(&self) -> impl Iterator<Item = &Channel> {
        self.channels
            .iter()
            .flatten()
            .filter(|channel| channel.is_webhook())
    }

    pub fn webhook_feed(&self, id: &str) -> Option<&Channel> {
        self.webhook_feeds().find(|channel| channel.id == id)
    }

    pub fn unread(&self, channel_id: &str) -> UnreadState {
        let mut state = self.unread.get(channel_id).copied().unwrap_or_default();
        if self.webhook_feed(channel_id).is_some() {
            state.mentions = 0;
        }
        state
    }

    pub fn user_presence(&self, id: &str) -> UserPresence {
        self.user_presence.get(id).copied().unwrap_or_default()
    }

    pub fn people(&self) -> Vec<(&str, &str)> {
        if self.profile.id.is_none() {
            return Vec::new();
        }
        let mut people = self
            .users
            .values()
            .filter(|user| !user.banned && self.can_start_direct_message(&user.id))
            .filter_map(|user| Some((user.id.as_str(), user.name()?)))
            .collect::<Vec<_>>();
        let activity: HashMap<_, _> = self
            .channels
            .iter()
            .flatten()
            .filter_map(|channel| {
                Some((channel.other_user_id.as_deref()?, channel.last_activity_at?))
            })
            .collect();
        people.sort_by_cached_key(|(id, name)| {
            (
                std::cmp::Reverse(activity.get(id).copied()),
                name.to_lowercase(),
                *id,
            )
        });
        people
    }

    pub fn mention_directory(&self) -> impl Iterator<Item = (&str, &str)> {
        self.user_order.iter().filter_map(|id| {
            let user = self.users.get(id)?;
            Some((id.as_str(), user.name()?))
        })
    }

    pub fn mention_name(&self, id: &str) -> Option<&str> {
        self.users.get(id).and_then(User::name)
    }

    pub fn same_user_directory(&self, previous: &Self) -> bool {
        Arc::ptr_eq(&self.users, &previous.users)
    }

    pub fn can_start_direct_message(&self, user_id: &str) -> bool {
        !user_id.is_empty() && user_id != BOT_USER_ID && self.profile.id.as_deref() != Some(user_id)
    }

    pub fn direct_message(&self, user_id: &str) -> Option<&Channel> {
        self.channels.iter().flatten().find(|channel| {
            channel.kind == "dm"
                && channel.is_conversation()
                && channel.other_user_id.as_deref() == Some(user_id)
        })
    }

    pub fn message_author(&self, id: &str) -> &str {
        if self.profile.id.as_deref() == Some(id) {
            return self.profile.name();
        }
        self.users
            .get(id)
            .and_then(User::name)
            .unwrap_or("Unknown user")
    }

    pub fn person(&self, id: &str) -> Option<&User> {
        self.users.get(id).filter(|user| !user.banned)
    }

    pub fn inspect_person(&self, id: &str) -> Option<&User> {
        self.users
            .get(id)
            .filter(|user| self.is_admin() || !user.banned)
            .or_else(|| {
                if !self.is_admin() {
                    return None;
                }
                self.admin_accounts
                    .users
                    .as_ref()?
                    .users
                    .iter()
                    .find(|entry| entry.user.id == id)
                    .map(|entry| &entry.user)
            })
    }

    pub fn is_admin(&self) -> bool {
        self.profile.role == "admin"
    }

    pub fn avatar(&self, id: Option<&str>) -> Option<&Arc<AvatarPhoto>> {
        self.avatars.get(id?)
    }

    fn avatar_key(&self, id: &str) -> Option<&str> {
        if self.profile.id.as_deref() == Some(id) {
            self.profile.avatar_key.as_deref()
        } else {
            self.users
                .get(id)
                .or_else(|| self.inspect_person(id))?
                .avatar_key
                .as_deref()
        }
        .filter(|key| !key.is_empty())
    }
}

struct SessionSettings {
    socket_endpoint: String,
    drafts_path: Option<PathBuf>,
    start_call: Box<StartCall>,
    enumerate_devices: fn() -> Devices,
    devices: DeviceSettings,
    sounds: VoiceSounds,
    presence: PresenceSettings,
    window_active: watch::Receiver<bool>,
    alert_activation: watch::Receiver<Option<MessageTarget>>,
}

pub async fn run(
    commands: Receiver<Command>,
    views: Sender<View>,
    sounds: VoiceSounds,
    activity: watch::Receiver<bool>,
    window_active: watch::Receiver<bool>,
    alert_activation: watch::Receiver<Option<MessageTarget>>,
) {
    let base = Url::parse(env!("SPK_URL")).expect("validated SPK URL");
    let saved = SavedSession::new(&base);
    let settings = SessionSettings {
        socket_endpoint: env!("SPK_WS_URL").to_owned(),
        drafts_path: dirs::data_local_dir().map(|dir| dir.join("spk-gpui").join("drafts.json")),
        start_call: Box::new(Call::start),
        enumerate_devices: Devices::enumerate,
        devices: DeviceSettings::load(),
        sounds,
        presence: PresenceSettings::load(activity),
        window_active,
        alert_activation,
    };
    run_session(base, saved, settings, commands, views).await;
}

async fn run_session(
    base: Url,
    mut saved: SavedSession,
    mut settings: SessionSettings,
    commands: Receiver<Command>,
    views: Sender<View>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut next = Command::Restore;
    let mut sign_in_prompt = None;
    loop {
        let auth = match next {
            Command::Restore => restore(&base, &mut saved).await,
            Command::SignIn { email, password } => sign_in(&base, email, password)
                .await
                .map(Some)
                .map_err(|failure| (false, sign_in_message(failure))),
            Command::OpenAccountLink => match redeem_link(&base, &commands, &views).await {
                None => return,
                Some(AccountLinkOutcome::Cancelled) => sign_in_prompt.map_or(Ok(None), Err),
                Some(AccountLinkOutcome::PasswordChanged) => Err((
                    false,
                    "Password changed. You were signed out everywhere. Sign in with the new password.",
                )),
                Some(AccountLinkOutcome::SignedUp(client)) => match client.profile().await {
                    Ok(profile) => Ok(Some((client, profile))),
                    Err(_) => {
                        let saved_session = saved.save(&client).await.is_ok();
                        Err((
                            saved_session,
                            "Account created. Sign in to finish setting up your profile.",
                        ))
                    }
                },
            },
            _ => return,
        };

        let (message, retry_restore) = match auth {
            Ok(Some((client, profile))) => {
                let notice = saved.save(&client).await.err();
                let end = match choose_nickname(&client, profile, notice, &commands, &views).await {
                    Ok(profile) => {
                        watch_session(
                            &client,
                            profile,
                            notice,
                            &mut saved,
                            &mut settings,
                            &commands,
                            &views,
                        )
                        .await
                    }
                    Err(end) => end,
                };
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
        sign_in_prompt = message.map(|message| (retry_restore, message));
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

enum ChatNavigation {
    AwaitingChannel {
        channel: String,
        message: Option<String>,
        from_alert: bool,
    },
    OpeningAlert,
}

impl ChatNavigation {
    fn is_opening_alert(navigation: &Option<Self>) -> bool {
        matches!(navigation, Some(Self::OpeningAlert))
    }

    fn open_chat(
        navigation: &mut Option<Self>,
        chat: &mut ChatSession,
        client: &SpkClient,
        channel: &Channel,
    ) {
        // Reselecting the pending conversation keeps its access check in place.
        if chat.open(client, channel) || !Self::is_opening_alert(navigation) {
            *navigation = None;
        }
    }
}

async fn watch_session(
    client: &SpkClient,
    profile: Profile,
    notice: Option<&'static str>,
    saved: &mut SavedSession,
    settings: &mut SessionSettings,
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
    let mut session_save_error = notice;
    sidebar.user_status = settings.presence.status;
    let mut presence = PresenceReporter::default();
    let mut avatars = AvatarLoads::default();
    avatars.fill(client, &sidebar);
    sidebar.voice_intent.preferred = settings.devices.preferred.clone();
    sidebar.device_notice = settings.devices.notice;
    sidebar.devices = Arc::new((settings.enumerate_devices)());
    let draft_store = settings
        .drafts_path
        .clone()
        .zip(sidebar.profile.id.clone())
        .map(|(path, account)| DraftStore::new(path, client.origin().to_owned(), account));
    let mut chat = if let Some(store) = &draft_store {
        match store.load().await {
            Ok(bodies) => ChatSession::with_drafts(bodies),
            Err(_) => {
                sidebar.draft_notice = Some("Could not load saved Drafts.");
                ChatSession::default()
            }
        }
    } else {
        ChatSession::default()
    };
    let mut unread = UnreadSession::default();
    unread.refresh(client);
    let mut direct_message_opens = JoinSet::new();
    let mut voice = VoiceSession::default();
    let mut radio = RadioSession::default();
    let mut search = SearchSession::default();
    let mut chat_navigation = None;
    let mut alerts = LiveAlerts::default();
    let mut soundboard = SoundboardSession::default();
    let mut admin_soundboard = AdminSoundboard::default();
    let mut camera_previews = JoinSet::new();
    let mut voice_moderation = JoinSet::new();
    let mut channel_mutations = JoinSet::new();
    let mut webhook_mutations = JoinSet::<webhooks::WebhookCompletion>::new();
    let mut channel_mutation_result = None;
    let mut profiles = JoinSet::new();
    let mut account_admin = AccountAdmin::default();
    let mut draft_save_at = None;
    let mut draft_save_failed = false;
    let end = loop {
        if commands.is_closed() {
            break SessionEnd::Closed;
        }
        if draft_store.is_some() && chat.drafts_to_save() {
            draft_save_at.get_or_insert_with(|| Instant::now() + Duration::from_millis(250));
        }
        unread.write_next(client);
        chat.fill_previews(client);
        chat.gifs.fill_previews(client);
        chat.fill_staged_gif_preview(client);
        sidebar.chat = chat.view();
        search.retain_conversation(
            &mut sidebar.search,
            sidebar
                .chat
                .as_ref()
                .filter(|chat| !chat.unavailable())
                .map(|chat| chat.channel_id.as_str()),
        );
        sidebar.voice_moderation_pending = !voice_moderation.is_empty();
        radio.sync(&mut sidebar, &voice);
        soundboard.sync(&mut sidebar, &voice);
        admin_soundboard.sync(&mut sidebar);
        voice.start_pending(&mut sidebar, client, &settings.start_call);
        let action_results = std::mem::take(&mut sidebar.action_results);
        let mut next = sidebar.clone();
        if ChatNavigation::is_opening_alert(&chat_navigation) {
            // A retained alert can point to a conversation the account can no longer access.
            if next.chat.as_ref().is_some_and(ChatView::loading) {
                next.chat = None;
                next.chat_notice = Some("Opening message…");
            } else {
                chat_navigation = None;
            }
        }
        next.action_results = action_results;
        next.alerts = std::mem::take(&mut alerts.ready);
        if views.send(View::SignedIn(Box::new(next))).await.is_err() {
            break SessionEnd::Closed;
        }
        let retry_at = voice.retry_at();
        let rejoin_at = voice.rejoin_at();
        let chat_timeout = chat.next_timeout();
        let soundboard_transition = soundboard.next_transition;
        let active = *settings.presence.activity.borrow_and_update();
        let alert_activation = &mut settings.alert_activation;
        let mut from_alert = false;
        tokio::select! {
            () = presence.report(sidebar.user_status, active) => {}
            Ok(()) = settings.presence.activity.changed() => {}
            _ = sleep_until(chat_timeout.unwrap_or_else(Instant::now)), if chat_timeout.is_some() => chat.expire(),
            _ = sleep_until(soundboard_transition.unwrap_or_else(Instant::now)), if soundboard_transition.is_some() => {},
            _ = sleep_until(draft_save_at.unwrap_or_else(Instant::now)), if draft_save_at.is_some() => {
                draft_save_at = None;
                if let Some(store) = &draft_store {
                    chat.save_drafts(store);
                }
            }
            Some(result) = chat.draft_writes.join_next() => {
                let saved = chat.draft_save_completed(result);
                match saved {
                    Ok(()) => sidebar.draft_notice = None,
                    Err(message) if !draft_save_failed => {
                        sidebar.action_results.push(ActionResult::error(message));
                    }
                    Err(_) => {}
                }
                draft_save_failed = saved.is_err();
            }
            Some(result) = unread.writes.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                unread.complete_write(&mut sidebar, &mut chat, result);
            }
            Some(result) = unread.refreshes.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                unread.complete_refresh(&mut sidebar, result);
            }
            Some(result) = chat.tasks.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                if let Ok(page) = &result {
                    alerts.remember_history(&page.messages);
                }
                // SPK checks conversation access before reporting that the target message is missing.
                let unverified_alert = ChatNavigation::is_opening_alert(&chat_navigation)
                    && result.as_ref().is_err_and(|error| {
                        !matches!(error, Failure::Http(reqwest::StatusCode::NOT_FOUND))
                    });
                chat.complete(client, sidebar.profile.id.as_deref(), result, &mut sidebar.action_results);
                if unverified_alert {
                    chat.close();
                    sidebar.chat_notice = None;
                }
            }
            Some(result) = chat.action_tasks.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                sidebar.action_results.extend(chat.action_complete(client, result));
            }
            Some(Ok((channel_id, message_id, result))) = chat.vote_tasks.join_next() => {
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                sidebar.action_results.extend(chat.vote_complete(client, &channel_id, &message_id, result));
            }
            Some(Ok(result)) = chat.preview_tasks.join_next() => chat.preview_complete(result),
            Some(result) = chat.gifs.browsing.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                chat.gifs.browse_complete(result);
            }
            Some(result) = chat.gifs.favorites.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                sidebar.action_results.extend(chat.gifs.favorites_complete(client, result));
            }
            Some(Ok(result)) = chat.gifs.previews.join_next() => chat.gifs.preview_complete(result),
            Some(Ok(result)) = chat.staged_gif_preview.join_next() => chat.staged_gif_preview_complete(result),
            Some(_) = chat.message_preview.join_next() => {},
            Some(result) = chat.pin_tasks.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                chat.pins_complete(result);
            }
            Some(result) = chat.attachments.tasks.join_next() => {
                if matches!(result, Ok(true)) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
            }
            Ok(event) = chat.attachments.events.recv() => chat.attachment_event(client, event),
            Some(Ok((result, reply, operation))) = webhook_mutations.join_next() => {
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                let result = webhooks::MutationContext {
                    sidebar: &mut sidebar,
                    chat: &mut chat,
                    navigation: &mut chat_navigation,
                    direct_message_opens: &mut direct_message_opens,
                    refresh: &mut refresh,
                    unread: &mut unread,
                    search: &mut search,
                }.complete(client, result, operation);
                if let Err(WebhookError::Action(message)) = result {
                    sidebar.action_results.push(ActionResult::error(message));
                }
                let _ = reply.send(result);
            }
            Some(result) = direct_message_opens.join_next() => {
                match result.unwrap_or(Err(Failure::Connection)) {
                    Ok(channel) => {
                        sidebar.chat_notice = None;
                        ChatNavigation::open_chat(&mut chat_navigation, &mut chat, client, &channel);
                        let channels = sidebar.channels.get_or_insert_default();
                        if !channels.iter().any(|listed| listed.id == channel.id) {
                            channels.push(channel);
                        }
                        refresh.fetch(client, Resource::Channels);
                    }
                    Err(error) if error.unauthorized() => break SessionEnd::Invalidated(SESSION_EXPIRED),
                    Err(_) => {
                        sidebar.chat_notice = None;
                        sidebar.action_results.push(ActionResult::error("Could not open the direct message. Try again."));
                    }
                }
            }
            _ = sleep_until(rejoin_at.unwrap_or_else(Instant::now)), if rejoin_at.is_some() => {}
            _ = sleep_until(retry_at.unwrap_or_else(Instant::now)), if retry_at.is_some() => {
                voice.retry_presence();
                refresh.fetch(client, Resource::Presence);
            }
            command = async {
                tokio::select! {
                    // Keep launch and signed-out activations pending until this authenticated loop.
                    Ok(()) = alert_activation.changed() => {
                        from_alert = true;
                        Ok(Command::OpenMessageTarget(alert_activation.borrow_and_update().clone()
                            .expect("the desktop only publishes message targets")))
                    }
                    command = commands.recv() => command,
                }
            } => match command {
                Err(_) => break SessionEnd::Closed,
                Ok(Command::SignOut) => break SessionEnd::SignOut,
                Ok(Command::SetUserStatus(status)) => {
                    sidebar.user_status = status;
                    if let Err(message) = settings.presence.save(status).await {
                        sidebar.action_results.push(ActionResult::error(message));
                    }
                }
                Ok(Command::Refresh) => {
                    refresh.fetch_all(client);
                    unread.refresh(client);
                    if matches!(sidebar.connection, Connection::Online) {
                        refresh.fetch(client, Resource::UserPresence);
                    }
                }
                Ok(Command::ManageChannel(action)) => {
                    if !sidebar.channel_admin.pending {
                        sidebar.channel_admin.revision += 1;
                        sidebar.channel_admin.succeeded = false;
                        sidebar.channel_admin.validation = None;
                        match sidebar.channel_mutation(action) {
                            Ok((mutation, notice)) => {
                                sidebar.channel_admin.pending = true;
                                let client = client.clone();
                                channel_mutations.spawn(async move {
                                    client.mutate_channel(mutation).await.map(|()| notice)
                                });
                            }
                            Err(channel_admin::ChannelMutationError::Validation(message)) => {
                                sidebar.channel_admin.validation = Some(message);
                            }
                            Err(channel_admin::ChannelMutationError::Action(message)) => {
                                sidebar.action_results.push(ActionResult::error(message));
                            }
                        }
                    }
                }
                Ok(Command::MutateWebhook { action, reply }) => {
                    if !webhook_mutations.is_empty() {
                        let message = "A webhook change is already in progress.";
                        sidebar.action_results.push(ActionResult::error(message));
                        let _ = reply.send(Err(WebhookError::Action(message)));
                    } else {
                        match action.validated(&sidebar) {
                            Ok(action) => {
                                let client = client.clone();
                                webhook_mutations.spawn(async move { action.send(&client, reply).await });
                            }
                            Err(error) => {
                                if let WebhookError::Action(message) = error {
                                    sidebar.action_results.push(ActionResult::error(message));
                                }
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                }
                Ok(Command::OpenTextChannel(id)) => {
                    if let Some(channel) = sidebar.channels.iter().flatten().find(|channel| channel.id == id && channel.kind == "text") {
                        direct_message_opens = JoinSet::new();
                        sidebar.chat_notice = None;
                        ChatNavigation::open_chat(&mut chat_navigation, &mut chat, client, channel);
                    }
                }
                Ok(Command::OpenWebhook(id)) => {
                    if let Some(channel) = sidebar.webhook_feed(&id) {
                        direct_message_opens = JoinSet::new();
                        ChatNavigation::open_chat(&mut chat_navigation, &mut chat, client, channel);
                        sidebar.chat_notice = None;
                    } else {
                        sidebar.action_results.push(ActionResult::error("Webhook feed unavailable."));
                    }
                }
                Ok(Command::OpenDirectMessage(id)) => {
                    direct_message_opens = JoinSet::new();
                    if let Some(channel) = sidebar.channels.iter().flatten().find(|channel| channel.id == id && channel.kind == "dm" && channel.is_conversation()) {
                        sidebar.chat_notice = None;
                        ChatNavigation::open_chat(&mut chat_navigation, &mut chat, client, channel);
                    } else {
                        sidebar.chat_notice = None;
                        sidebar.action_results.push(ActionResult::error("Direct message unavailable."));
                    }
                }
                Ok(Command::StartDirectMessage(user_id)) => {
                    if !ChatNavigation::is_opening_alert(&chat_navigation) {
                        chat_navigation = None;
                    }
                    direct_message_opens = JoinSet::new();
                    if !sidebar.can_start_direct_message(&user_id) {
                        sidebar.chat_notice = None;
                        sidebar.action_results.push(ActionResult::error("Direct message unavailable."));
                    } else if let Some(channel) = sidebar.direct_message(&user_id) {
                        ChatNavigation::open_chat(&mut chat_navigation, &mut chat, client, channel);
                        sidebar.chat_notice = None;
                    } else {
                        sidebar.chat_notice = Some("Opening direct message…");
                        let client = client.clone();
                        direct_message_opens.spawn(async move { client.open_direct_message(&user_id).await });
                    }
                }
                Ok(Command::CloseChat) => {
                    chat_navigation = None;
                    direct_message_opens = JoinSet::new();
                    sidebar.chat_notice = None;
                    chat.close();
                }
                Ok(Command::LoadOlderMessages) => chat.older(client),
                Ok(Command::LoadNewerMessages) => chat.newer(client),
                Ok(Command::RetryChatHistory) => chat.retry_history(client),
                Ok(Command::ReadVisibleChat(channel_id)) => {
                    if matches!(sidebar.connection, Connection::Online) && chat.should_read(&channel_id) {
                        unread.mark(channel_id, Mark::Visible);
                    }
                }
                Ok(Command::MarkChatRead(channel_id)) => {
                    if sidebar.channels.iter().flatten().any(|channel| {
                        channel.id == channel_id && channel.is_conversation()
                    }) {
                        unread.mark(channel_id, Mark::Read);
                    }
                }
                Ok(Command::MarkChatUnread { channel_id, message_id }) => {
                    if let Some(request) = chat.start_mark_unread(&channel_id, &message_id) {
                        unread.mark(channel_id, Mark::Unread(message_id, request));
                    }
                },
                Ok(Command::JumpToChatMessage { .. } | Command::LoadLatestMessages)
                    if ChatNavigation::is_opening_alert(&chat_navigation) => {}
                Ok(Command::JumpToChatMessage { channel_id, message_id }) => {
                    chat_navigation = None;
                    sidebar.chat_notice = None;
                    chat.jump(client, &channel_id, message_id);
                }
                Ok(Command::LoadLatestMessages) => {
                    chat_navigation = None;
                    chat.latest(client);
                }
                Ok(Command::UpdateChatDraft { channel_id, body }) => chat.update_draft(channel_id, body),
                Ok(Command::ResolveChatDraftRestoration { channel_id, nonce, accept }) => {
                    chat.resolve_draft_restoration(&channel_id, &nonce, accept);
                }
                Ok(Command::SetChatReply { channel_id, message_id }) => chat.set_reply(&channel_id, message_id.as_deref()),
                Ok(Command::SendChatMessage { channel_id, body }) => chat.send(channel_id, body),
                Ok(Command::SendChatPoll { channel_id, question, options, completed }) => {
                    let _ = completed.send(chat.send_poll(channel_id, &question, &options));
                }
                Ok(Command::ShowGifs(true)) => chat.open_gifs(client),
                Ok(Command::ShowGifs(false)) => chat.gifs.close(),
                Ok(Command::SearchGifs(query)) => chat.gifs.search(client, query),
                Ok(Command::ShowGifPreviews(urls)) => chat.gifs.show_previews(urls),
                Ok(Command::ShowGifFavorites(show)) => chat.gifs.show_favorites(client, show),
                Ok(Command::MoreGifs) => chat.gifs.more(client),
                Ok(Command::RetryGifs) => chat.gifs.retry_browse(client),
                Ok(Command::RetryGifFavorites) => chat.gifs.load_favorites(client),
                Ok(Command::ToggleGifFavorite(id)) => chat.gifs.toggle_favorite(client, &id),
                Ok(Command::PickGif(id)) => chat.set_gif(Some(&id), None),
                Ok(Command::RemoveDraftGif(channel)) => chat.set_gif(None, Some(&channel)),
                Ok(Command::ReadMessageImage { target, url, body, reply }) => {
                    chat.read_message_image(client, &target, url, &body, reply);
                }
                Ok(Command::AddChatAttachments { channel_id, files }) => chat.add_attachments(client, channel_id, files),
                Ok(Command::CancelChatAttachment { channel_id, key }) => chat.cancel_attachment(client, &channel_id, key),
                Ok(Command::RetryChatAttachment { channel_id, key }) => chat.retry_attachment(client, &channel_id, key),
                Ok(Command::ToggleChatAttachmentSpoiler { channel_id, key }) => chat.toggle_attachment_spoiler(&channel_id, key),
                Ok(Command::DownloadChatAttachment { channel_id, message_id, attachment_id, destination, reply }) => {
                    chat.download_attachment(client, &channel_id, &message_id, attachment_id, destination, reply);
                }
                Ok(Command::ReadChatAttachment { channel_id, message_id, attachment_id, reply }) => {
                    chat.read_attachment(client, &channel_id, &message_id, attachment_id, reply);
                }
                Ok(Command::SearchMessages { query, scope }) => search.start(&mut sidebar.search, client, query, scope),
                Ok(Command::MoreSearchResults) => search.more(&mut sidebar.search, client),
                Ok(command @ (Command::OpenMessageTarget(_) | Command::OpenConversationTarget(_))) => {
                    let (channel, message) = match command {
                        Command::OpenMessageTarget(target) => (
                            target.channel_id().to_owned(),
                            Some(target.message_id().to_owned()),
                        ),
                        Command::OpenConversationTarget(id) => (id, None),
                        _ => unreachable!(),
                    };
                    direct_message_opens = JoinSet::new();
                    if ChatNavigation::is_opening_alert(&chat_navigation) {
                        chat.close();
                    }
                    if search::open_chat_target(&mut sidebar, &mut chat, client, &channel, message.as_deref()) {
                        chat_navigation = from_alert.then_some(ChatNavigation::OpeningAlert);
                    } else {
                        sidebar.chat_notice = Some(if message.is_some() {
                            "Opening message…"
                        } else {
                            "Opening conversation…"
                        });
                        chat_navigation = Some(ChatNavigation::AwaitingChannel { channel, message, from_alert });
                        refresh.fetch(client, Resource::Channels);
                    }
                }
                Ok(Command::RetryChatSend(nonce)) => {
                    let _ = chat.retry_send(&nonce, None);
                }
                Ok(Command::ResendChatMessage { nonce, body, completed }) => {
                    let _ = completed.send(chat.retry_send(&nonce, Some(body)));
                }
                Ok(Command::MarkChatSendDelivered(nonce)) => chat.mark_send_delivered(&nonce),
                Ok(Command::MutateChatMessage { channel_id, message_id, action }) => {
                    chat.mutate(client, &mut sidebar, &channel_id, message_id, action);
                }
                Ok(Command::EditChatPoll { channel_id, message_id, question, options, reset_votes, completed }) => {
                    let result = chat.prepare_poll_edit(
                        &sidebar, &channel_id, &message_id, &question, options, reset_votes,
                    );
                    match result {
                        Ok(Some(action)) => chat.start_message_action(client, &channel_id, message_id, action, Some(completed)),
                        Ok(None) => { let _ = completed.send(Ok(PollEditOutcome::ConfirmVoteChange)); }
                        Err(error) => {
                            if let PollEditError::Action(message) = error {
                                sidebar.action_results.push(ActionResult::error(message));
                            }
                            let _ = completed.send(Err(error));
                        }
                    }
                }
                Ok(Command::RetryChatPins(channel_id)) => chat.retry_pins(client, &channel_id),
                Ok(command @ (Command::SaveNickname(_) | Command::UploadAvatar(_) | Command::InspectProfileEmail(..))) => {
                    ProfileWork::start(&mut profiles, &mut sidebar, client, command);
                }
                Ok(Command::AdminAccounts(action)) => {
                    account_admin.command(&mut sidebar, client, AccountAdminSurface::Settings, action);
                }
                Ok(Command::ModerateAccount(action)) => {
                    account_admin.command(&mut sidebar, client, AccountAdminSurface::Workspace, action);
                }
                Ok(Command::JoinVoice(channel_id)) => voice.join(&mut sidebar, channel_id),
                Ok(Command::LeaveVoice) => voice.leave(&mut sidebar),
                Ok(Command::ModerateVoice { user_id, action }) if voice_moderation.is_empty() => {
                    sidebar.moderate_voice(client, &mut voice_moderation, user_id, action);
                }
                Ok(Command::ModerateVoice { .. }) => {}
                Ok(command @ (Command::SearchRadio(_) | Command::RadioFavorites
                    | Command::SetRadioFavorite { .. } | Command::SelectRadio(_) | Command::StopRadio
                    | Command::PauseRadio | Command::ListenToRadio)) => {
                    radio.command(&mut sidebar, &voice, client, command);
                }
                Ok(command @ (Command::OpenSoundboard | Command::LoadSoundboard | Command::PlaySound(_) | Command::PreviewSound(_))) => {
                    soundboard.command(&mut sidebar, &voice, client, command);
                }
                Ok(Command::AdminSoundboard(action)) => admin_soundboard.command(&mut sidebar, client, action),
                Ok(Command::PreviewCamera(frames)) => {
                    if !sidebar.camera_publishing() {
                        camera_previews.shutdown().await;
                        camera_previews.spawn(preview_camera(sidebar.voice_intent.camera.clone(), frames));
                    }
                }
                Ok(command) => {
                    if matches!(command, Command::SetCameraEnabled(true)) {
                        camera_previews.shutdown().await;
                    }
                    voice.apply_controls(&mut sidebar, settings, command).await;
                }
            },
            Some(_) = camera_previews.join_next() => {}
            Some(result) = channel_mutations.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                let notice = result.as_ref().copied().map_err(channel_admin::failure_notice);
                match result {
                    Ok(_) | Err(Failure::Http(reqwest::StatusCode::CONFLICT | reqwest::StatusCode::NOT_FOUND)) => {
                        channel_mutation_result = Some(notice);
                        refresh.fetch(client, Resource::Channels);
                    }
                    _ => {
                        sidebar.channel_admin.pending = false;
                        if let Err(message) = notice {
                            sidebar.action_results.push(ActionResult::error(message));
                        }
                    }
                }
            }
            Some(result) = voice_moderation.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                sidebar.action_results.push(match result {
                    Ok(VoiceModeration::Move(_)) => ActionResult::info("Move requested."),
                    Ok(VoiceModeration::Disconnect) => ActionResult::success("Voice participant disconnected."),
                    Err(error) => ActionResult::error(match error {
                        Failure::Http(reqwest::StatusCode::FORBIDDEN) => "You don’t have permission to moderate voice.",
                        Failure::Http(reqwest::StatusCode::CONFLICT) => "That participant is no longer in the expected voice channel.",
                        Failure::Http(reqwest::StatusCode::NOT_FOUND) => "That voice channel is unavailable.",
                        _ => "Could not moderate voice. Try again.",
                    }),
                });
            }
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
                    chat.connected(client, socket.clone());
                    unread.refresh(client);
                    presence.connected(socket.clone());
                    voice.connected(socket);
                    refresh.fetch_all(client);
                    refresh.fetch(client, Resource::UserPresence);
                    account_admin.refresh(&mut sidebar, client);
                }
                Ok(SocketEvent::Connection(state)) => {
                    sidebar.connection = state;
                    sidebar.channel_radio.clear();
                    refresh.presence_ready = false;
                    chat.disconnected();
                    alerts.disconnected();
                    unread.disconnected();
                    presence = PresenceReporter::default();
                    voice.disconnected();
                }
                Ok(SocketEvent::Frame(frame)) => match frame {
                    Frame::MessageNew { message, nonce } => {
                        if sidebar.channels.iter().flatten().find(|channel| channel.id == message.channel_id)
                            .is_none_or(|channel| channel.kind == "dm" && channel.last_activity_at.is_none_or(|at| at < message.created_at))
                        {
                            refresh.fetch(client, Resource::Channels);
                        }
                        alerts.receive(&sidebar, &message);
                        chat.message(
                            *message,
                            nonce.as_deref(),
                            sidebar.profile.id.as_deref(),
                            *settings.window_active.borrow(),
                        );
                    }
                    Frame::UnreadUpdate { channel_id, state } => {
                        unread.update(&mut sidebar, channel_id, state);
                    }
                    Frame::MessageDelete { channel_id, id } => {
                        chat.message_changed(client, &channel_id, &id, MessageChange::Delete);
                    }
                    Frame::MessageEdit { message } => {
                        let (channel_id, id) = (message.channel_id.clone(), message.id.clone());
                        chat.message_changed(client, &channel_id, &id, MessageChange::Edit(message));
                    }
                    Frame::MessagePreviews { channel_id, message_id, previews } => {
                        chat.message_changed(client, &channel_id, &message_id, MessageChange::Previews(previews));
                    }
                    Frame::ReactionUpdate { channel_id, message_id, reactions } => {
                        chat.message_changed(client, &channel_id, &message_id, MessageChange::Reactions(reactions));
                    }
                    Frame::PollUpdate { channel_id, message_id, poll } => {
                        chat.message_changed(client, &channel_id, &message_id, MessageChange::Poll(poll));
                    }
                    Frame::PinUpdate { channel_id, message_id, pinned } => {
                        chat.message_changed(client, &channel_id, &message_id, MessageChange::Pin(pinned));
                    }
                    Frame::MessageError { nonce, error } => chat.error(nonce.as_deref(), error),
                    Frame::Typing { channel_id, user_id } if sidebar.profile.id.as_ref() != Some(&user_id) => {
                        chat.receive_typing(&channel_id, user_id);
                    }
                    Frame::ChannelsChanged => refresh.fetch(client, Resource::Channels),
                    Frame::UsersChanged => {
                        refresh.fetch(client, Resource::Users);
                        account_admin.users_changed(&mut sidebar, client);
                    }
                    Frame::UserPresence { user_id, status } => {
                        refresh.update_user_presence(&mut sidebar, user_id, status);
                    }
                    Frame::Presence { user_id, channel_id, flags, video } => {
                        let new_participant = channel_id.is_some() && !sidebar.voice_presence.contains_key(&user_id);
                        let own_update = sidebar.profile.id.as_ref() == Some(&user_id);
                        for (cue, rising) in refresh.update_presence(&mut sidebar, user_id, channel_id, flags, video) {
                            settings.sounds.play(cue, rising);
                        }
                        voice.prune_watching(&mut sidebar);
                        voice.sync_remote_audio_presence(&mut sidebar);
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
                        sidebar.action_results.push(ActionResult::error(error));
                    }
                    Frame::SoundNew => sidebar.soundboard.has_new_clip = true,
                    Frame::VoiceSound { user_id, sound_id } => {
                        soundboard.relay(&mut sidebar, client, user_id, sound_id);
                    }
                    Frame::VoiceKicked { sid } => voice.kicked(&mut sidebar, &sid),
                    Frame::VoiceDisconnected { sid } => voice.server_disconnected(&mut sidebar, &sid),
                    _ => {}
                },
            },
            update = voice.next_update() => match update {
                VoiceUpdate::Media(report) => {
                    if let Some(view) = &mut sidebar.voice
                        && view.status != VoiceStatus::Leaving
                    {
                        sidebar.devices = report.devices.clone();
                        if view.update_media(report) && voice.media_connected() {
                            settings.sounds.play(Sound::Join, true);
                        }
                    }
                }
                VoiceUpdate::Video(report) => {
                    if let Some(call) = &mut sidebar.voice
                        && call.status != VoiceStatus::Leaving
                    {
                        call.video = Some(report);
                        voice.prune_watching(&mut sidebar);
                    }
                }
                VoiceUpdate::VideoParticipantLeft(id) => {
                    voice.video_participant_left(&mut sidebar, &id);
                }
                VoiceUpdate::RemoteAudioStalled => voice.recover_stalled_audio(&mut sidebar),
                VoiceUpdate::Screen(report) => {
                    voice.screen_report(&mut sidebar, settings, report);
                }
                VoiceUpdate::Finished(result) => {
                    if matches!(result, Err(VoiceIssue::Unauthorized)) {
                        break SessionEnd::Invalidated(SESSION_EXPIRED);
                    }
                    if voice.finished(&mut sidebar, result) {
                        settings.sounds.play(Sound::Leave, false);
                    }
                }
            },
            Some(result) = search.tasks.join_next() => {
                let result = result.unwrap_or(Err(Failure::Connection));
                if result.as_ref().is_err_and(Failure::unauthorized) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
                search.complete(&mut sidebar, &mut refresh, client, result);
            }
            () = RadioSession::playback_changed(&mut radio.player) => {}
            Some(Ok((id, result))) = radio.tasks.join_next_with_id() => {
                if let Err(error) = radio.complete(&mut sidebar, id, result) {
                    if error.unauthorized() {
                        break SessionEnd::Invalidated(SESSION_EXPIRED);
                    }
                    sidebar.radio.directory_error = Some("Could not load radio stations. Try again.");
                }
            }
            Some(Ok((id, result))) = soundboard.tasks.join_next_with_id() => {
                if soundboard.complete(&mut sidebar, client, id, result).is_err_and(|error| error.unauthorized()) {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
            }
            Some(Ok(result)) = admin_soundboard.tasks.join_next() => {
                if admin_soundboard.complete(&mut sidebar, result).is_err() {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
            }
            Some(Ok((kind, result))) = admin_soundboard.mutations.join_next() => {
                match admin_soundboard.mutated(&mut sidebar, client, kind, result) {
                    Ok(true) if sidebar.soundboard.clips.is_some() || sidebar.soundboard.loading => {
                        soundboard.refresh(&mut sidebar.soundboard, client);
                    }
                    Ok(_) => {}
                    Err(_) => break SessionEnd::Invalidated(SESSION_EXPIRED),
                }
            }
            Some(result) = account_admin.loads.join_next() => {
                if account_admin.loaded(&mut sidebar, client, result.unwrap_or(Err(Failure::Connection))).is_err() {
                    break SessionEnd::Invalidated(SESSION_EXPIRED);
                }
            }
            (surface, result) = PendingMutation::join(&mut account_admin.mutation) => {
                match account_admin.mutated(&mut sidebar, client, surface, result) {
                    Ok(true) => refresh.fetch(client, Resource::Users),
                    Ok(false) => {}
                    Err(_) => break SessionEnd::Invalidated(SESSION_EXPIRED),
                }
            }
            Some(Ok(work)) = profiles.join_next() => {
                let edit_finished = match work.complete(&mut sidebar) {
                    Ok(edited) => edited,
                    Err(_) => break SessionEnd::Invalidated(SESSION_EXPIRED),
                };
                if edit_finished {
                    refresh.fetch(client, Resource::Users);
                    avatars.directory_changed(&mut sidebar);
                    avatars.fill(client, &sidebar);
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
                if refresh.complete(&mut sidebar, resource, data) {
                    unread.refresh(client);
                }
                if resource == Resource::Channels && presence_loaded {
                    webhooks::reconcile_access(&mut sidebar, &mut chat, &mut chat_navigation);
                    search.reconcile_channels(&mut sidebar);
                }
                if resource == Resource::Channels && !refresh.dirty.contains(&resource) {
                    search.channels_refreshed(&mut sidebar, presence_loaded);
                }
                account_admin.clear_if_demoted(&mut sidebar);
                if resource == Resource::Channels
                    && !refresh.dirty.contains(&resource)
                    && let Some(result) = channel_mutation_result.take()
                {
                    sidebar.channel_admin.pending = false;
                    sidebar.channel_admin.succeeded = result.is_ok();
                    sidebar.action_results.push(match (result, presence_loaded) {
                        (Ok(message), true) => ActionResult::success(message),
                        (Ok(_), false) => ActionResult::warning("Channel updated. Refresh to reload the channel list."),
                        (Err(message), _) => ActionResult::error(message),
                    });
                }
                if resource == Resource::Channels
                    && presence_loaded
                    && let Some(id) = chat.selected_text_channel()
                    && !sidebar.channels_of_kind(ChannelKind::Text).any(|channel| channel.id == id)
                {
                    chat.close();
                    if !matches!(chat_navigation, Some(ChatNavigation::AwaitingChannel { .. })) {
                        sidebar.chat_notice = Some("That channel no longer exists.");
                    }
                }
                if resource == Resource::Channels
                    && !refresh.dirty.contains(&resource)
                    && let Some(ChatNavigation::AwaitingChannel { channel, message, from_alert }) = &chat_navigation
                {
                    if presence_loaded && search::open_chat_target(&mut sidebar, &mut chat, client, channel, message.as_deref()) {
                        chat_navigation = from_alert.then_some(ChatNavigation::OpeningAlert);
                    } else {
                        sidebar.chat_notice = None;
                        sidebar.action_results.push(ActionResult::error(if message.is_some() {
                            "Message unavailable."
                        } else {
                            "Conversation unavailable."
                        }));
                        chat_navigation = None;
                    }
                }
                if resource == Resource::Channels && !refresh.dirty.contains(&resource) && presence_loaded {
                    alerts.resolve_unlisted(&sidebar);
                }
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
                let error = saved.save(client).await.err();
                if let Some(message) = error.filter(|_| error != session_save_error) {
                    sidebar.action_results.push(ActionResult::error(message));
                }
                session_save_error = error;
            }
        }
    };
    voice.leave(&mut sidebar);
    if let Some(store) = &draft_store {
        if matches!(end, SessionEnd::Closed) {
            while let Ok(command) = commands.try_recv() {
                match command {
                    Command::UpdateChatDraft { channel_id, body } => {
                        chat.update_draft(channel_id, body)
                    }
                    Command::ResolveChatDraftRestoration {
                        channel_id,
                        nonce,
                        accept,
                    } => {
                        chat.resolve_draft_restoration(&channel_id, &nonce, accept);
                    }
                    _ => {}
                }
            }
        }
        if let Some(result) = chat.draft_writes.join_next().await {
            let _ = chat.draft_save_completed(result);
        }
        if chat.drafts_to_save() {
            chat.save_drafts(store);
            if let Some(result) = chat.draft_writes.join_next().await {
                let _ = chat.draft_save_completed(result);
            }
        }
    }
    if matches!(end, SessionEnd::Invalidated(SESSION_EXPIRED))
        && matches!(
            chat_navigation,
            Some(
                ChatNavigation::OpeningAlert
                    | ChatNavigation::AwaitingChannel {
                        from_alert: true,
                        ..
                    }
            )
        )
    {
        settings.alert_activation.mark_changed();
    }
    drop(webhook_mutations);
    drop(chat);
    camera_previews.shutdown().await;
    drop(soundboard);
    drop(admin_soundboard);
    drop(radio);
    voice.stop().await;
    end
}
