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

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as platform;

#[cfg(not(any(target_os = "macos", windows)))]
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

    fn name(self) -> String {
        format!("spk:screen:{}:{}", self.height, self.fps)
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
        quality: Quality,
        audio: bool,
        sink: CaptureSink,
    ) -> BoxFuture<'static, Result<(), CaptureError>>;
}

#[derive(Clone)]
#[allow(dead_code)] // Native adapters send frames through these bounded queues.
pub struct CaptureSink {
    video: mpsc::Sender<(VideoFrame<I420Buffer>, u32)>,
    audio: Option<mpsc::Sender<Vec<i16>>>,
    audio_unavailable: watch::Sender<bool>,
}

#[allow(dead_code)]
impl CaptureSink {
    pub fn video(&self, frame: VideoFrame<I420Buffer>, fps: u32) {
        let _ = self.video.try_send((frame, fps));
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
        if self.attempt != attempt
            || !matches!(
                (&self.action, &action),
                (_, Action::Stop)
                    | (Action::Open, Action::Choose(..))
                    | (Action::Choose(..), Action::Start(_))
            )
        {
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
            let action = wait_for_action(&mut requested, next.attempt, |action| {
                matches!(action, Action::Choose(..))
            })
            .await?;
            let Action::Choose(id, playback) = action.action else {
                unreachable!()
            };
            let choice = choices
                .into_iter()
                .find(|choice| choice.id == id)
                .ok_or(CaptureError::Unavailable)?;
            report(State::Picking);
            let selection = platform::select(choice, playback).await?;
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
            capture_and_publish(room, request.quality, scope, selection.capture, &report).await
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
    scope: Option<AudioScope>,
    capture: Box<dyn Capture>,
    report: &impl Fn(State),
) -> Result<(), CaptureError> {
    let (video, mut video_frames) = mpsc::channel(1);
    let (audio, mut audio_samples) = mpsc::channel(4);
    let (audio_unavailable, mut audio_status) = watch::channel(false);
    let capture = capture.start(
        quality,
        scope.is_some(),
        CaptureSink {
            video,
            audio: scope.is_some().then_some(audio),
            audio_unavailable,
        },
    );
    tokio::select! {
        biased;
        result = capture => result,
        result = async {
            let (frame, mut fps) = tokio::time::timeout(Duration::from_secs(15), video_frames.recv()).await
                .ok().flatten().ok_or(CaptureError::Unavailable)?;
            let (mut width, mut height) = (frame.buffer.width(), frame.buffer.height());
            let source = NativeVideoSource::new(VideoResolution { width, height }, true);
            let video = LocalVideoTrack::create_video_track(&quality.name(), RtcVideoSource::Native(source.clone()));
            let (video, started) = Publication::new(room, LocalTrack::Video(video), TrackPublishOptions {
                video_encoding: Some(VideoEncoding {
                    max_bitrate: (f64::from(width * height) * 0.458 * f64::from(fps).powf(0.674) / 100_000.0).round() as u64 * 100_000,
                    max_framerate: f64::from(fps),
                }),
                source: TrackSource::Screenshare,
                stream: "screen".into(),
                simulcast: false,
                degradation_preference: Some(if fps == 5 { DegradationPreference::MaintainResolution } else { DegradationPreference::MaintainFramerate }),
                ..Default::default()
            });
            if !started.await.unwrap_or(false) {
                return Err(CaptureError::Publication);
            }
            let frames = VideoFrameSlot::default();
            source.capture_frame(&frame);
            frames.update(&frame, false);
            let mut audio_available = scope.is_some() && !*audio_status.borrow_and_update();
            report(State::Live {
                width, height, fps, audio: scope.filter(|_| audio_available),
                audio_unavailable: scope.is_some() && !audio_available, frames: frames.clone(),
            });

            let mut audio: Option<(Publication, NativeAudioSource)> = None;
            let mut events = room.subscribe();
            loop {
                tokio::select! {
                    biased;
                    result = audio_status.changed(), if audio_available => {
                        if result.is_err() || *audio_status.borrow_and_update() {
                            audio_available = false;
                            audio = None;
                            report(State::Live { width, height, fps, audio: None, audio_unavailable: true, frames: frames.clone() });
                        }
                    }
                    event = events.recv() => match event {
                        Some(RoomEvent::Reconnected) if !video.in_room(room) => return Err(CaptureError::Publication),
                        Some(RoomEvent::Reconnected) if audio.as_ref().is_some_and(|(track, _)| !track.in_room(room)) => {
                            audio_available = false;
                            audio = None;
                            report(State::Live { width, height, fps, audio: None, audio_unavailable: true, frames: frames.clone() });
                        }
                        Some(RoomEvent::Disconnected { .. }) | None => return Err(CaptureError::Publication),
                        _ => {}
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
                                report(State::Live { width, height, fps, audio: None, audio_unavailable: true, frames: frames.clone() });
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
                    }
                    Some((frame, active_fps)) = video_frames.recv() => {
                        source.capture_frame(&frame);
                        frames.update(&frame, false);
                        let (active_width, active_height) = (frame.buffer.width(), frame.buffer.height());
                        if (width, height, fps) != (active_width, active_height, active_fps) {
                            (width, height, fps) = (active_width, active_height, active_fps);
                            report(State::Live {
                                width, height, fps, audio: scope.filter(|_| audio_available),
                                audio_unavailable: scope.is_some() && !audio_available, frames: frames.clone(),
                            });
                        }
                    }
                }
            }
        } => result,
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
