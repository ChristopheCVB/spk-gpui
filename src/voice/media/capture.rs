use livekit::prelude::*;
use livekit::rtc_engine::lk_runtime::LkRuntime;
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use tokio::{sync::oneshot, task::JoinSet};

#[cfg(windows)]
use crate::voice::windows_audio;
use crate::voice::{Device, devices::select_microphone};

#[derive(Default)]
pub(super) struct Capture {
    pub(super) device: Option<Device>,
    pub(super) track: Option<LocalAudioTrack>,
    pub(super) published: Option<TrackSid>,
    starting: Option<oneshot::Sender<()>>,
    pub(super) jobs: JoinSet<Option<TrackSid>>,
    pub(super) failed: bool,
}

impl Capture {
    pub(super) fn silence(&self) {
        if let Some(track) = &self.track {
            track.mute();
        }
    }

    fn current_publication(&self, room: &Room) -> Option<TrackSid> {
        // A full LiveKit reconnect can republish the same track with a new SID.
        let sid = self.track.as_ref()?.sid();
        room.local_participant()
            .track_publications()
            .contains_key(&sid)
            .then_some(sid)
    }

    pub(super) async fn reconcile_publication(&mut self, room: &Room, audio: &PlatformAudio) {
        if self.published.is_some() {
            if let Some(sid) = self.current_publication(room) {
                self.published = Some(sid);
            } else {
                self.stop(room, audio).await;
            }
        }
    }

    pub(super) fn start(&mut self, room: &Room, audio: &PlatformAudio, device: Device) {
        let (starting, cancelled) = oneshot::channel();
        let track = LocalAudioTrack::create_audio_track("microphone", audio.rtc_source());
        track.mute();
        self.track = Some(track.clone());
        self.device = Some(device.clone());
        self.starting = Some(starting);
        self.jobs.spawn(publish_microphone(
            room.local_participant(),
            audio.clone(),
            device,
            track,
            cancelled,
        ));
    }

    pub(super) async fn stop(&mut self, room: &Room, audio: &PlatformAudio) {
        self.device = None;
        self.failed = false;
        self.starting.take();
        let published = self.published.take();
        let sid = self.current_publication(room).or(published);
        if let Some(track) = self.track.take() {
            track.mute();
            stop_recording(audio);
        }
        if let Some(sid) = sid {
            let _ = room.local_participant().unpublish_track(&sid).await;
        }
    }

    pub(super) async fn complete(
        &mut self,
        room: &Room,
        audio: &PlatformAudio,
        published: Option<TrackSid>,
    ) {
        if self.starting.take().is_none() {
            if let Some(sid) = published {
                let _ = room.local_participant().unpublish_track(&sid).await;
            }
            return;
        }
        self.published = published.and_then(|_| self.current_publication(room));
        if self.published.is_some() {
            if let Some(track) = &self.track {
                track.unmute();
            }
        } else {
            self.track = None;
            self.failed = true;
            stop_recording(audio);
        }
    }
}

pub(super) fn stop_recording(audio: &PlatformAudio) {
    let _ = audio.stop_recording();
    LkRuntime::instance()
        .pc_factory()
        .set_adm_recording_enabled(false);
}

async fn publish_microphone(
    participant: LocalParticipant,
    audio: PlatformAudio,
    device: Device,
    track: LocalAudioTrack,
    mut cancelled: oneshot::Receiver<()>,
) -> Option<TrackSid> {
    #[cfg(target_os = "macos")]
    tokio::select! {
        biased;
        _ = &mut cancelled => return None,
        granted = microphone_permission() => if !granted { return None; },
    }
    if !matches!(
        cancelled.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ) || !select_microphone(&audio, &device)
    {
        return None;
    }
    LkRuntime::instance()
        .pc_factory()
        .set_adm_recording_enabled(true);
    #[cfg(not(windows))]
    audio.start_recording().ok()?;

    let publication = participant
        .publish_track(
            LocalTrack::Audio(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .ok()?;

    // WASAPI capture starts through LiveKit's track negotiation on Windows.
    #[cfg(windows)]
    if !tokio::select! {
        biased;
        _ = &mut cancelled => false,
        captured = windows_audio::wait_for_capture(&track) => captured,
    } {
        stop_recording(&audio);
        let _ = participant.unpublish_track(&publication.sid()).await;
        return None;
    }
    Some(publication.sid())
}

#[cfg(target_os = "macos")]
async fn microphone_permission() -> bool {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};

    let (send, receive) = oneshot::channel();
    {
        let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
            return false;
        };
        let send = std::sync::Mutex::new(Some(send));
        let completion = RcBlock::new(move |granted: Bool| {
            if let Some(send) = send
                .lock()
                .expect("microphone permission callback mutex")
                .take()
            {
                let _ = send.send(granted.as_bool());
            }
        });
        // AVFoundation copies the block and invokes it once with the user's permission.
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &completion) };
    }
    receive.await.unwrap_or(false)
}
