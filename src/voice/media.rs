use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use livekit::rtc_engine::{SessionStats, lk_runtime::LkRuntime};
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use livekit::{DisconnectReason, prelude::*};
use tokio::{
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior, interval},
};

use super::{
    Device, Devices, Intent, Issue, Report, Status, Update, VoiceProcessing, Worker,
    devices::{route_speakers, switch_speakers},
    screen,
    video::Video,
    wait_until,
};
use crate::api::VoiceFlags;

mod capture;
mod diagnostics;
pub use diagnostics::Diagnostics;
use diagnostics::Sampler;
#[cfg(test)]
pub(crate) mod fixture;
mod livekit_audio;
#[cfg(target_os = "macos")]
mod macos_audio;
mod microphone_watchdog;
mod processing;
mod remote_audio;
mod voice_output;

#[cfg(target_os = "macos")]
pub(crate) use capture::microphone_permission;
use capture::{Capture, stop_recording};
use livekit_audio::LiveKitAudio;
use microphone_watchdog::{MicrophoneSample, MicrophoneSamples, MicrophoneWatchdog};

type RoomEvents = tokio::sync::mpsc::UnboundedReceiver<RoomEvent>;

#[derive(Default)]
pub(super) struct Media {
    audio: Option<PlatformAudio>,
    capture: Capture,
    video: Video,
    connecting: Option<JoinHandle<Result<(Room, RoomEvents), Issue>>>,
    room: Option<Arc<Room>>,
    #[cfg(test)]
    fixture: Option<fixture::TestRoom>,
}

impl Media {
    pub(super) async fn stop_audio(&mut self) {
        #[cfg(test)]
        if let Some(fixture) = &mut self.fixture {
            fixture.stop_audio();
        }
        self.video.stop_camera().await;
        self.capture.shutdown().await;
        if let Some(audio) = self.audio.take() {
            stop_recording(&audio);
            LkRuntime::instance()
                .pc_factory()
                .set_adm_playout_enabled(false);
        }
    }

    pub(super) async fn close(self) {
        #[cfg(test)]
        if let Some(fixture) = self.fixture {
            fixture.close();
        }
        if let Some(room) = self.room {
            let _ = room.close().await;
        } else if let Some(connecting) = self.connecting
            && let Ok(Ok((room, _))) = connecting.await
        {
            let _ = room.close().await;
        }
    }

    pub(super) async fn connect_and_listen(
        worker: &Worker,
        media: &mut Self,
        token: String,
    ) -> Result<(), Issue> {
        let audio = media
            .audio
            .insert(PlatformAudio::new().map_err(|_| Issue::Playback)?);
        stop_recording(audio);
        let devices = Devices::scan(audio).await?;
        let requested = worker.requested.borrow().clone();
        let speakers = route_speakers(
            &devices.speakers,
            requested.preferred.speakers.as_ref(),
            None,
            |device| switch_speakers(audio, device),
        )
        .ok_or(Issue::Playback)?;
        LkRuntime::instance()
            .pc_factory()
            .set_adm_playout_enabled(false);
        // Drive the SDK's join to completion so cancellation can close any room it returns.
        let connection = media.connecting.insert(tokio::spawn(connect(token)));
        let connected = connection.await.map_err(|_| Issue::Media)?;
        media.connecting.take();
        let (room, events) = connected?;
        let room: &Arc<Room> = media.room.insert(Arc::new(room));
        let mut backend = LiveKitAudio {
            room,
            audio,
            events,
            capture: &mut media.capture,
            remote: Default::default(),
        };
        tokio::select! {
            biased;
            result = Self::listen(worker, &mut backend, &speakers) => result,
            () = async {
                tokio::join!(media.video.run(worker, room), screen::run(worker, room));
                std::future::pending::<()>().await;
            } => unreachable!(),
        }
    }

    async fn listen(
        worker: &Worker,
        backend: &mut impl RoomAudio,
        speakers: &Device,
    ) -> Result<(), Issue> {
        let mut requested = worker.requested.clone();
        let mut devices = Arc::new(backend.devices().await?);
        let mut intent = requested.borrow_and_update().clone();
        // LiveKit can initialize WebRTC with the default route while connecting.
        let mut output = route_speakers(
            &devices.speakers,
            intent.preferred.speakers.as_ref(),
            Some(speakers),
            |device| backend.switch_speakers(device),
        );
        let playing = output.as_ref().filter(|_| !intent.mute.deafened);
        let mut playout = Playout {
            format: playing.and_then(|device| backend.playout_format(device)),
            recording_initialized: false,
        };
        let me = backend.identity();
        let mut speaking = Speaking::default();
        let mut reported = None;
        // shortcut: a remove and return between scans can be missed, use backend notifications when available.
        let mut scan = interval(Duration::from_secs(1));
        scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
        scan.tick().await;
        let mut check_routes = true;
        let mut retry_mic = intent.retry_mic;
        let mut mic_uses_system_default = false;
        let mut previous_default_mic = None;
        let mut retry_speakers = intent.retry_speakers;
        let mut reconnecting = false;
        let mut resync = false;

        let mut diagnostics = Diagnostics::default();
        let mut echo_reference_unavailable = false;
        let mut stats = Sampler::default();
        let mut stats_job = JoinSet::new();
        let mut microphones = MicrophoneWatchdog::default();
        let mut microphone_job = JoinSet::new();
        let mut watchdog_requested = false;

        loop {
            if reconnecting
                && intent.mute.effective_muted()
                && backend.selected_microphone().is_some()
            {
                backend.stop_microphone().await;
            }
            if check_routes && !reconnecting {
                check_routes = false;
                let preferred_mic = devices
                    .microphones
                    .preferred(intent.preferred.microphone.as_ref());
                let default_mic = devices.microphones.default.as_ref();
                let use_system_default = !cfg!(windows) && preferred_mic.is_none();
                let wanted = if intent.mute.effective_muted() {
                    None
                } else {
                    preferred_mic.or(default_mic)
                };
                // PulseAudio can move a CPAL stream off the device that used to be the default.
                let restore_preferred_mic = cfg!(target_os = "linux")
                    && preferred_mic.is_some_and(|device| {
                        device.id.starts_with("pulseaudio:")
                            && previous_default_mic.as_ref() == Some(device)
                            && default_mic != Some(device)
                    });
                previous_default_mic.clone_from(&devices.microphones.default);
                if backend.selected_microphone() != wanted
                    || retry_mic != intent.retry_mic
                    || wanted.is_some() && mic_uses_system_default != use_system_default
                    || restore_preferred_mic
                {
                    backend.stop_microphone().await;
                    speaking.remove(me.as_str());
                    retry_mic = intent.retry_mic;
                }
                if intent.mute.deafened {
                    backend.set_playout(false);
                }
                if devices.speakers.resolve(intent.preferred.speakers.as_ref()) != output.as_ref()
                    || retry_speakers != intent.retry_speakers
                    || resync
                {
                    retry_speakers = intent.retry_speakers;
                    let next_output = route_speakers(
                        &devices.speakers,
                        intent.preferred.speakers.as_ref(),
                        output.as_ref(),
                        |device| backend.switch_speakers(device),
                    );
                    if output != next_output {
                        playout.format = None;
                    }
                    output = next_output;
                }
                let playing = output.as_ref().filter(|_| !intent.mute.deafened);
                echo_reference_unavailable = backend.sync_remote_audio(&intent, &devices, playing);
                playout.sync(
                    playing.is_some(),
                    playing.and_then(|device| backend.playout_format(device)),
                    backend.macos_recording_initialized(),
                    |enabled| backend.set_playout(enabled),
                );
                let mut processing = intent.preferred.processing;
                processing.echo_cancellation &= !echo_reference_unavailable;
                if let Some(device) = wanted
                    && backend.can_start_microphone()
                {
                    backend.start_microphone(device.clone(), processing, use_system_default);
                    mic_uses_system_default = use_system_default;
                }
                backend.sync_microphone_processing(processing).await;
                if resync
                    && !intent.mute.effective_muted()
                    && !requested.borrow().mute.effective_muted()
                {
                    backend.resume_microphone();
                }
            }

            microphones.retain(&intent, output.is_some());
            speaking.expire(Instant::now());
            let muted = reconnecting || backend.active_microphone().is_none();
            if muted {
                speaking.remove(me.as_str());
            }
            let flags = VoiceFlags {
                muted,
                deafened: intent.mute.deafened,
                speaking: speaking.0.contains_key(me.as_str()),
                radio_listening: None,
            };
            worker.publish_flags(flags, !reconnecting, std::mem::take(&mut resync));
            let report = Report {
                status: if reconnecting {
                    Status::Reconnecting
                } else if !intent.mute.effective_muted() && backend.microphone_unavailable() {
                    Status::ListenOnly
                } else {
                    Status::Connected
                },
                devices: devices.clone(),
                microphone: backend.active_microphone().cloned(),
                speakers: output.clone(),
                speaking: speaking.0.keys().cloned().collect(),
                connecting_microphones: microphones.connecting(Instant::now()),
                diagnostics,
                echo_reference_unavailable,
            };
            if reported.as_ref() != Some(&report) {
                reported = Some(report.clone());
                worker.update(Update::Media(Arc::new(report)));
            }

            tokio::select! {
                biased;
                result = requested.changed() => {
                    result.map_err(|_| Issue::Disconnected)?;
                    let next = requested.borrow_and_update().clone();
                    if next.refresh_devices != intent.refresh_devices
                        || next.preferred.microphone != intent.preferred.microphone
                        || next.preferred.speakers != intent.preferred.speakers
                        || next.preferred.media_output != intent.preferred.media_output
                        || next.retry_mic != intent.retry_mic || next.retry_speakers != intent.retry_speakers
                        || (intent.mute.effective_muted() && !next.mute.effective_muted())
                    {
                        devices = Arc::new(backend.devices().await?);
                    }
                    intent = next;
                    check_routes = true;
                }
                event = backend.next_event(reconnecting) => match backend.handle_event(event).await {
                    MediaEvent::Speaking(active) if !reconnecting => speaking.update(active, Instant::now()),
                    MediaEvent::ParticipantLeft(id) => {
                        speaking.remove(&id);
                        microphones.participant_left(&id);
                        microphone_job = JoinSet::new();
                        check_routes = true;
                    }
                    MediaEvent::Reconnecting => {
                        reconnecting = true;
                        echo_reference_unavailable = false;
                        diagnostics = Diagnostics::default();
                        stats = Sampler::default();
                        stats_job = JoinSet::new();
                        microphones = MicrophoneWatchdog::default();
                        microphone_job = JoinSet::new();
                        watchdog_requested = false;
                        backend.silence_microphone();
                        backend.set_playout(false);
                        backend.suspend_remote_audio();
                        playout.format = None;
                        speaking.0.clear();
                    }
                    MediaEvent::Reconnected if reconnecting => {
                        reconnecting = false;
                        resync = true;
                        check_routes = true;
                        devices = Arc::new(backend.devices().await?);
                        backend.reconcile_microphone().await;
                        intent = requested.borrow_and_update().clone();
                    }
                    MediaEvent::Disconnected(issue) => return Err(issue),
                    MediaEvent::MicrophoneChanged | MediaEvent::RemoteAudioChanged => check_routes = true,
                    MediaEvent::Quality(quality) if !reconnecting => diagnostics.quality = Some(quality),
                    _ => {}
                },
                _ = scan.tick() => {
                    devices = Arc::new(backend.devices().await?);
                    check_routes = true;
                    if !reconnecting && stats_job.is_empty() {
                        backend.start_stats(&mut stats_job);
                    }
                    if !reconnecting && output.is_some() && microphone_job.is_empty() {
                        let expected = intent.remote_microphone_mutes.keys()
                            .filter(|id| intent.expects_remote_microphone(id))
                            .cloned()
                            .collect::<Vec<_>>();
                        if !expected.is_empty() {
                            backend.sample_microphones(expected, &mut microphone_job);
                        }
                    }
                }
                Some(Ok(samples)) = microphone_job.join_next() => {
                    let now = Instant::now();
                    let repairs = microphones.observe(&intent, samples, now);
                    if !repairs.is_empty() {
                        backend.repair_microphones(repairs).await;
                        check_routes = true;
                    } else if !watchdog_requested
                        && intent.watchdog_rejoin_after.is_none_or(|deadline| now >= deadline)
                        && let Some(output_since) = backend.voice_output_healthy_since()
                        && microphones.widespread_stall(&intent, now, output_since)
                    {
                        watchdog_requested = true;
                        worker.update(Update::RemoteAudioStalled);
                    }
                }
                Some(result) = stats_job.join_next() => {
                    let sample = result.ok().flatten();
                    let quality = diagnostics.quality;
                    diagnostics = stats.room(sample.as_ref());
                    diagnostics.quality = quality;
                }
                _ = wait_until(speaking.deadline()) => {},
            }
        }
    }
}

trait RoomAudio {
    type Event;
    type PlayoutFormat: PartialEq;

    fn identity(&self) -> String;
    async fn devices(&self) -> Result<Devices, Issue>;
    fn switch_speakers(&self, device: &Device) -> bool;
    fn set_playout(&self, enabled: bool);
    // Reports when the active voice output cannot provide an echo reference.
    fn sync_remote_audio(
        &mut self,
        intent: &Intent,
        devices: &Devices,
        speakers: Option<&Device>,
    ) -> bool;
    fn suspend_remote_audio(&mut self) {}
    fn voice_output_healthy_since(&self) -> Option<Instant>;
    fn playout_format(&self, device: &Device) -> Option<Self::PlayoutFormat>;
    fn macos_recording_initialized(&self) -> bool;
    fn selected_microphone(&self) -> Option<&Device>;
    fn active_microphone(&self) -> Option<&Device>;
    fn can_start_microphone(&self) -> bool;
    fn microphone_unavailable(&self) -> bool;
    fn start_microphone(
        &mut self,
        device: Device,
        processing: VoiceProcessing,
        use_system_default: bool,
    );
    async fn sync_microphone_processing(&mut self, _: VoiceProcessing) {}
    async fn stop_microphone(&mut self);
    fn silence_microphone(&self);
    fn resume_microphone(&self);
    async fn reconcile_microphone(&mut self);
    fn sample_microphones(&self, expected: Vec<String>, jobs: &mut JoinSet<MicrophoneSamples>);
    async fn repair_microphones(&mut self, repairs: Vec<(String, TrackSid)>);
    fn start_stats(&self, _: &mut JoinSet<Option<SessionStats>>) {}
    async fn next_event(&mut self, reconnecting: bool) -> Self::Event;
    async fn handle_event(&mut self, event: Self::Event) -> MediaEvent;
}

enum MediaEvent {
    Speaking(HashSet<String>),
    ParticipantLeft(String),
    Reconnecting,
    Reconnected,
    Disconnected(Issue),
    MicrophoneChanged,
    RemoteAudioChanged,
    Quality(ConnectionQuality),
    Other,
}

struct Playout<Format> {
    format: Option<Format>,
    recording_initialized: bool,
}

impl<Format: PartialEq> Playout<Format> {
    fn sync(
        &mut self,
        enabled: bool,
        format: Option<Format>,
        recording_initialized: bool,
        mut set_enabled: impl FnMut(bool),
    ) {
        let recording_changed =
            std::mem::replace(&mut self.recording_initialized, recording_initialized)
                != recording_initialized;
        let format_changed = self
            .format
            .as_ref()
            .zip(format.as_ref())
            .is_some_and(|(previous, current)| previous != current);
        if enabled && (format_changed || (format.is_none() && recording_changed)) {
            // CoreAudio can renegotiate the format after capture has started or stopped.
            set_enabled(false);
        }
        if let Some(format) = format {
            self.format = Some(format);
        } else if !enabled {
            self.format = None;
        }
        set_enabled(enabled);
    }
}

#[derive(Default)]
struct Speaking(HashMap<String, Option<Instant>>);

impl Speaking {
    fn update(&mut self, active: HashSet<String>, now: Instant) {
        for (id, off) in &mut self.0 {
            if !active.contains(id) {
                off.get_or_insert(now + Duration::from_millis(500));
            }
        }
        self.0.extend(active.into_iter().map(|id| (id, None)));
    }

    fn remove(&mut self, id: &str) {
        self.0.remove(id);
    }

    fn deadline(&self) -> Option<Instant> {
        self.0.values().filter_map(|off| *off).min()
    }

    fn expire(&mut self, now: Instant) {
        self.0.retain(|_, off| off.is_none_or(|off| off > now));
    }
}

async fn connect(token: String) -> Result<(Room, RoomEvents), Issue> {
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.join_retries = 0;
    Room::connect(env!("LIVEKIT_URL"), &token, options)
        .await
        .map_err(|_| Issue::Media)
}

pub(super) fn disconnect_issue(reason: DisconnectReason) -> Issue {
    match reason {
        DisconnectReason::UnknownReason
        | DisconnectReason::SignalClose
        | DisconnectReason::StateMismatch
        | DisconnectReason::ConnectionTimeout
        | DisconnectReason::ServerShutdown
        | DisconnectReason::MediaFailure
        | DisconnectReason::JoinFailure => Issue::LiveKitDisconnected,
        _ => Issue::Disconnected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_format_changes_reopen_the_same_playout_stream() {
        let stereo = (48_000, 2);
        let mono = (24_000, 1);
        let mut playout = Playout {
            format: Some(stereo),
            recording_initialized: false,
        };
        let mut open_format = Some(stereo);
        let mut update = |play: bool, capturing, hardware_format| {
            let mut stopped = false;
            playout.sync(
                play,
                play.then_some(hardware_format),
                capturing,
                |enabled| {
                    assert!(play || !enabled);
                    if enabled {
                        open_format.get_or_insert(hardware_format);
                    } else {
                        stopped = open_format.take().is_some();
                    }
                },
            );
            (open_format, stopped)
        };

        assert_eq!(update(true, true, stereo), (Some(stereo), false));
        assert_eq!(update(true, true, mono), (Some(mono), true));
        assert_eq!(update(true, true, mono), (Some(mono), false));
        let wideband_mono = (48_000, 1);
        assert_eq!(update(true, false, mono), (Some(mono), false));
        assert_eq!(
            update(true, false, wideband_mono),
            (Some(wideband_mono), true)
        );
        assert_eq!(update(true, false, stereo), (Some(stereo), true));
        assert_eq!(update(false, false, stereo), (None, true));
        assert_eq!(update(false, true, mono), (None, false));
        assert_eq!(update(true, true, mono), (Some(mono), false));
    }

    #[test]
    fn media_failure_is_retryable_and_session_replacement_is_authoritative() {
        assert!(matches!(
            disconnect_issue(DisconnectReason::MediaFailure),
            Issue::LiveKitDisconnected
        ));
        assert!(matches!(
            disconnect_issue(DisconnectReason::JoinFailure),
            Issue::LiveKitDisconnected
        ));
        assert!(matches!(
            disconnect_issue(DisconnectReason::DuplicateIdentity),
            Issue::Disconnected
        ));
    }

    #[test]
    fn holds_speaking_after_activity_stops() {
        let now = Instant::now();
        let mut speaking = Speaking::default();
        speaking.update(HashSet::from(["alice".into()]), now);
        speaking.update(HashSet::new(), now);
        speaking.expire(now + Duration::from_millis(499));
        assert!(speaking.0.contains_key("alice"));
        speaking.expire(now + Duration::from_millis(500));
        assert!(speaking.0.is_empty());
    }
}
