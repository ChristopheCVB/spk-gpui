use std::{borrow::Cow, future::Future, time::Duration};

use futures::future::BoxFuture;
use livekit::{
    options::{AudioEncoding, DegradationPreference, TrackPublishOptions, VideoEncoding},
    prelude::*,
    webrtc::{
        audio_frame::AudioFrame,
        audio_source::{AudioSourceOptions, RtcAudioSource, native::NativeAudioSource},
        video_frame::{I420Buffer, VideoBuffer, VideoFrame},
        video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
    },
};
use tokio::sync::{mpsc, oneshot, watch};

use super::{Device, Intent, Update, Worker, video::VideoFrameSlot};

const QUALITY_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as platform;
#[cfg(windows)]
pub(crate) use self::windows::{SYSTEM_PICKER, pick_system_source};

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::*;

    pub async fn choices() -> Result<Vec<Choice>, CaptureError> {
        Ok(Vec::new())
    }

    pub async fn select(_: Choice, _: Option<Device>) -> Result<Selection, CaptureError> {
        Err(CaptureError::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Quality {
    pub height: u32,
    pub fps: u32,
}

impl Default for Quality {
    fn default() -> Self {
        Self {
            height: 1080,
            fps: 30,
        }
    }
}

impl Quality {
    pub const HEIGHTS: [u32; 3] = [720, 1080, 1440];
    pub const FPS: [u32; 3] = [5, 30, 60];

    #[allow(dead_code)] // Used by native capture adapters.
    pub fn dimensions(self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width < 2 || height < 2 {
            return None;
        }
        let max_width = f64::from((self.height * 16 + 4) / 9);
        let ratio = (max_width / f64::from(width))
            .min(f64::from(self.height) / f64::from(height))
            .min(1.0);
        let width = (f64::from(width) * ratio) as u32 & !1;
        let height = (f64::from(height) * ratio) as u32 & !1;
        (width >= 2 && height >= 2).then_some((width, height))
    }

    fn publication_name(width: u32, height: u32, fps: u32) -> String {
        if Self::HEIGHTS.contains(&height)
            && Self::FPS.contains(&fps)
            && width == (height * 16 + 4) / 9
        {
            format!("spk:screen:{height}:{fps}")
        } else {
            "screen".into()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

#[allow(dead_code)] // Constructed by native capture adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioScope {
    PlaybackMix,
    Application,
}

impl AudioScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::PlaybackMix => "Display playback mix",
            Self::Application => "Audio from the owning application, including its other windows",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Native adapters report platform-specific errors.
pub enum CaptureError {
    Cancelled,
    Permission,
    Unavailable,
    SourceLost,
    #[cfg(target_os = "linux")]
    PortalSelection,
    Publication,
}

impl CaptureError {
    fn message(self) -> &'static str {
        match self {
            Self::Cancelled => "Screen Share selection cancelled.",
            Self::Permission => {
                "Allow screen capture in system settings, then select a source again."
            }
            Self::Unavailable => {
                "Screen Share is unavailable. Check capture permissions and try again."
            }
            #[cfg(target_os = "linux")]
            Self::PortalSelection => {
                "The Wayland picker did not provide a source. Check Screen Share permissions and try again."
            }
            Self::SourceLost => "The Screen Share source stopped. Select a source to share again.",
            Self::Publication => "Could not publish Screen Share. Select a source to try again.",
        }
    }
}

pub struct Selection {
    pub label: String,
    pub audio: Option<AudioScope>,
    pub capture: Box<dyn Capture>,
}

pub trait Capture: Send {
    fn start(
        self: Box<Self>,
        audio: bool,
        sink: CaptureSink,
    ) -> BoxFuture<'static, Result<(), CaptureError>>;
}

#[derive(Clone)]
#[allow(dead_code)] // Native adapters send frames through these bounded queues.
pub struct CaptureSink {
    video: mpsc::Sender<(VideoFrame<I420Buffer>, Quality, u32)>,
    quality: watch::Receiver<Quality>,
    audio: Option<mpsc::Sender<Vec<i16>>>,
    audio_unavailable: watch::Sender<bool>,
}

#[allow(dead_code)]
impl CaptureSink {
    pub fn video(&self, frame: VideoFrame<I420Buffer>, quality: Quality, fps: u32) {
        // Callbacks can arrive after a preset change, so carry the settings that produced the frame.
        let _ = self.video.try_send((frame, quality, fps));
    }

    pub fn audio(&self, samples: Vec<i16>) {
        if let Some(sender) = &self.audio
            && !samples.is_empty()
            && samples.len().is_multiple_of(2)
        {
            let _ = sender.try_send(samples);
        }
    }

    pub fn audio_unavailable(&self) {
        self.audio_unavailable.send_replace(true);
    }

    #[cfg(test)]
    fn test_pair(with_audio: bool) -> (Self, mpsc::Receiver<Vec<i16>>) {
        let (video, _) = mpsc::channel(1);
        let (audio, receiver) = mpsc::channel(1);
        (
            Self {
                video,
                quality: watch::channel(Quality::default()).1,
                audio: with_audio.then_some(audio),
                audio_unavailable: watch::Sender::new(false),
            },
            receiver,
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Action {
    #[default]
    Stop,
    Open,
    Choose(String, Option<Device>),
    #[cfg(windows)]
    ChooseSystem(Result<windows_wgc::Graphics::Capture::GraphicsCaptureItem, CaptureError>),
    Start(bool),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Request {
    pub attempt: u64,
    pub action: Action,
    pub quality: Quality,
}

impl Request {
    pub fn open(&mut self) {
        self.attempt += 1;
        self.action = Action::Open;
    }

    pub fn advance(&mut self, attempt: u64, action: Action) -> bool {
        let valid = match (&self.action, &action) {
            (_, Action::Stop)
            | (Action::Open, Action::Choose(..))
            | (Action::Choose(..), Action::Start(_)) => true,
            #[cfg(windows)]
            (Action::Open, Action::ChooseSystem(..))
            | (Action::ChooseSystem(..), Action::Start(_)) => true,
            _ => false,
        };
        if self.attempt != attempt || !valid {
            return false;
        }
        self.action = action;
        true
    }
}

#[derive(Clone)]
pub struct Report {
    pub attempt: u64,
    pub state: State,
}

#[derive(Clone)]
pub enum State {
    Picking,
    Choices(Vec<Choice>),
    Confirm {
        label: String,
        audio: Option<AudioScope>,
    },
    Starting,
    Live {
        width: u32,
        height: u32,
        fps: u32,
        quality: Quality,
        quality_failed: Option<Quality>,
        audio: Option<AudioScope>,
        audio_unavailable: bool,
        frames: VideoFrameSlot,
    },
    Ended(&'static str),
}

impl State {
    pub fn active(&self) -> bool {
        matches!(self, Self::Live { .. })
    }
}

pub(super) async fn run(worker: &Worker, room: &Room) {
    let mut requested = worker.requested.clone();
    let mut previous = 0;
    while let Ok(next) = requested
        .wait_for(|intent| {
            intent.screen.attempt != previous && intent.screen.action == Action::Open
        })
        .await
        .map(|intent| intent.screen.clone())
    {
        previous = next.attempt;
        let report = |state: State| {
            worker.publish_screen_presence(state.active());
            worker.update(Update::Screen(Report {
                attempt: next.attempt,
                state,
            }));
        };
        report(State::Picking);
        let result = select_until_cancelled(&mut requested.clone(), next.attempt, async {
            let choices = platform::choices().await?;
            if choices.is_empty() {
                return Err(CaptureError::Unavailable);
            }
            report(State::Choices(choices.clone()));
            let action = wait_for_action(&mut requested, next.attempt, |action| match action {
                Action::Choose(..) => true,
                #[cfg(windows)]
                Action::ChooseSystem(..) => true,
                _ => false,
            })
            .await?;
            report(State::Picking);
            let selection = match action.action {
                Action::Choose(id, playback) => {
                    let choice = choices
                        .into_iter()
                        .find(|choice| choice.id == id)
                        .ok_or(CaptureError::Unavailable)?;
                    platform::select(choice, playback).await?
                }
                #[cfg(windows)]
                Action::ChooseSystem(item) => platform::select_system(item?).await?,
                _ => unreachable!(),
            };
            report(State::Confirm {
                label: selection.label,
                audio: selection.audio,
            });
            let request = wait_for_action(&mut requested, next.attempt, |action| {
                matches!(action, Action::Start(_))
            })
            .await?;
            let Action::Start(wants_audio) = request.action else {
                unreachable!()
            };
            let scope = selection.audio.filter(|_| wants_audio);
            report(State::Starting);
            capture_and_publish(
                room,
                request.quality,
                &mut requested,
                scope,
                selection.capture,
                &report,
            )
            .await
        })
        .await;
        let error = result.err().unwrap_or(CaptureError::SourceLost);
        let active = requested.borrow().screen.clone();
        worker.publish_screen_presence(false);
        if active.attempt == next.attempt && active.action != Action::Stop {
            report(State::Ended(error.message()));
        }
    }
}

async fn select_until_cancelled<T>(
    requested: &mut watch::Receiver<Intent>,
    attempt: u64,
    work: impl Future<Output = Result<T, CaptureError>>,
) -> Result<T, CaptureError> {
    let cancelled =
        |intent: &Intent| intent.screen.attempt != attempt || intent.screen.action == Action::Stop;
    tokio::select! {
        biased;
        _ = async { let _ = requested.wait_for(cancelled).await; } => Err(CaptureError::Cancelled),
        result = work => {
            if cancelled(&requested.borrow()) {
                Err(CaptureError::Cancelled)
            } else {
                result
            }
        },
    }
}

async fn wait_for_action(
    requested: &mut watch::Receiver<Intent>,
    attempt: u64,
    predicate: impl Fn(&Action) -> bool,
) -> Result<Request, CaptureError> {
    requested
        .wait_for(|intent| intent.screen.attempt == attempt && predicate(&intent.screen.action))
        .await
        .map(|intent| intent.screen.clone())
        .map_err(|_| CaptureError::Cancelled)
}

async fn capture_and_publish(
    room: &Room,
    quality: Quality,
    requested: &mut watch::Receiver<Intent>,
    scope: Option<AudioScope>,
    capture: Box<dyn Capture>,
    report: &impl Fn(State),
) -> Result<(), CaptureError> {
    let (video, mut video_frames) = mpsc::channel(1);
    let (audio, mut audio_samples) = mpsc::channel(4);
    let (audio_unavailable, mut audio_status) = watch::channel(false);
    let (capture_quality, target) = watch::channel(quality);
    let capture = capture.start(
        scope.is_some(),
        CaptureSink {
            video,
            quality: target,
            audio: scope.is_some().then_some(audio),
            audio_unavailable,
        },
    );
    tokio::select! {
        biased;
        result = capture => result,
        result = async {
            let (frame, mut applied, fps) = tokio::time::timeout(Duration::from_secs(15), video_frames.recv()).await
                .ok().flatten().ok_or(CaptureError::Unavailable)?;
            let mut format = VideoFormat::new(&frame, fps);
            let (mut video, started) = VideoPublication::new(room, format);
            if !started.await.unwrap_or(false) {
                return Err(CaptureError::Publication);
            }
            let frames = VideoFrameSlot::default();
            video.source.capture_frame(&frame);
            frames.update(&frame, false);
            let mut audio_available = scope.is_some() && !*audio_status.borrow_and_update();
            let report_live = |format: VideoFormat, quality, quality_failed, available| {
                report(State::Live {
                    width: format.width,
                    height: format.height,
                    fps: format.fps,
                    quality,
                    quality_failed,
                    audio: scope.filter(|_| available),
                    audio_unavailable: scope.is_some() && !available,
                    frames: frames.clone(),
                });
            };
            let mut quality_failed = None;
            report_live(format, applied, quality_failed, audio_available);

            let mut audio: Option<(Publication, NativeAudioSource)> = None;
            let mut events = room.subscribe();
            let mut target = applied;
            let mut last_requested = quality;
            let mut deadline = None;
            let mut replacement: Option<(VideoPublication, VideoFormat, oneshot::Receiver<bool>)> = None;
            loop {
                // Audio can stay ready while WebRTC drains its buffer. Poll video fairly.
                let changed = tokio::select! {
                    result = requested.changed() => {
                        result.map_err(|_| CaptureError::Cancelled)?;
                        let next = requested.borrow_and_update().screen.quality;
                        if next == last_requested {
                            continue;
                        }
                        last_requested = next;
                        if next == target {
                            continue;
                        }
                        target = next;
                        capture_quality.send_replace(target);
                        deadline = (target != applied).then(|| tokio::time::Instant::now() + QUALITY_TIMEOUT);
                        replacement = None;
                        quality_failed = None;
                        true
                    }
                    () = async {
                        tokio::time::sleep_until(deadline.expect("quality deadline")).await;
                    }, if deadline.is_some() => {
                        if target == applied {
                            return Err(CaptureError::Publication);
                        }
                        quality_failed = Some(target);
                        target = applied;
                        capture_quality.send_replace(applied);
                        deadline = replacement.is_some().then(|| tokio::time::Instant::now() + QUALITY_TIMEOUT);
                        replacement = None;
                        true
                    }
                    ready = async {
                        (&mut replacement.as_mut().expect("replacement publication").2).await.unwrap_or(false)
                    }, if replacement.is_some() => {
                        let (new, new_format, _) = replacement.take().expect("replacement publication");
                        deadline = None;
                        if ready {
                            // Keep the old video until LiveKit accepts its replacement.
                            video = new;
                            format = new_format;
                            applied = target;
                        } else if target != applied {
                            quality_failed = Some(target);
                            target = applied;
                            capture_quality.send_replace(applied);
                            deadline = Some(tokio::time::Instant::now() + QUALITY_TIMEOUT);
                        } else {
                            return Err(CaptureError::Publication);
                        }
                        true
                    }
                    result = audio_status.changed(), if audio_available => {
                        if result.is_err() || *audio_status.borrow_and_update() {
                            audio_available = false;
                            audio = None;
                            true
                        } else {
                            false
                        }
                    }
                    event = events.recv() => match event {
                        Some(RoomEvent::Reconnected) if !video.publication.in_room(room) => return Err(CaptureError::Publication),
                        Some(RoomEvent::Reconnected) if audio.as_ref().is_some_and(|(track, _)| !track.in_room(room)) => {
                            audio_available = false;
                            audio = None;
                            true
                        }
                        Some(RoomEvent::Disconnected { .. }) | None => return Err(CaptureError::Publication),
                        _ => false,
                    },
                    Some(samples) = audio_samples.recv(), if audio_available => {
                        if audio.is_none() {
                            // shortcut: LiveKit 0.12 can downmix this stereo source. Use per-track stereo signaling once the SDK exposes it.
                            let source = NativeAudioSource::new(AudioSourceOptions {
                                echo_cancellation: false,
                                noise_suppression: false,
                                auto_gain_control: false,
                            }, 48_000, 2, 50);
                            let track = LocalAudioTrack::create_audio_track("screen-audio", RtcAudioSource::Native(source.clone()));
                            let (publication, started) = Publication::new(room, LocalTrack::Audio(track), TrackPublishOptions {
                                audio_encoding: Some(AudioEncoding { max_bitrate: 256_000 }),
                                source: TrackSource::ScreenshareAudio,
                                stream: "screen".into(),
                                dtx: false,
                                red: false,
                                ..Default::default()
                            });
                            if !started.await.unwrap_or(false) {
                                audio_available = false;
                                report_live(format, applied, quality_failed, audio_available);
                                continue;
                            }
                            audio = Some((publication, source));
                        }
                        if let Some((_, source)) = &audio {
                            let _ = source.capture_frame(&AudioFrame {
                                samples_per_channel: (samples.len() / 2) as u32,
                                data: Cow::Owned(samples),
                                sample_rate: 48_000,
                                num_channels: 2,
                            }).await;
                        }
                        false
                    }
                    Some((frame, captured, fps)) = video_frames.recv() => {
                        let captured_format = VideoFormat::new(&frame, fps);
                        if captured != target {
                            if captured == applied && captured_format == format {
                                video.source.capture_frame(&frame);
                                frames.update(&frame, false);
                            }
                            false
                        } else if let Some((next, expected, _)) = &replacement {
                            if *expected == captured_format {
                                next.source.capture_frame(&frame);
                            }
                            false
                        } else if format != captured_format
                            && (target != applied || format.fps != fps || format.name() != captured_format.name())
                        {
                            let (next, started) = VideoPublication::new(room, captured_format);
                            next.source.capture_frame(&frame);
                            replacement = Some((next, captured_format, started));
                            false
                        } else {
                            video.source.capture_frame(&frame);
                            frames.update(&frame, false);
                            let changed = (format, applied) != (captured_format, target);
                            (format, applied) = (captured_format, target);
                            deadline = None;
                            changed
                        }
                    }
                };
                if changed {
                    report_live(format, applied, quality_failed, audio_available);
                }
            }
        } => result,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct VideoFormat {
    width: u32,
    height: u32,
    fps: u32,
}

impl VideoFormat {
    fn new(frame: &VideoFrame<I420Buffer>, fps: u32) -> Self {
        Self {
            width: frame.buffer.width(),
            height: frame.buffer.height(),
            fps,
        }
    }

    fn name(self) -> String {
        Quality::publication_name(self.width, self.height, self.fps)
    }
}

struct VideoPublication {
    publication: Publication,
    source: NativeVideoSource,
}

impl VideoPublication {
    fn new(room: &Room, format: VideoFormat) -> (Self, oneshot::Receiver<bool>) {
        let VideoFormat { width, height, fps } = format;
        let source = NativeVideoSource::new(VideoResolution { width, height }, true);
        let video = LocalVideoTrack::create_video_track(
            &format.name(),
            RtcVideoSource::Native(source.clone()),
        );
        let (publication, started) = Publication::new(
            room,
            LocalTrack::Video(video),
            TrackPublishOptions {
                video_encoding: Some(VideoEncoding {
                    max_bitrate: (f64::from(width * height) * 0.458 * f64::from(fps).powf(0.674)
                        / 100_000.0)
                        .round() as u64
                        * 100_000,
                    max_framerate: f64::from(fps),
                }),
                source: TrackSource::Screenshare,
                stream: "screen".into(),
                simulcast: false,
                degradation_preference: Some(if fps == 5 {
                    DegradationPreference::MaintainResolution
                } else {
                    DegradationPreference::MaintainFramerate
                }),
                ..Default::default()
            },
        );
        (
            Self {
                publication,
                source,
            },
            started,
        )
    }
}

struct Publication {
    track: LocalTrack,
    stop: Option<oneshot::Sender<()>>,
}

impl Publication {
    fn new(
        room: &Room,
        track: LocalTrack,
        options: TrackPublishOptions,
    ) -> (Self, oneshot::Receiver<bool>) {
        let publisher = room.local_participant();
        let (stop, mut stopped) = oneshot::channel();
        let (ready, started) = oneshot::channel();
        tokio::spawn({
            let track = track.clone();
            async move {
                if stopped.try_recv().is_ok() {
                    return;
                }
                let published = publisher
                    .publish_track(track.clone(), options)
                    .await
                    .is_ok();
                let _ = ready.send(published);
                if published {
                    let _ = stopped.await;
                    let _ = publisher.unpublish_track(&track.sid()).await;
                }
            }
        });
        (
            Self {
                track,
                stop: Some(stop),
            },
            started,
        )
    }

    fn in_room(&self, room: &Room) -> bool {
        room.local_participant()
            .track_publications()
            .contains_key(&self.track.sid())
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.track.mute();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

#[cfg(test)]
mod tests;
