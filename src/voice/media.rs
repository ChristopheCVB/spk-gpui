use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use livekit::rtc_engine::lk_runtime::LkRuntime;
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use livekit::{DisconnectReason, prelude::*};
use tokio::{
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval},
};

use super::{
    Device, Devices, Issue, Report, Status, Update, Worker, devices::route_speakers, wait_until,
};
use crate::api::VoiceFlags;

mod capture;

use capture::{Capture, stop_recording};

type RoomEvents = tokio::sync::mpsc::UnboundedReceiver<RoomEvent>;

#[derive(Default)]
pub(super) struct Media {
    audio: Option<PlatformAudio>,
    capture: Capture,
    connecting: Option<JoinHandle<Result<(Room, RoomEvents), Issue>>>,
    room: Option<Room>,
}

impl Media {
    pub(super) async fn stop_audio(&mut self) {
        self.capture.shutdown().await;
        if let Some(audio) = self.audio.take() {
            stop_recording(&audio);
            LkRuntime::instance()
                .pc_factory()
                .set_adm_playout_enabled(false);
        }
    }

    pub(super) async fn close(self) {
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
        let requested = worker.requested.borrow().clone();
        let devices = Devices::read(audio);
        let speakers = route_speakers(
            audio,
            &devices.speakers,
            requested.preferred.speakers.as_ref(),
            None,
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
        Self::listen(
            worker,
            media.room.insert(room),
            audio,
            events,
            speakers,
            &mut media.capture,
        )
        .await
    }

    async fn listen(
        worker: &Worker,
        room: &Room,
        audio: &PlatformAudio,
        mut events: RoomEvents,
        speakers: Device,
        capture: &mut Capture,
    ) -> Result<(), Issue> {
        let mut requested = worker.requested.clone();
        let mut intent = requested.borrow_and_update().clone();
        let runtime = LkRuntime::instance();
        let mut devices = Arc::new(Devices::read(audio));
        // LiveKit can initialize WebRTC with the default route while connecting.
        let mut output = route_speakers(
            audio,
            &devices.speakers,
            intent.preferred.speakers.as_ref(),
            Some(&speakers),
        );
        runtime
            .pc_factory()
            .set_adm_playout_enabled(output.is_some() && !intent.mute.deafened);
        subscribe_to_microphones(room);
        let me = room.local_participant().identity();
        let mut speaking = Speaking::default();
        let mut reported = None;
        // shortcut: a remove and return between scans can be missed, use backend notifications when available.
        let mut scan = interval(Duration::from_secs(1));
        scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
        scan.tick().await;
        let mut check_routes = true;
        let mut retry_mic = intent.retry_mic;
        let mut retry_speakers = intent.retry_speakers;
        let mut reconnecting = false;
        let mut resync = false;
        let mut recording = false;

        loop {
            if reconnecting && intent.mute.effective_muted() && capture.selected_device().is_some()
            {
                capture.stop(room, audio).await;
            }
            if check_routes && !reconnecting {
                check_routes = false;
                let input = devices
                    .microphones
                    .resolve(intent.preferred.microphone.as_ref());
                let wanted = if intent.mute.effective_muted() {
                    None
                } else {
                    input
                };
                if capture.selected_device() != wanted || retry_mic != intent.retry_mic {
                    capture.stop(room, audio).await;
                    speaking.remove(me.as_str());
                    retry_mic = intent.retry_mic;
                }
                if intent.mute.deafened {
                    runtime.pc_factory().set_adm_playout_enabled(false);
                }
                if devices.speakers.resolve(intent.preferred.speakers.as_ref()) != output.as_ref()
                    || retry_speakers != intent.retry_speakers
                    || resync
                {
                    retry_speakers = intent.retry_speakers;
                    output = route_speakers(
                        audio,
                        &devices.speakers,
                        intent.preferred.speakers.as_ref(),
                        output.as_ref(),
                    );
                }
                // shortcut: track capture start/stop, use CoreAudio format notifications if Bluetooth switches later.
                let capturing = cfg!(target_os = "macos") && audio.is_recording_initialized();
                sync_playout(
                    output.is_some() && !intent.mute.deafened,
                    &mut recording,
                    capturing,
                    |enabled| {
                        runtime.pc_factory().set_adm_playout_enabled(enabled);
                    },
                );
                if let Some(device) = wanted
                    && capture.can_start()
                {
                    capture.start(room, audio, device.clone());
                }
                if resync && !intent.mute.effective_muted() {
                    capture.resume();
                }
            }

            speaking.expire(Instant::now());
            let muted = reconnecting || !capture.has_publication();
            if muted {
                speaking.remove(me.as_str());
            }
            let flags = VoiceFlags {
                muted,
                deafened: intent.mute.deafened,
                speaking: speaking.0.contains_key(me.as_str()),
            };
            worker.publish_flags(flags, !reconnecting, std::mem::take(&mut resync));
            let report = Report {
                status: if reconnecting {
                    Status::Reconnecting
                } else if !intent.mute.effective_muted() && capture.unavailable() {
                    Status::ListenOnly
                } else {
                    Status::Connected
                },
                devices: devices.clone(),
                microphone: capture.active_device().cloned(),
                speakers: output.clone(),
                speaking: speaking.0.keys().cloned().collect(),
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
                    if next.refresh_devices != intent.refresh_devices || next.preferred != intent.preferred
                        || next.retry_mic != intent.retry_mic || next.retry_speakers != intent.retry_speakers
                    {
                        devices = Arc::new(Devices::read(audio));
                    }
                    intent = next;
                    check_routes = true;
                }
                event = events.recv() => match event {
                    Some(RoomEvent::TrackPublished { publication, .. }) => subscribe_to_microphone(&publication),
                    Some(RoomEvent::ActiveSpeakersChanged { speakers }) if !reconnecting => speaking.update(
                        speakers.into_iter().map(|speaker| speaker.identity().to_string()).collect(),
                        Instant::now(),
                    ),
                    Some(RoomEvent::ParticipantDisconnected(participant)) => speaking.remove(participant.identity().as_str()),
                    Some(RoomEvent::LocalTrackRepublished { publication, .. })
                        if !capture.owns_track(&publication.sid()) =>
                    {
                        // The SDK can be republishing while mute or deafen removes the old track.
                        let _ = room.local_participant().unpublish_track(&publication.sid()).await;
                    }
                    Some(RoomEvent::Reconnecting) => {
                        reconnecting = true;
                        capture.silence();
                        runtime.pc_factory().set_adm_playout_enabled(false);
                        speaking.0.clear();
                    }
                    Some(RoomEvent::Reconnected) if reconnecting => {
                        reconnecting = false;
                        resync = true;
                        check_routes = true;
                        intent = requested.borrow_and_update().clone();
                        devices = Arc::new(Devices::read(audio));
                        capture.reconcile_publication(room, audio).await;
                        subscribe_to_microphones(room);
                    }
                    Some(RoomEvent::Disconnected { reason }) => return Err(disconnect_issue(reason)),
                    None => return Err(Issue::LiveKitDisconnected),
                    _ => {}
                },
                Some(published) = capture.next_publication(), if !reconnecting => {
                    capture.complete(room, audio, published).await;
                    check_routes = true;
                }
                _ = scan.tick() => {
                    devices = Arc::new(Devices::read(audio));
                    check_routes = true;
                }
                _ = wait_until(speaking.deadline()) => {},
            }
        }
    }
}

fn sync_playout(
    enabled: bool,
    previous_recording: &mut bool,
    recording: bool,
    mut set_enabled: impl FnMut(bool),
) {
    let recording_changed = std::mem::replace(previous_recording, recording) != recording;
    if enabled && recording_changed {
        // CoreAudio can change the speaker format while the selected device stays the same.
        set_enabled(false);
    }
    set_enabled(enabled);
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

fn subscribe_to_microphones(room: &Room) {
    for participant in room.remote_participants().into_values() {
        for publication in participant.track_publications().into_values() {
            subscribe_to_microphone(&publication);
        }
    }
}

fn subscribe_to_microphone(publication: &RemoteTrackPublication) {
    if publication.kind() == TrackKind::Audio && publication.source() == TrackSource::Microphone {
        publication.set_subscribed(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_format_changes_reopen_the_same_playout_stream() {
        let stereo = (48_000, 2);
        let mono = (24_000, 1);
        let mut recording = false;
        let mut open_format = Some(stereo);
        let mut update = |play, capturing, hardware_format| {
            let mut stopped = false;
            sync_playout(play, &mut recording, capturing, |enabled| {
                assert!(play || !enabled);
                if enabled {
                    open_format.get_or_insert(hardware_format);
                } else {
                    stopped = open_format.take().is_some();
                }
            });
            (open_format, stopped)
        };

        assert_eq!(update(true, true, mono), (Some(mono), true));
        assert_eq!(update(true, true, mono), (Some(mono), false));
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
