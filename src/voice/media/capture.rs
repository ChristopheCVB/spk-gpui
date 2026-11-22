use std::sync::Arc;

use livekit::prelude::*;
use livekit::rtc_engine::lk_runtime::LkRuntime;
use livekit::webrtc::audio_source::{RtcAudioSource, native::NativeAudioSource};
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use tokio::{
    sync::{broadcast, oneshot},
    task::JoinSet,
};

use crate::voice::{Device, VoiceProcessing};

use super::processing::{ProcessedInput, RenderBlock};

#[derive(Default)]
pub(super) struct Capture {
    device: Option<Device>,
    track: Option<LocalAudioTrack>,
    published: Option<TrackSid>,
    starting: Option<oneshot::Sender<()>>,
    jobs: JoinSet<Option<TrackSid>>,
    failed: bool,
    input: Option<ProcessedInput>,
}

impl Capture {
    pub(super) fn selected_device(&self) -> Option<&Device> {
        self.device.as_ref()
    }

    pub(super) fn active_device(&self) -> Option<&Device> {
        self.published.as_ref().and(self.selected_device())
    }

    pub(super) fn can_start(&self) -> bool {
        self.device.is_none() && self.jobs.is_empty()
    }

    pub(super) fn unavailable(&self) -> bool {
        self.failed || self.can_start()
    }

    pub(super) fn has_publication(&self) -> bool {
        self.published.is_some()
    }

    pub(super) fn owns_track(&self, sid: &TrackSid) -> bool {
        self.track.as_ref().is_some_and(|track| track.sid() == *sid)
    }

    pub(super) fn resume(&self) {
        if self.has_publication()
            && let Some(track) = &self.track
        {
            if let Some(input) = &self.input {
                input.discard_buffer();
            }
            track.unmute();
        }
    }

    pub(super) async fn shutdown(&mut self) {
        self.silence();
        if let Some(input) = self.input.take() {
            input.stop().await;
        }
        self.jobs.shutdown().await;
    }

    pub(super) async fn next_publication(&mut self) -> Option<Option<TrackSid>> {
        self.jobs
            .join_next()
            .await
            .map(|result| result.ok().flatten())
    }

    pub(super) fn silence(&self) {
        if let Some(track) = &self.track {
            track.mute();
        }
        if let Some(input) = &self.input {
            input.discard_buffer();
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

    pub(super) async fn reconcile_publication(&mut self, room: &Room) {
        if self.published.is_some() {
            if let Some(sid) = self.current_publication(room) {
                self.published = Some(sid);
            } else {
                self.stop(room).await;
            }
        }
    }

    pub(super) fn start(
        &mut self,
        room: &Room,
        device: Device,
        processing: VoiceProcessing,
        use_system_default: bool,
        render: broadcast::Receiver<Arc<RenderBlock>>,
    ) {
        let (starting, cancelled) = oneshot::channel();
        let source = NativeAudioSource::new(Default::default(), super::processing::RATE, 1, 30);
        let track = LocalAudioTrack::create_audio_track(
            "microphone",
            RtcAudioSource::Native(source.clone()),
        );
        track.mute();
        let selected = (!use_system_default).then(|| device.clone());
        let (input, opened) =
            ProcessedInput::start(selected, processing, source, track.clone(), render);
        self.input = Some(input);
        self.track = Some(track.clone());
        self.device = Some(device);
        self.starting = Some(starting);
        self.jobs.spawn(publish_microphone(
            room.local_participant(),
            track,
            cancelled,
            opened,
        ));
    }

    pub(super) async fn stop(&mut self, room: &Room) {
        self.device = None;
        self.failed = false;
        self.starting.take();
        let published = self.published.take();
        let sid = self.current_publication(room).or(published);
        self.silence();
        if let Some(input) = self.input.take() {
            input.stop().await;
        }
        self.track = None;
        if let Some(sid) = sid {
            let _ = room.local_participant().unpublish_track(&sid).await;
        }
    }

    pub(super) async fn complete(&mut self, room: &Room, published: Option<TrackSid>) {
        if self.starting.take().is_none() {
            if let Some(sid) = published {
                let _ = room.local_participant().unpublish_track(&sid).await;
            }
            return;
        }
        self.published = published.and_then(|_| self.current_publication(room));
        if self.has_publication() {
            self.resume();
        } else {
            let device = self.device.clone();
            self.fail(room, device).await;
        }
    }

    pub(super) async fn sync_processing(&mut self, room: &Room, processing: VoiceProcessing) {
        if let Some(input) = &self.input {
            if input.ended() {
                let device = self.device.clone();
                self.fail(room, device).await;
            } else {
                input.update(processing);
            }
        }
    }

    async fn fail(&mut self, room: &Room, device: Option<Device>) {
        self.stop(room).await;
        self.device = device;
        self.failed = true;
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
    track: LocalAudioTrack,
    mut cancelled: oneshot::Receiver<()>,
    opened: oneshot::Receiver<()>,
) -> Option<TrackSid> {
    tokio::select! {
        biased;
        _ = &mut cancelled => return None,
        ready = opened => ready.ok()?,
    }
    let publication = participant
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .ok()?;

    Some(publication.sid())
}

#[cfg(target_os = "macos")]
pub(crate) async fn microphone_permission() -> bool {
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
