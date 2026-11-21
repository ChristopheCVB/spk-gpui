use std::{path::PathBuf, sync::Arc, time::Duration};

use tokio::{task::spawn_blocking, time::Instant};

use super::{Command, Sidebar};
use crate::api::{Connection, SpkClient, VoiceSocket};
use crate::sounds::{Sound, VoiceSounds};
use crate::voice::{
    self, Call, DeviceKind, DevicePreferences, Devices, Intent, Issue as VoiceIssue,
    ParticipantVolumeChange, Report as VoiceReport, Status as VoiceStatus, Update as VoiceUpdate,
    screen::{self, Action, State},
    video::{CameraStatus, VideoReport},
};

pub(super) const VOICE_REPLACED: &str = "You joined voice from another tab or device.";
const CHANNEL_UNAVAILABLE: &str = "This voice channel is no longer available.";

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

    fn voice_replaced(&self) -> bool {
        self.profile
            .id
            .as_ref()
            .and_then(|id| self.voice_presence.get(id))
            .zip(self.voice.as_ref())
            .is_some_and(|(member, voice)| member.channel_id != voice.channel_id)
    }

    pub(super) fn finish_voice(&mut self, result: Result<(), VoiceIssue>) -> bool {
        let voice = self.voice.take();
        self.voice_intent.camera.enabled = false;
        self.voice_intent.watching.clear();
        self.voice_intent.screen.action = Action::Stop;
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
        self.can_join_voice(id).then_some(id)
    }
}

pub(super) struct DeviceSettings {
    pub(super) path: Option<PathBuf>,
    pub(super) preferred: DevicePreferences,
    pub(super) notice: Option<&'static str>,
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
            "Could not save audio settings. Your choices will only last until you close SPK.",
        );
    }
}

pub(super) type StartCall = dyn Fn(String, SpkClient, VoiceSocket, Intent) -> Call + Send + Sync;

pub(super) struct VoiceSettings {
    pub(super) socket_endpoint: String,
    pub(super) start_call: Box<StartCall>,
    pub(super) enumerate_devices: fn() -> Devices,
    pub(super) devices: DeviceSettings,
    pub(super) sounds: VoiceSounds,
}

#[derive(Default)]
pub(super) struct VoiceSession {
    socket: Option<VoiceSocket>,
    call: Option<Call>,
    reclaim: Option<VoiceReclaim>,
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
        if self.call.is_none()
            && let Some(VoiceDestination {
                channel_id,
                server_move,
            }) = sidebar.switching_to.take()
        {
            if let Some(socket) = &self.socket
                && (server_move || sidebar.has_voice_channel(&channel_id))
            {
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
                sidebar.voice_notice = Some(if self.socket.is_none() {
                    VoiceIssue::Socket.message()
                } else {
                    CHANNEL_UNAVAILABLE
                });
            }
        }
    }

    pub(super) fn connected(&mut self, socket: VoiceSocket) {
        self.reclaim = self
            .call
            .is_some()
            .then_some(VoiceReclaim::WaitingForPresence);
        self.socket = Some(socket);
    }

    pub(super) fn disconnected(&mut self) {
        self.socket = None;
        self.reclaim = None;
        if let Some(call) = &self.call {
            call.set_socket(None);
        }
    }

    pub(super) fn radio_scope<'a>(&'a self, sidebar: &'a Sidebar) -> Option<(&'a str, &'a str)> {
        let voice = sidebar.voice.as_ref()?;
        (self.socket.is_some() && voice.status != VoiceStatus::Leaving)
            .then_some((self.call.as_ref()?.sid(), voice.channel_id.as_str()))
    }

    pub(super) fn radio_socket(&self, sidebar: &Sidebar) -> Option<(VoiceSocket, String)> {
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

    pub(super) fn retry_presence(&mut self) {
        self.reclaim = Some(VoiceReclaim::Retried);
    }

    pub(super) fn own_presence_updated(&mut self, sidebar: &mut Sidebar) {
        if self.reclaim.is_some() && sidebar.voice_replaced() {
            self.reclaim = None;
            self.cancel(sidebar, Some(VOICE_REPLACED));
        }
    }

    pub(super) fn presence_refreshed(&mut self, sidebar: &mut Sidebar, loaded: bool) {
        if self.reclaim.is_some() && loaded {
            self.reclaim = None;
            if sidebar.voice_replaced() {
                self.cancel(sidebar, Some(VOICE_REPLACED));
            } else if let Some(call) = &self.call {
                call.set_socket(self.socket.clone());
            }
        } else if matches!(self.reclaim, Some(VoiceReclaim::WaitingForPresence)) {
            self.reclaim = Some(VoiceReclaim::RetryAt(
                Instant::now() + Duration::from_secs(1),
            ));
        }
    }

    pub(super) fn join(&mut self, sidebar: &mut Sidebar, channel_id: String) {
        if sidebar.can_join_voice(&channel_id) {
            self.request_voice(sidebar, channel_id, false);
        } else if self.socket.is_none() {
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
            && self.call.as_ref().is_some_and(|call| call.owns(sid))
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
        if self.call.as_ref().is_some_and(|call| !call.owns(sid)) {
            self.cancel(sidebar, Some(VOICE_REPLACED));
        }
    }

    pub(super) fn server_disconnected(&mut self, sidebar: &mut Sidebar, sid: &str) {
        if self.call.as_ref().is_some_and(|call| call.owns(sid)) {
            self.cancel(sidebar, Some(VoiceIssue::Disconnected.message()));
        }
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
        if let (Some(call), Some(voice)) = (&mut self.call, &mut sidebar.voice)
            && voice.begin_leave(notice.is_none())
        {
            call.cancel();
            sidebar.voice_intent.camera.enabled = false;
            sidebar.voice_intent.watching.clear();
            sidebar.voice_intent.screen.action = Action::Stop;
            sidebar.voice_notice = notice;
        }
    }

    pub(super) async fn next_update(&self) -> VoiceUpdate {
        voice::next_update(self.call.as_ref()).await
    }

    pub(super) fn finished(&mut self) {
        self.call = None;
        self.reclaim = None;
    }

    pub(super) async fn stop(self) {
        if let Some(call) = self.call {
            call.stop().await;
        }
    }

    pub(super) fn participant_changed(
        &self,
        sidebar: &mut Sidebar,
        id: &str,
        channel: Option<&str>,
    ) {
        if let Some(call) = &self.call
            && sidebar
                .voice
                .as_ref()
                .is_some_and(|voice| channel != Some(&voice.channel_id))
            && sidebar.voice_intent.watching.remove(id)
        {
            call.apply(&sidebar.voice_intent);
        }
    }

    pub(super) async fn apply_controls(
        &self,
        sidebar: &mut Sidebar,
        settings: &mut VoiceSettings,
        command: Command,
    ) {
        let save_preferences = matches!(
            &command,
            Command::SetOutputVolume(_)
                | Command::SetParticipantVolume(..)
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
                if changed || settings.devices.notice.is_some() {
                    settings.devices.save(&sidebar.voice_intent.preferred).await;
                    sidebar.device_notice = settings.devices.notice;
                }
                return;
            }
            Command::SetRadioVolume(volume) => {
                let volume = volume.min(200);
                if sidebar.voice_intent.preferred.radio_volume != volume {
                    sidebar.voice_intent.preferred.radio_volume = volume;
                    settings.devices.save(&sidebar.voice_intent.preferred).await;
                    sidebar.device_notice = settings.devices.notice;
                }
                return;
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
                if camera_failed(sidebar) {
                    sidebar.voice_intent.camera.enabled = false;
                }
                sidebar.voice_intent.camera.device = device;
            }
            Command::ChooseCameraResolution(resolution) => {
                if camera_failed(sidebar) {
                    sidebar.voice_intent.camera.enabled = false;
                }
                sidebar.voice_intent.camera.resolution = resolution;
            }
            Command::OpenScreenShare => {
                let Some(voice) = sidebar
                    .voice
                    .as_mut()
                    .filter(|voice| voice.is_participating())
                else {
                    return;
                };
                if voice
                    .screen
                    .as_ref()
                    .is_some_and(|report| !matches!(report.state, State::Ended(_)))
                {
                    return;
                }
                sidebar.voice_intent.screen.open();
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
            Command::StartScreenShare(attempt) => {
                if !sidebar.voice.as_ref().is_some_and(|voice| {
                    voice.is_participating()
                        && voice
                            .screen
                            .as_ref()
                            .is_some_and(|report| matches!(report.state, State::Confirm { .. }))
                }) {
                    return;
                }
                sidebar.voice_intent.screen.advance(
                    attempt,
                    Action::Start(sidebar.voice_intent.preferred.screen_audio),
                );
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
            Command::SetScreenQuality(quality) => {
                if screen::Quality::HEIGHTS.contains(&quality.height)
                    && screen::Quality::FPS.contains(&quality.fps)
                {
                    sidebar.voice_intent.screen.quality = quality;
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
                    if sidebar.profile.id.as_ref() == Some(&id)
                        || !sidebar
                            .voice_presence
                            .get(&id)
                            .is_some_and(|member| member.channel_id == voice.channel_id)
                    {
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
            settings.devices.save(&sidebar.voice_intent.preferred).await;
            sidebar.device_notice = settings.devices.notice;
        }
    }
}

fn camera_failed(sidebar: &Sidebar) -> bool {
    sidebar
        .voice
        .as_ref()
        .and_then(|voice| voice.video.as_ref())
        .is_some_and(|video| matches!(video.camera, CameraStatus::Failed(_)))
}
