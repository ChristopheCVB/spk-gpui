use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use tokio::{
    task::{JoinSet, spawn_blocking},
    time::Instant,
};

use super::{ActionResult, Command, Sidebar, VoiceProcessingSetting};
use crate::api::{Channel, Connection, Failure, SpkClient, VoiceModeration, VoiceSocket};
use crate::sounds::Sound;
use crate::voice::{
    self, Call, DeviceKind, DevicePreferences, Intent, Issue as VoiceIssue, MediaOutput,
    ParticipantVolumeChange, Report as VoiceReport, Status as VoiceStatus, Update as VoiceUpdate,
    screen::{self, Action, State},
    video::{CameraStatus, VideoReport},
};

pub(super) const VOICE_REPLACED: &str = "You joined voice from another tab or device.";
const CHANNEL_UNAVAILABLE: &str = "This voice channel is no longer available.";
const REJOIN_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(8),
];

#[derive(Clone)]
pub(super) struct VoiceDestination {
    channel_id: String,
    server_move: bool,
}

#[derive(Clone)]
pub struct VoiceView {
    pub channel_id: String,
    pub status: VoiceStatus,
    pub media: Option<Arc<VoiceReport>>,
    pub video: Option<Arc<VideoReport>>,
    pub screen: Option<screen::Report>,
    pub(super) play_leave: bool,
}

impl VoiceView {
    pub(super) fn joining(channel_id: String) -> Self {
        Self {
            channel_id,
            status: VoiceStatus::Joining,
            media: None,
            video: None,
            screen: None,
            play_leave: false,
        }
    }

    pub fn is_participating(&self) -> bool {
        matches!(
            self.status,
            VoiceStatus::Connected | VoiceStatus::ListenOnly | VoiceStatus::Reconnecting
        )
    }

    pub(super) fn begin_leave(&mut self, explicit: bool) -> bool {
        if self.status == VoiceStatus::Leaving {
            return false;
        }
        self.play_leave = explicit && self.is_participating();
        self.status = VoiceStatus::Leaving;
        self.media = None;
        self.video = None;
        self.screen = None;
        true
    }

    pub(super) fn update_media(&mut self, report: Arc<VoiceReport>) -> bool {
        if self.status == VoiceStatus::Leaving {
            return false;
        }
        let was_listening = self.is_participating();
        self.status = report.status;
        self.media = Some(report);
        !was_listening && self.is_participating()
    }
}

impl Sidebar {
    pub(crate) fn echo_reference_unavailable(&self) -> bool {
        self.voice
            .as_ref()
            .and_then(|voice| voice.media.as_ref())
            .is_some_and(|report| report.echo_reference_unavailable)
    }

    pub(crate) fn media_output(&self) -> MediaOutput {
        let preferred = &self.voice_intent.preferred;
        MediaOutput {
            device: self
                .devices
                .media_outputs
                .resolve(preferred.media_output.as_ref())
                .cloned(),
            volume: preferred.output_gain(),
        }
    }

    pub fn camera_status(&self) -> CameraStatus {
        let camera = &self.voice_intent.camera;
        if !camera.enabled {
            return CameraStatus::Off;
        }
        self.voice
            .as_ref()
            .and_then(|voice| voice.video.as_ref())
            .filter(|report| report.camera_revision == camera.revision)
            .map_or(CameraStatus::Starting, |report| report.camera)
    }

    pub fn camera_publishing(&self) -> bool {
        matches!(
            self.camera_status(),
            CameraStatus::Starting | CameraStatus::Live(..)
        )
    }

    pub fn voice_moderation_channels(&self, user_id: &str) -> Option<Vec<&Channel>> {
        if !self.is_admin() || !matches!(self.connection, Connection::Online) {
            return None;
        }
        let member = self.voice_presence.get(user_id)?;
        Some(
            self.channels
                .iter()
                .flatten()
                .filter(|channel| channel.kind == "voice" && channel.id != member.channel_id)
                .collect(),
        )
    }

    pub(super) fn moderate_voice(
        &mut self,
        client: &SpkClient,
        tasks: &mut JoinSet<Result<VoiceModeration, Failure>>,
        user_id: String,
        action: VoiceModeration,
    ) {
        if !self.is_admin() {
            self.action_results.push(ActionResult::error(
                "You don’t have permission to moderate voice.",
            ));
            return;
        }
        let available = self
            .voice_moderation_channels(&user_id)
            .is_some_and(|destinations| match &action {
                VoiceModeration::Move(id) => destinations.iter().any(|channel| &channel.id == id),
                VoiceModeration::Disconnect => true,
            });
        if !available {
            self.action_results.push(ActionResult::error(
                "This voice moderation action is unavailable.",
            ));
            return;
        }
        let client = client.clone();
        tasks.spawn(async move {
            client.moderate_voice(&user_id, &action).await?;
            Ok(action)
        });
    }

    pub(super) fn has_voice_channel(&self, channel_id: &str) -> bool {
        self.channels
            .iter()
            .flatten()
            .any(|channel| channel.kind == "voice" && channel.id == channel_id)
    }

    pub fn switching_to(&self) -> Option<&str> {
        self.switching_to
            .as_ref()
            .map(|destination| destination.channel_id.as_str())
    }

    pub fn can_join_voice(&self, channel_id: &str) -> bool {
        matches!(self.connection, Connection::Online)
            && self.has_voice_channel(channel_id)
            && self.can_switch_voice(channel_id)
    }

    fn can_switch_voice(&self, channel_id: &str) -> bool {
        self.switching_to() != Some(channel_id)
            && self
                .voice
                .as_ref()
                .is_none_or(|voice| voice.channel_id != channel_id)
    }

    fn own_voice_presence(&self) -> Option<&super::VoiceMembership> {
        self.voice_presence.get(self.profile.id.as_ref()?)
    }

    fn remote_microphone_mutes(&self, channel_id: &str) -> HashMap<String, bool> {
        self.voice_presence
            .iter()
            .filter(|(id, member)| {
                member.channel_id == channel_id && self.profile.id.as_ref() != Some(id)
            })
            .map(|(id, member)| (id.clone(), member.flags.muted || member.flags.deafened))
            .collect()
    }

    fn voice_replaced(&self) -> bool {
        self.own_voice_presence()
            .zip(self.voice.as_ref())
            .is_some_and(|(member, voice)| member.channel_id != voice.channel_id)
    }

    pub(super) fn finish_voice(&mut self, result: Result<(), VoiceIssue>) -> bool {
        let voice = self.voice.take();
        self.clear_voice_media();
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

    fn clear_voice_media(&mut self) {
        self.voice_intent.camera.enabled = false;
        self.voice_intent.watching.clear();
        self.voice_intent.remote_microphone_mutes.clear();
        self.voice_intent.screen.action = Action::Stop;
    }

    pub fn rejoin_channel(&self) -> Option<&str> {
        let id = self.voice_rejoin.as_deref()?;
        self.can_join_voice(id).then_some(id)
    }
}

pub(super) struct DeviceSettings {
    pub(super) path: Option<PathBuf>,
    pub(super) preferred: DevicePreferences,
    pub(super) notice: Option<&'static str>,
    pub(super) save_failed: bool,
}

impl DeviceSettings {
    pub(super) fn load() -> Self {
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
            save_failed: false,
        }
    }

    async fn save(&mut self, sidebar: &mut Sidebar) {
        self.preferred = sidebar.voice_intent.preferred.clone();
        let path = self.path.clone();
        let preferred = self.preferred.clone();
        let saved = spawn_blocking(move || path.is_some_and(|path| preferred.save(&path).is_ok()))
            .await
            .unwrap_or(false);
        self.save_failed = !saved;
        if saved {
            self.notice = None;
            sidebar.device_notice = None;
        } else {
            sidebar.action_results.push(ActionResult::error(
                "Could not save audio settings. Your choices will only last until you close SPK.",
            ));
        }
    }
}

pub(super) type StartCall = dyn Fn(String, SpkClient, VoiceSocket, Intent) -> Call + Send + Sync;

#[derive(Default)]
pub(super) struct VoiceSession {
    socket: Option<VoiceSocket>,
    call: Option<Call>,
    reclaim: Option<VoiceReclaim>,
    rejoin: Option<VoiceRejoin>,
    watchdog_rejoin_after: Option<Instant>,
}

struct VoiceRejoin {
    ended_sid: String,
    attempted: usize,
    deadline: Instant,
    departed: bool,
}

enum VoiceReclaim {
    WaitingForPresence,
    RetryAt(Instant),
    Retried,
}

impl VoiceSession {
    pub(super) fn start_pending(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        start_call: &StartCall,
    ) {
        if self.call.is_some() {
            return;
        }
        let destination = sidebar.switching_to.take().or_else(|| {
            self.rejoin_at()
                .filter(|&deadline| deadline <= Instant::now())?;
            Some(VoiceDestination {
                channel_id: sidebar.voice.as_ref()?.channel_id.clone(),
                server_move: false,
            })
        });
        let Some(VoiceDestination {
            channel_id,
            server_move,
        }) = destination
        else {
            return;
        };
        if let Some(socket) = &self.socket
            && (server_move || sidebar.has_voice_channel(&channel_id))
        {
            sidebar.voice_intent.remote_microphone_mutes =
                sidebar.remote_microphone_mutes(&channel_id);
            sidebar.voice_intent.watchdog_rejoin_after = self.watchdog_rejoin_after;
            if let Some(rejoin) = &mut self.rejoin {
                rejoin.attempted += 1;
            }
            self.call = Some(start_call(
                channel_id.clone(),
                client.clone(),
                socket.clone(),
                sidebar.voice_intent.clone(),
            ));
            sidebar.voice = Some(VoiceView::joining(channel_id));
            sidebar.voice_notice = None;
            sidebar.voice_rejoin = None;
        } else {
            self.cancel(
                sidebar,
                Some(if self.socket.is_none() {
                    VoiceIssue::Socket.message()
                } else {
                    CHANNEL_UNAVAILABLE
                }),
            );
        }
    }

    pub(super) fn connected(&mut self, socket: VoiceSocket) {
        self.reclaim = (self.call.is_some() || self.rejoin.is_some())
            .then_some(VoiceReclaim::WaitingForPresence);
        self.socket = Some(socket);
        self.release_ended_presence();
    }

    fn release_ended_presence(&self) {
        if let (Some(rejoin), Some(socket)) = (&self.rejoin, &self.socket)
            && !rejoin.departed
        {
            socket.release_voice(&rejoin.ended_sid);
        }
    }

    pub(super) fn disconnected(&mut self) {
        self.socket = None;
        self.reclaim = None;
        if let Some(call) = &self.call {
            call.set_socket(None);
        }
    }

    pub(super) fn active_call_sid<'a>(&'a self, sidebar: &Sidebar) -> Option<&'a str> {
        sidebar
            .voice
            .as_ref()
            .filter(|voice| voice.is_participating())?;
        self.call.as_ref().map(Call::sid)
    }

    pub(super) fn radio_scope<'a>(&'a self, sidebar: &'a Sidebar) -> Option<(&'a str, &'a str)> {
        let voice = sidebar.voice.as_ref()?;
        (self.socket.is_some() && voice.status != VoiceStatus::Leaving)
            .then_some((self.call.as_ref()?.sid(), voice.channel_id.as_str()))
    }

    pub(super) fn active_socket(&self, sidebar: &Sidebar) -> Option<(VoiceSocket, String)> {
        let (sid, _) = self.radio_scope(sidebar)?;
        sidebar.voice.as_ref()?.is_participating().then(|| {
            (
                self.socket
                    .as_ref()
                    .expect("scope has a voice socket")
                    .clone(),
                sid.to_owned(),
            )
        })
    }

    pub(super) fn set_radio_listening(&self, sidebar: &mut Sidebar, listening: bool) {
        let current = &mut sidebar.voice_intent.radio_listening;
        if listening != current.unwrap_or(false) {
            *current = Some(listening);
            if let Some(call) = &self.call {
                call.apply(&sidebar.voice_intent);
            }
        }
    }

    pub(super) fn retry_at(&self) -> Option<Instant> {
        match self.reclaim {
            Some(VoiceReclaim::RetryAt(deadline)) => Some(deadline),
            _ => None,
        }
    }

    pub(super) fn rejoin_at(&self) -> Option<Instant> {
        let rejoin = self.rejoin.as_ref()?;
        (self.call.is_none() && self.socket.is_some() && self.reclaim.is_none())
            .then_some(rejoin.deadline)
    }

    pub(super) fn retry_presence(&mut self) {
        self.reclaim = Some(VoiceReclaim::Retried);
    }

    pub(super) fn own_presence_updated(&mut self, sidebar: &mut Sidebar) {
        let retry_replaced = self.rejoin.as_mut().is_some_and(|rejoin| {
            if sidebar.own_voice_presence().is_none() {
                rejoin.departed = true;
                return false;
            }
            // An own Presence after departure belongs to another client until SPK acknowledges the retry.
            rejoin.departed && self.call.as_ref().is_none_or(Call::awaiting_acknowledgment)
        });
        if retry_replaced
            || ((self.reclaim.is_some() || self.rejoin.is_some()) && sidebar.voice_replaced())
            || (sidebar.voice_rejoin.is_some() && sidebar.own_voice_presence().is_some())
        {
            self.reclaim = None;
            self.cancel(sidebar, Some(VOICE_REPLACED));
        }
    }

    pub(super) fn presence_refreshed(&mut self, sidebar: &mut Sidebar, loaded: bool) {
        if loaded {
            self.prune_watching(sidebar);
            self.sync_remote_audio_presence(sidebar);
        }
        if self.reclaim.is_some() && loaded {
            let first_check = matches!(self.reclaim.take(), Some(VoiceReclaim::WaitingForPresence));
            if sidebar.voice_replaced() {
                self.cancel(sidebar, Some(VOICE_REPLACED));
            } else if let Some(rejoin) = &mut self.rejoin
                && (rejoin.attempted == 0
                    || self.call.as_ref().is_none_or(Call::awaiting_acknowledgment))
            {
                if sidebar.own_voice_presence().is_none() {
                    rejoin.departed = true;
                } else if first_check && !rejoin.departed {
                    // A snapshot can race the ended session's Leave on the new socket.
                    self.reclaim = Some(VoiceReclaim::RetryAt(
                        Instant::now() + Duration::from_secs(1),
                    ));
                } else {
                    self.cancel(sidebar, Some(VOICE_REPLACED));
                }
            } else if let Some(call) = &self.call {
                call.set_socket(self.socket.clone());
            }
        } else if matches!(self.reclaim, Some(VoiceReclaim::WaitingForPresence))
            || (self.rejoin.is_some() && self.reclaim.is_some())
        {
            self.reclaim = Some(VoiceReclaim::RetryAt(
                Instant::now() + Duration::from_secs(1),
            ));
        }
    }

    pub(super) fn join(&mut self, sidebar: &mut Sidebar, channel_id: String) {
        let retrying = self.rejoin.is_some() && sidebar.has_voice_channel(&channel_id);
        if sidebar.can_join_voice(&channel_id) || (retrying && self.socket.is_some()) {
            self.request_voice(sidebar, channel_id, false);
        } else if self.socket.is_none() {
            if retrying {
                self.cancel(sidebar, None);
            }
            sidebar.voice_notice = Some(VoiceIssue::Socket.message());
        } else if !sidebar.has_voice_channel(&channel_id) {
            sidebar.voice_notice = Some(CHANNEL_UNAVAILABLE);
        }
    }

    pub(super) fn leave(&mut self, sidebar: &mut Sidebar) {
        sidebar.switching_to = None;
        self.cancel(sidebar, None);
    }

    pub(super) fn accepts_move(&self, sidebar: &Sidebar, sid: &str, channel_id: &str) -> bool {
        !channel_id.is_empty()
            && self.owns(sid)
            && sidebar.can_switch_voice(channel_id)
            && sidebar
                .voice
                .as_ref()
                .is_some_and(|voice| voice.status != VoiceStatus::Leaving)
    }

    pub(super) fn move_to(&mut self, sidebar: &mut Sidebar, channel_id: String) {
        self.request_voice(sidebar, channel_id, true);
    }

    pub(super) fn acknowledge(
        &mut self,
        sidebar: &Sidebar,
        sid: &str,
        channel_id: &str,
        nonce: &str,
    ) {
        if let Some(call) = &mut self.call
            && sidebar
                .voice
                .as_ref()
                .is_some_and(|voice| voice.status != VoiceStatus::Leaving)
        {
            call.acknowledge(sid, channel_id, nonce);
        }
    }

    pub(super) fn kicked(&mut self, sidebar: &mut Sidebar, sid: &str) {
        if (self.call.is_some() || self.rejoin.is_some() || sidebar.voice_rejoin.is_some())
            && !self.owns(sid)
        {
            self.cancel(sidebar, Some(VOICE_REPLACED));
        }
    }

    pub(super) fn server_disconnected(&mut self, sidebar: &mut Sidebar, sid: &str) {
        if self.owns(sid) {
            self.cancel(sidebar, Some(VoiceIssue::Disconnected.message()));
        }
    }

    fn owns(&self, sid: &str) -> bool {
        self.call.as_ref().is_some_and(|call| call.owns(sid))
            || self
                .rejoin
                .as_ref()
                .is_some_and(|rejoin| rejoin.ended_sid == sid)
    }

    pub(super) fn channel_removed(&mut self, sidebar: &mut Sidebar) {
        self.cancel(sidebar, Some(CHANNEL_UNAVAILABLE));
    }

    fn request_voice(&mut self, sidebar: &mut Sidebar, channel_id: String, server_move: bool) {
        sidebar.switching_to = Some(VoiceDestination {
            channel_id,
            server_move,
        });
        self.cancel(sidebar, None);
    }

    fn cancel(&mut self, sidebar: &mut Sidebar, notice: Option<&'static str>) {
        sidebar.voice_rejoin = None;
        let retrying = self.rejoin.take().is_some();
        if let (Some(call), Some(voice)) = (&mut self.call, &mut sidebar.voice) {
            if voice.begin_leave(notice.is_none()) {
                call.cancel();
                sidebar.clear_voice_media();
                sidebar.voice_notice = notice;
            }
        } else if retrying {
            self.reclaim = None;
            sidebar.voice = None;
            sidebar.voice_notice = notice;
        } else if notice.is_some() {
            sidebar.voice_notice = notice;
        }
    }

    pub(super) async fn next_update(&self) -> VoiceUpdate {
        loop {
            let update = voice::next_update(self.call.as_ref()).await;
            if matches!(update, VoiceUpdate::Finished(_))
                || self
                    .rejoin
                    .as_ref()
                    .is_none_or(|rejoin| rejoin.attempted > 0)
            {
                return update;
            }
        }
    }

    pub(super) fn recover_stalled_audio(&mut self, sidebar: &mut Sidebar) {
        let now = Instant::now();
        if self
            .watchdog_rejoin_after
            .is_none_or(|deadline| now >= deadline)
            && sidebar.voice.as_ref().is_some_and(|voice| {
                matches!(
                    voice.status,
                    VoiceStatus::Connected | VoiceStatus::ListenOnly
                )
            })
            && self.rejoin.is_none()
        {
            self.recover(sidebar);
            if self.rejoin.is_some() {
                self.watchdog_rejoin_after = Some(now + Duration::from_secs(60));
            }
        }
    }

    pub(super) fn recover(&mut self, sidebar: &mut Sidebar) {
        let (Some(call), Some(view)) = (&mut self.call, &sidebar.voice) else {
            return;
        };
        if self.rejoin.is_none() && view.is_participating() {
            self.rejoin = Some(VoiceRejoin {
                ended_sid: call.sid().to_owned(),
                attempted: 0,
                deadline: Instant::now(),
                departed: sidebar.own_voice_presence().is_none(),
            });
            call.cancel();
            sidebar.voice = Some(VoiceView::joining(view.channel_id.clone()));
            sidebar.clear_voice_media();
            if self.reclaim.is_some() {
                self.release_ended_presence();
            }
        }
    }

    pub(super) fn media_connected(&mut self) -> bool {
        self.rejoin.take().is_none()
    }

    pub(super) fn finished(
        &mut self,
        sidebar: &mut Sidebar,
        result: Result<(), VoiceIssue>,
    ) -> bool {
        if matches!(result, Err(VoiceIssue::LiveKitDisconnected)) {
            self.recover(sidebar);
        }
        let ended = self.call.take();
        let Some(rejoin) = &mut self.rejoin else {
            self.reclaim = None;
            return sidebar.finish_voice(result);
        };
        if let Some(ended) = ended {
            rejoin.ended_sid = ended.sid().to_owned();
        }
        if matches!(result, Err(VoiceIssue::Socket)) && self.socket.is_none() {
            rejoin.attempted = rejoin.attempted.saturating_sub(1);
        }
        if let Some(delay) = REJOIN_DELAYS.get(rejoin.attempted)
            && !matches!(
                result,
                Err(VoiceIssue::TokenRejected | VoiceIssue::Endpoint | VoiceIssue::Disconnected)
            )
        {
            rejoin.deadline = Instant::now() + *delay;
            let channel_id = sidebar
                .voice
                .as_ref()
                .expect("voice rejoin has a destination")
                .channel_id
                .clone();
            sidebar.voice = Some(VoiceView::joining(channel_id));
            sidebar.clear_voice_media();
            sidebar.voice_notice = None;
            false
        } else {
            self.rejoin = None;
            self.reclaim = None;
            let channel_id = sidebar.voice.as_ref().map(|view| view.channel_id.clone());
            sidebar.finish_voice(result);
            if !matches!(result, Err(VoiceIssue::Disconnected)) {
                sidebar.voice_rejoin = channel_id;
            }
            false
        }
    }

    pub(super) async fn stop(self) {
        if let Some(call) = self.call {
            call.stop().await;
        }
    }

    pub(super) fn video_participant_left(&self, sidebar: &mut Sidebar, id: &str) {
        if let Some(report) = sidebar
            .voice
            .as_mut()
            .and_then(|voice| voice.video.as_mut())
        {
            Arc::make_mut(report).participants.remove(id);
        }
        if sidebar.voice_intent.watching.remove(id)
            && let Some(call) = &self.call
        {
            call.apply(&sidebar.voice_intent);
        }
    }

    pub(super) fn sync_remote_audio_presence(&self, sidebar: &mut Sidebar) {
        if let Some(call) = &self.call
            && let Some(voice) = &sidebar.voice
            && voice.status != VoiceStatus::Leaving
        {
            let mutes = sidebar.remote_microphone_mutes(&voice.channel_id);
            if mutes != sidebar.voice_intent.remote_microphone_mutes {
                sidebar.voice_intent.remote_microphone_mutes = mutes;
                call.apply(&sidebar.voice_intent);
            }
        }
    }

    pub(super) fn prune_watching(&self, sidebar: &mut Sidebar) {
        let Some(voice) = &sidebar.voice else {
            return;
        };
        let watching = &mut sidebar.voice_intent.watching;
        let count = watching.len();
        watching.retain(|id| {
            let Some(member) = sidebar.voice_presence.get(id) else {
                return false;
            };
            member.channel_id == voice.channel_id
                && (voice
                    .video
                    .as_ref()
                    .is_some_and(|report| report.reconnecting)
                    || member.video_feeds(id, Some(voice)).is_live())
        });
        if watching.len() != count
            && let Some(call) = &self.call
        {
            call.apply(&sidebar.voice_intent);
        }
    }

    pub(super) fn screen_report(
        &self,
        sidebar: &mut Sidebar,
        settings: &super::SessionSettings,
        report: screen::Report,
    ) {
        let request = &mut sidebar.voice_intent.screen;
        let Some(voice) = sidebar
            .voice
            .as_mut()
            .filter(|voice| voice.is_participating())
        else {
            return;
        };
        if report.attempt != request.attempt || request.action == Action::Stop {
            return;
        }
        if matches!(report.state, State::Cancelled) {
            request.advance(report.attempt, Action::Stop);
            voice.screen = None;
            return;
        }
        let was_live = voice
            .screen
            .as_ref()
            .is_some_and(|screen| screen.state.active());
        if was_live != report.state.active() {
            settings.sounds.play(Sound::Screen, report.state.active());
        }
        if let State::Live { quality, quality_failed: Some(failed), .. } = &report.state
            && request.quality == *failed
            && !voice.screen.as_ref().is_some_and(|previous| {
                matches!(previous.state, State::Live { quality_failed: Some(value), .. } if value == *failed)
            })
        {
            request.quality = *quality;
            if let Some(call) = &self.call {
                call.apply(&sidebar.voice_intent);
            }
        }
        voice.screen = Some(report);
    }

    pub(super) async fn apply_controls(
        &self,
        sidebar: &mut Sidebar,
        settings: &mut super::SessionSettings,
        command: Command,
    ) {
        let save_preferences = matches!(
            &command,
            Command::SetOutputVolume(_)
                | Command::SetParticipantVolume(..)
                | Command::SetSoundboardEnabled(_)
                | Command::SetSoundboardVolume(_)
                | Command::SetScreenAudio(_)
        );
        match command {
            Command::ToggleMute => {
                sidebar.voice_intent.mute.toggle_mic();
                settings
                    .sounds
                    .play(Sound::Mute, !sidebar.voice_intent.mute.effective_muted());
            }
            Command::ToggleDeafen => {
                sidebar.voice_intent.mute.toggle_deafen();
                settings
                    .sounds
                    .play(Sound::Deafen, !sidebar.voice_intent.mute.deafened);
            }
            Command::RetryMicrophone => sidebar.voice_intent.retry_mic += 1,
            Command::RefreshDevices => {
                if self.call.is_none() {
                    sidebar.devices = Arc::new((settings.enumerate_devices)());
                } else {
                    sidebar.voice_intent.refresh_devices += 1;
                }
            }
            Command::SetVoiceProcessing(setting) => {
                let processing = &mut sidebar.voice_intent.preferred.processing;
                match setting {
                    VoiceProcessingSetting::EchoCancellation(enabled) => {
                        processing.echo_cancellation = enabled;
                    }
                    VoiceProcessingSetting::NoiseSuppression(mode) => {
                        processing.noise_suppression = mode;
                    }
                    VoiceProcessingSetting::AutoGain(enabled) => processing.auto_gain = enabled,
                    VoiceProcessingSetting::Sensitivity(value) => {
                        processing.sensitivity = value.min(100)
                    }
                }
                if let Some(call) = &self.call {
                    call.apply(&sidebar.voice_intent);
                }
                settings.devices.save(sidebar).await;
                return;
            }
            Command::SetOutputVolume(volume) => {
                sidebar.voice_intent.preferred.output_volume = volume.min(200);
            }
            Command::SetParticipantVolume(id, change) => {
                if sidebar.profile.id.as_ref() == Some(&id)
                    || !sidebar.voice.as_ref().is_some_and(|voice| {
                        voice.is_participating()
                            && sidebar
                                .voice_presence
                                .get(&id)
                                .is_some_and(|member| member.channel_id == voice.channel_id)
                    })
                {
                    return;
                }
                let preferred = &mut sidebar.voice_intent.preferred;
                let mut volume = preferred.participant_volume(&id);
                match change {
                    ParticipantVolumeChange::Microphone(value) => volume.mic = value.min(200),
                    ParticipantVolumeChange::Screen(value) => volume.screen = value.min(200),
                    ParticipantVolumeChange::Muted(muted) => volume.muted = muted,
                }
                if volume == Default::default() {
                    preferred.participant_volumes.remove(&id);
                } else {
                    preferred.participant_volumes.insert(id, volume);
                }
            }
            Command::ChooseDevice(kind, device) => {
                let choice = sidebar.voice_intent.preferred.get_mut(kind);
                let changed = *choice != device;
                *choice = device;
                match kind {
                    DeviceKind::Microphone => sidebar.voice_intent.retry_mic += 1,
                    DeviceKind::Speakers => sidebar.voice_intent.retry_speakers += 1,
                    DeviceKind::MediaOutput => {}
                }
                if let Some(call) = &self.call {
                    call.apply(&sidebar.voice_intent);
                }
                if changed || settings.devices.notice.is_some() || settings.devices.save_failed {
                    settings.devices.save(sidebar).await;
                }
                return;
            }
            Command::SetRadioVolume(volume) => {
                let volume = volume.min(200);
                if sidebar.voice_intent.preferred.radio_volume != volume {
                    sidebar.voice_intent.preferred.radio_volume = volume;
                    settings.devices.save(sidebar).await;
                }
                return;
            }
            Command::SetSoundboardEnabled(enabled) => {
                sidebar.voice_intent.preferred.soundboard_enabled = enabled;
            }
            Command::SetSoundboardVolume(volume) => {
                sidebar.voice_intent.preferred.soundboard_volume = volume.min(100);
            }
            Command::SetCameraEnabled(enabled) => {
                if !sidebar
                    .voice
                    .as_ref()
                    .is_some_and(VoiceView::is_participating)
                {
                    return;
                }
                sidebar.voice_intent.camera.enabled = enabled;
                sidebar.voice_intent.camera.revision += 1;
            }
            Command::ChooseCamera(device) => {
                if matches!(sidebar.camera_status(), CameraStatus::Failed(_)) {
                    sidebar.voice_intent.camera.enabled = false;
                }
                sidebar.voice_intent.camera.device = device;
            }
            Command::ChooseCameraResolution(resolution) => {
                if matches!(sidebar.camera_status(), CameraStatus::Failed(_)) {
                    sidebar.voice_intent.camera.enabled = false;
                }
                sidebar.voice_intent.camera.resolution = resolution;
            }
            Command::OpenScreenShare | Command::ChooseScreenShareDisplay(_) => {
                let Some(voice) = sidebar
                    .voice
                    .as_mut()
                    .filter(|voice| voice.is_participating())
                else {
                    return;
                };
                let display_only = matches!(command, Command::ChooseScreenShareDisplay(_));
                let allowed = match command {
                    Command::ChooseScreenShareDisplay(attempt) => {
                        sidebar.voice_intent.preferred.screen_audio
                            && voice.screen.as_ref().is_some_and(|report| {
                                report.attempt == attempt
                                    && matches!(
                                        report.state,
                                        State::Confirm {
                                            can_choose_display: true,
                                            ..
                                        }
                                    )
                            })
                    }
                    _ => voice
                        .screen
                        .as_ref()
                        .is_none_or(|report| matches!(report.state, State::Ended(_))),
                };
                if !allowed {
                    return;
                }
                let playback = voice
                    .media
                    .as_ref()
                    .and_then(|media| media.speakers.clone());
                sidebar.voice_intent.screen.open(playback);
                sidebar.voice_intent.screen.display_only = display_only;
                voice.screen = Some(screen::Report {
                    attempt: sidebar.voice_intent.screen.attempt,
                    state: State::Picking,
                });
            }
            Command::ChooseScreenSource(attempt, id) => {
                let Some(voice) = sidebar
                    .voice
                    .as_mut()
                    .filter(|voice| voice.is_participating())
                else {
                    return;
                };
                if !voice.screen.as_ref().is_some_and(|report| {
                    matches!(&report.state, State::Choices(choices) if choices.iter().any(|choice| choice.id == id))
                }) {
                    return;
                }
                let playback = voice
                    .media
                    .as_ref()
                    .and_then(|media| media.speakers.clone());
                sidebar
                    .voice_intent
                    .screen
                    .advance(attempt, Action::Choose(id, playback));
            }
            #[cfg(windows)]
            Command::ChooseWindowsScreenSource(attempt, item) => {
                if !sidebar.voice.as_ref().is_some_and(|voice| {
                    voice.is_participating()
                        && voice
                            .screen
                            .as_ref()
                            .is_some_and(|report| matches!(&report.state, State::Choices(_)))
                }) {
                    return;
                }
                sidebar
                    .voice_intent
                    .screen
                    .advance(attempt, Action::ChooseSystem(item));
            }
            Command::StartScreenShare(attempt, confirmed_audio) => {
                let Some(State::Confirm { audio, .. }) = sidebar
                    .voice
                    .as_ref()
                    .filter(|voice| voice.is_participating())
                    .and_then(|voice| voice.screen.as_ref())
                    .filter(|report| report.attempt == attempt)
                    .map(|report| &report.state)
                else {
                    return;
                };
                let audio = confirmed_audio
                    && audio.is_some()
                    && sidebar.voice_intent.preferred.screen_audio;
                sidebar
                    .voice_intent
                    .screen
                    .advance(attempt, Action::Start(audio));
            }
            Command::StopScreenShare(attempt) => {
                if sidebar.voice_intent.screen.advance(attempt, Action::Stop)
                    && let Some(voice) = &mut sidebar.voice
                    && let Some(report) = voice.screen.take()
                    && report.state.active()
                {
                    settings.sounds.play(Sound::Screen, false);
                }
            }
            Command::SetScreenAudio(enabled) => {
                sidebar.voice_intent.preferred.screen_audio = enabled;
            }
            Command::SetScreenHeight(height) => {
                if screen::Quality::HEIGHTS.contains(&height) {
                    sidebar.voice_intent.screen.quality.height = height;
                }
            }
            Command::SetScreenFps(fps) => {
                if screen::Quality::FPS.contains(&fps) {
                    sidebar.voice_intent.screen.quality.fps = fps;
                }
            }
            Command::SetWatching(id, enabled) => {
                let Some(voice) = sidebar
                    .voice
                    .as_ref()
                    .filter(|voice| voice.is_participating())
                else {
                    return;
                };
                if enabled {
                    if !sidebar.can_watch_video(&voice.channel_id, &id) {
                        return;
                    }
                    sidebar.voice_intent.watching.insert(id);
                } else {
                    sidebar.voice_intent.watching.remove(&id);
                }
            }
            _ => return,
        }
        if let Some(call) = &self.call {
            call.apply(&sidebar.voice_intent);
        }
        if save_preferences {
            settings.devices.save(sidebar).await;
        }
    }
}
