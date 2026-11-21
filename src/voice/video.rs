use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use futures::StreamExt;
use gpui_kit::RenderImage;
use image::{Frame, RgbaImage, imageops};
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    webrtc::{
        video_frame::{I420Buffer, VideoBuffer, VideoFormatType, VideoFrame, VideoRotation},
        video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
        video_stream::native::NativeVideoStream,
    },
};
use tokio::task::JoinHandle;

use super::{Update, Worker, wait_until};
use crate::api::VideoPresence;

mod camera;
pub use camera::{CameraDevice, CameraResolution, run_camera_command};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CameraIntent {
    pub enabled: bool,
    pub device: Option<String>,
    pub resolution: CameraResolution,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CameraStatus {
    #[default]
    Off,
    Starting,
    Live(u32, u32),
    Failed(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VideoSource {
    Camera,
    Screen,
}

impl VideoSource {
    fn from_track(source: TrackSource) -> Option<Self> {
        match source {
            TrackSource::Camera => Some(Self::Camera),
            TrackSource::Screenshare => Some(Self::Screen),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
pub struct VideoFrameSlot(Arc<Mutex<Option<Arc<RenderImage>>>>);

impl VideoFrameSlot {
    pub fn latest(&self) -> Option<Arc<RenderImage>> {
        self.0.lock().ok()?.clone()
    }

    fn update(&self, frame: &VideoFrame<impl AsRef<dyn VideoBuffer>>, mirror: bool) {
        let buffer = frame.buffer.as_ref();
        let (width, height) = (buffer.width(), buffer.height());
        let Some(mut image) =
            RgbaImage::from_raw(width, height, vec![0; width as usize * height as usize * 4])
        else {
            return;
        };
        buffer.to_argb(
            VideoFormatType::ARGB,
            image.as_mut(),
            width * 4,
            width as i32,
            height as i32,
        );
        let mut image = match frame.rotation {
            VideoRotation::VideoRotation0 => image,
            VideoRotation::VideoRotation90 => imageops::rotate90(&image),
            VideoRotation::VideoRotation180 => imageops::rotate180(&image),
            VideoRotation::VideoRotation270 => imageops::rotate270(&image),
        };
        if mirror {
            imageops::flip_horizontal_in_place(&mut image);
        }
        if let Ok(mut latest) = self.0.lock() {
            *latest = Some(Arc::new(RenderImage::new(vec![Frame::new(image)])));
        }
    }
}

#[derive(Clone, Default)]
pub struct ParticipantVideo {
    pub live: HashSet<VideoSource>,
    pub frames: HashMap<VideoSource, VideoFrameSlot>,
}

#[derive(Clone, Default)]
pub struct VideoReport {
    pub devices: Arc<Vec<CameraDevice>>,
    pub camera: CameraStatus,
    pub watching: HashSet<String>,
    pub participants: HashMap<String, ParticipantVideo>,
}

#[derive(Default)]
pub(super) struct Video {
    capture: Option<camera::Capture>,
    publication: Option<CameraPublication>,
    report: VideoReport,
    remote: HashMap<TrackSid, RemoteVideo>,
}

struct CameraPublication {
    source: NativeVideoSource,
    track: LocalVideoTrack,
    started: tokio::sync::oneshot::Receiver<Result<(), ()>>,
    stop: tokio::sync::oneshot::Sender<()>,
}

struct RemoteVideo {
    frames: VideoFrameSlot,
    task: JoinHandle<()>,
}

impl Drop for RemoteVideo {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Video {
    pub(super) async fn run(&mut self, worker: &Worker, room: &Room) {
        let mut events = room.subscribe();
        let mut requested = worker.requested.clone();
        let mut camera = CameraIntent::default();
        let mut watching = HashSet::new();
        let mut departed = HashSet::new();
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(3));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut reconnecting = false;
        let mut deadline = None;
        let mut sync = true;

        loop {
            let intent = requested.borrow_and_update().clone();
            departed.retain(|id| intent.watching.contains(id));
            if watching != intent.watching {
                watching = intent.watching;
                sync = true;
            }
            if camera != intent.camera {
                self.stop_camera().await;
                self.report.camera = CameraStatus::Off;
                camera = intent.camera;
                if camera.enabled {
                    match camera::Capture::start(&camera) {
                        Ok(capture) => {
                            self.capture = Some(capture);
                            self.report.camera = CameraStatus::Starting;
                            deadline = Some(tokio::time::Instant::now() + camera::START_TIMEOUT);
                        }
                        Err(message) => self.report.camera = CameraStatus::Failed(message),
                    }
                } else {
                    deadline = None;
                }
                sync = true;
            }

            if sync {
                sync = false;
                self.report.watching = watching.difference(&departed).cloned().collect();
                self.sync_tracks(room, reconnecting);
                let mut watching: Vec<_> = self.report.watching.iter().cloned().collect();
                watching.sort_unstable();
                worker.publish_video_presence(VideoPresence {
                    cam: matches!(self.report.camera, CameraStatus::Live(..)),
                    watching,
                    ..VideoPresence::default()
                });
                worker.update(Update::Video(Arc::new(self.report.clone())));
            }

            tokio::select! {
                biased;
                result = requested.changed() => if result.is_err() { return; },
                event = events.recv() => {
                    match event {
                        None | Some(RoomEvent::Disconnected { .. }) => return,
                        Some(RoomEvent::Reconnecting) => reconnecting = true,
                        Some(RoomEvent::Reconnected) => {
                            reconnecting = false;
                            if matches!(self.report.camera, CameraStatus::Live(..))
                                && self.publication.as_ref().is_some_and(|camera| {
                                    !room.local_participant().track_publications().contains_key(&camera.track.sid())
                                })
                            {
                                self.stop_camera().await;
                                self.report.camera = CameraStatus::Failed("Could not restore Camera video. Start Camera to try again.");
                                deadline = None;
                            }
                        }
                        Some(RoomEvent::LocalTrackRepublished { publication, .. })
                            if publication.source() == TrackSource::Camera
                                && self.publication.as_ref().is_none_or(|cam| cam.track.sid() != publication.sid()) =>
                        {
                            let _ = room.local_participant().unpublish_track(&publication.sid()).await;
                        }
                        Some(RoomEvent::ParticipantDisconnected(participant)) if !reconnecting => {
                            let id = participant.identity().0;
                            if watching.contains(&id) {
                                departed.insert(id);
                            }
                        }
                        Some(RoomEvent::TrackPublished { .. } | RoomEvent::TrackUnpublished { .. }
                            | RoomEvent::TrackMuted { .. } | RoomEvent::TrackUnmuted { .. }
                            | RoomEvent::TrackSubscribed { .. } | RoomEvent::TrackUnsubscribed { .. }
                            | RoomEvent::ParticipantConnected(_) | RoomEvent::Connected { .. }) => {}
                        _ => continue,
                    }
                    sync = true;
                },
                _ = refresh.tick() => {
                    let devices = tokio::task::spawn_blocking(camera::devices).await.unwrap_or_default();
                    if *self.report.devices != devices {
                        self.report.devices = Arc::new(devices);
                        sync = true;
                    }
                },
                frame = camera::next_frame(&mut self.capture), if self.capture.is_some() => {
                    if let Some(frame) = frame {
                        deadline = Some(tokio::time::Instant::now() + camera::FRAME_TIMEOUT);
                        if self.publication.is_none() {
                            self.publish_camera(room, &frame);
                            sync = true;
                        }
                        if let Some(camera) = &self.publication {
                            camera.source.capture_frame(&frame);
                        }
                        self.report.participants
                            .entry(room.local_participant().identity().0)
                            .or_default().frames.entry(VideoSource::Camera).or_default().update(&frame, true);
                    } else {
                        let message = self.capture.as_mut().expect("camera capture exists").failure().await;
                        self.stop_camera().await;
                        self.report.camera = CameraStatus::Failed(message);
                        deadline = None;
                        sync = true;
                    }
                },
                result = wait_publication(&mut self.publication), if self.report.camera == CameraStatus::Starting && self.publication.is_some() => {
                    self.report.camera = match result {
                        Ok(()) => {
                            let format = self.publication.as_ref().expect("camera publication exists").source.video_resolution();
                            CameraStatus::Live(format.width, format.height)
                        }
                        Err(()) => {
                            self.stop_camera().await;
                            deadline = None;
                            CameraStatus::Failed("Could not publish Camera video.")
                        }
                    };
                    sync = true;
                },
                _ = wait_until(deadline) => {
                    self.stop_camera().await;
                    self.report.camera = CameraStatus::Failed("Camera stopped delivering video. Check the device and try again.");
                    deadline = None;
                    sync = true;
                },
            }
        }
    }

    fn publish_camera(&mut self, room: &Room, frame: &VideoFrame<I420Buffer>) {
        let publisher = room.local_participant();
        let source = NativeVideoSource::new(
            VideoResolution {
                width: frame.buffer.width(),
                height: frame.buffer.height(),
            },
            false,
        );
        let track =
            LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(source.clone()));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready, started) = tokio::sync::oneshot::channel();
        tokio::spawn({
            let track = track.clone();
            async move {
                let result = publisher
                    .publish_track(
                        LocalTrack::Video(track.clone()),
                        TrackPublishOptions {
                            source: TrackSource::Camera,
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = ready.send(result.as_ref().map(|_| ()).map_err(|_| ()));
                if result.is_ok() {
                    let _ = stopped.await;
                    let _ = publisher.unpublish_track(&track.sid()).await;
                }
            }
        });
        self.publication = Some(CameraPublication {
            source,
            track,
            started,
            stop,
        });
    }

    pub(super) async fn stop_camera(&mut self) {
        if let Some(publication) = self.publication.take() {
            publication.track.mute();
            let _ = publication.stop.send(());
        }
        if let Some(capture) = self.capture.take() {
            capture.stop().await;
        }
    }

    fn sync_tracks(&mut self, room: &Room, reconnecting: bool) {
        let me = room.local_participant().identity().0;
        self.report.participants.retain(|id, _| *id == me);
        let local = self.report.participants.entry(me).or_default();
        if matches!(self.report.camera, CameraStatus::Live(..)) {
            local.live.insert(VideoSource::Camera);
        } else if self.capture.is_none() {
            local.live.remove(&VideoSource::Camera);
            local.frames.remove(&VideoSource::Camera);
        }

        let mut active = HashSet::new();
        for participant in room.remote_participants().values() {
            let id = participant.identity().0;
            let wanted = self.report.watching.contains(&id);
            let video = self.report.participants.entry(id).or_default();
            for publication in participant.track_publications().values() {
                let Some(source) = VideoSource::from_track(publication.source()) else {
                    continue;
                };
                if publication.is_desired() != wanted {
                    publication.set_subscribed(wanted);
                }
                if publication.is_muted() {
                    continue;
                }
                video.live.insert(source);
                if !wanted || reconnecting {
                    continue;
                }
                let Some(RemoteTrack::Video(track)) = publication.track() else {
                    continue;
                };
                let sid = publication.sid();
                active.insert(sid.clone());
                let remote = self.remote.entry(sid).or_insert_with(|| {
                    let frames = VideoFrameSlot::default();
                    let mut stream = NativeVideoStream::new(track.rtc_track());
                    let task = tokio::spawn({
                        let frames = frames.clone();
                        async move {
                            while let Some(frame) = stream.next().await {
                                frames.update(&frame, false);
                            }
                        }
                    });
                    RemoteVideo { frames, task }
                });
                video.frames.insert(source, remote.frames.clone());
            }
        }
        self.remote.retain(|sid, _| active.contains(sid));
    }
}

async fn wait_publication(publication: &mut Option<CameraPublication>) -> Result<(), ()> {
    (&mut publication.as_mut().ok_or(())?.started)
        .await
        .unwrap_or(Err(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a loopback LiveKit server and SPK_TEST_PUBLISHER/SPK_TEST_RECEIVER tokens"]
    async fn livekit_camera_only_delivers_frames_while_watching() {
        watch_video(VideoSource::Camera, TrackSource::Camera).await;
    }

    #[tokio::test]
    #[ignore = "requires a loopback LiveKit server and SPK_TEST_PUBLISHER/SPK_TEST_RECEIVER tokens"]
    async fn livekit_screen_only_delivers_frames_while_watching() {
        watch_video(VideoSource::Screen, TrackSource::Screenshare).await;
    }

    async fn watch_video(video_source: VideoSource, track_source: TrackSource) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let endpoint =
            std::env::var("SPK_TEST_LIVEKIT").unwrap_or_else(|_| "ws://127.0.0.1:17880".into());
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        let (publisher, _) = Room::connect(
            &endpoint,
            &std::env::var("SPK_TEST_PUBLISHER").unwrap(),
            options.clone(),
        )
        .await
        .unwrap();
        let (receiver, mut events) = Room::connect(
            &endpoint,
            &std::env::var("SPK_TEST_RECEIVER").unwrap(),
            options,
        )
        .await
        .unwrap();
        let presenter = publisher.local_participant().identity().0;
        let mut local = Video::default();
        let mut remote = Video::default();
        let frame = VideoFrame::new(
            VideoRotation::VideoRotation0,
            I420Buffer::new_black(320, 180),
        );
        let (capture, share) = if video_source == VideoSource::Camera {
            local.publish_camera(&publisher, &frame);
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                wait_publication(&mut local.publication),
            )
            .await
            .unwrap()
            .unwrap();
            (local.publication.as_ref().unwrap().source.clone(), None)
        } else {
            let capture = NativeVideoSource::new(
                VideoResolution {
                    width: 320,
                    height: 180,
                },
                true,
            );
            let track = LocalVideoTrack::create_video_track(
                "spk:screen:180:30",
                RtcVideoSource::Native(capture.clone()),
            );
            let publication = publisher
                .local_participant()
                .publish_track(
                    LocalTrack::Video(track),
                    TrackPublishOptions {
                        source: track_source,
                        simulcast: false,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            (capture, Some(publication))
        };

        let publication = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(RoomEvent::TrackPublished { publication, .. }) = events.recv().await {
                    break publication;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(publication.source(), track_source);
        remote.sync_tracks(&receiver, false);
        let remote_tile = &remote.report.participants[&presenter];
        assert!(remote_tile.live.contains(&video_source));
        assert!(remote_tile.frames.is_empty());
        assert!(!publication.is_desired());

        remote.report.watching.insert(presenter.clone());
        remote.sync_tracks(&receiver, false);
        assert!(publication.is_desired());
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                capture.capture_frame(&frame);
                remote.sync_tracks(&receiver, false);
                if remote.report.participants[&presenter]
                    .frames
                    .get(&video_source)
                    .and_then(VideoFrameSlot::latest)
                    .is_some_and(|image| image.as_bytes(0).is_some_and(|pixels| !pixels.is_empty()))
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        })
        .await
        .unwrap();

        remote.report.watching.clear();
        remote.sync_tracks(&receiver, false);
        assert!(!publication.is_desired());
        assert!(remote.report.participants[&presenter].frames.is_empty());
        if let Some(share) = share {
            publisher
                .local_participant()
                .unpublish_track(&share.sid())
                .await
                .unwrap();
        } else {
            local.stop_camera().await;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(RoomEvent::TrackUnpublished { publication, .. }) = events.recv().await
                    && publication.source() == track_source
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        receiver.close().await.unwrap();
        publisher.close().await.unwrap();
    }
}
