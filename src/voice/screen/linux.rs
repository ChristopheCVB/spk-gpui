use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use livekit::webrtc::{native::yuv_helper::argb_to_i420, video_frame::VideoRotation};
use tokio::time::{MissedTickBehavior, interval, timeout};
use webrtc_sys::desktop_capturer::{
    CaptureError as NativeError, DesktopCapturerCallback, DesktopCapturerCallbackWrapper,
    ffi::{
        DesktopCapturer, DesktopCapturerOptions, DesktopFrame, SourceType, new_desktop_capturer,
    },
};
use x11rb::protocol::xproto::ConnectionExt;

use super::*;

mod portal;
mod pulse;

const TYPES: [(SourceType, SourceKind, &str, &str); 2] = [
    (
        SourceType::Screen,
        SourceKind::Display,
        "display",
        "Display",
    ),
    (SourceType::Window, SourceKind::Window, "window", "Window"),
];
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn use_system_picker() -> Result<bool, CaptureError> {
    wayland()
}

pub(super) async fn pick(playback: Option<Device>) -> Result<Selection, CaptureError> {
    if !wayland()? {
        return Err(CaptureError::Unavailable);
    }
    portal::pick(playback).await
}

pub async fn choices() -> Result<Vec<Choice>, CaptureError> {
    if wayland()? {
        return Err(CaptureError::Unavailable);
    }
    prepare_x11_window_capture()?;
    let mut choices = Vec::new();
    for (source_type, kind, id, label) in TYPES {
        let capturer = new_desktop_capturer(options(source_type));
        if let Some(capturer) = capturer.as_ref() {
            choices.extend(capturer.get_source_list().into_iter().enumerate().map(
                |(index, source)| Choice {
                    kind,
                    id: format!("{id}:{}", source.id),
                    label: if source.title.trim().is_empty() {
                        format!("{label} {}", index + 1)
                    } else {
                        format!("{label} · {}", source.title)
                    },
                },
            ));
        }
    }
    Ok(choices)
}

pub async fn select(choice: Choice, playback: Option<Device>) -> Result<Selection, CaptureError> {
    if wayland()? {
        return Err(CaptureError::Unavailable);
    }
    let (source_type, id) = TYPES
        .iter()
        .find_map(|&(source_type, _, prefix, _)| {
            let (kind, id) = choice.id.split_once(':')?;
            (kind == prefix).then_some((source_type, id.parse::<u64>().ok()?))
        })
        .ok_or(CaptureError::Unavailable)?;
    let mut capturer = new_desktop_capturer(options(source_type));
    if capturer.is_null() {
        return Err(CaptureError::Unavailable);
    }
    if !capturer
        .get_source_list()
        .iter()
        .any(|source| source.id == id)
        || !capturer.select_source(id)
    {
        return Err(CaptureError::SourceLost);
    }

    let frames = Arc::new(Mutex::new(FrameStatus::default()));
    capturer
        .pin_mut()
        .start(Box::new(DesktopCapturerCallbackWrapper::new(Box::new(
            Frames(frames.clone()),
        ))));
    let video = VideoCapture {
        frames,
        backend: VideoBackend::X11(capturer),
    };
    ready_selection(
        video,
        choice.label,
        source_type == SourceType::Screen,
        playback,
    )
    .await
}

async fn ready_selection(
    mut video: VideoCapture,
    label: String,
    display: bool,
    playback: Option<Device>,
) -> Result<Selection, CaptureError> {
    timeout(FRAME_TIMEOUT, video.wait_for_source())
        .await
        .map_err(|_| CaptureError::SourceLost)??;
    let monitor = if display {
        pulse::Monitor::open(playback).await
    } else {
        None
    };
    Ok(Selection {
        label,
        audio: monitor.as_ref().map(|_| AudioScope::PlaybackMix),
        capture: Box::new(Selected { video, monitor }),
    })
}

fn prepare_x11_window_capture() -> Result<(), CaptureError> {
    let (connection, _) = x11rb::connect(None).map_err(|_| CaptureError::Unavailable)?;
    // WebRTC looks up these atoms without creating them. A bare X server may lack them.
    for name in [b"WM_STATE".as_slice(), b"_NET_WM_WINDOW_TYPE"] {
        connection
            .intern_atom(false, name)
            .map_err(|_| CaptureError::Unavailable)?
            .reply()
            .map_err(|_| CaptureError::Unavailable)?;
    }
    Ok(())
}

fn options(source_type: SourceType) -> DesktopCapturerOptions {
    DesktopCapturerOptions {
        source_type,
        include_cursor: true,
        allow_sck_system_picker: false,
    }
}

fn wayland() -> Result<bool, CaptureError> {
    let display = std::env::var_os("WAYLAND_DISPLAY");
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("x11") => Ok(false),
        Ok(session) if session.starts_with("wayland") => display
            .filter(|name| !name.is_empty())
            .map(|_| true)
            .ok_or(CaptureError::Unavailable),
        _ if display.is_some_and(|name| !name.is_empty()) => Ok(true),
        _ => Ok(false),
    }
}

struct Selected {
    video: VideoCapture,
    monitor: Option<pulse::Monitor>,
}

impl Capture for Selected {
    fn start(
        self: Box<Self>,
        audio: bool,
        sink: CaptureSink,
    ) -> BoxFuture<'static, Result<(), CaptureError>> {
        Box::pin(async move {
            let Self { mut video, monitor } = *self;
            let monitor = monitor.filter(|_| audio);
            tokio::select! {
                result = video.capture(sink.clone()) => result,
                () = async {
                    if let Some(monitor) = monitor {
                        monitor.capture(&sink).await;
                    } else if audio {
                        sink.audio_unavailable();
                    }
                    std::future::pending().await
                } => unreachable!(),
            }
        })
    }
}

enum VideoBackend {
    X11(cxx::UniquePtr<DesktopCapturer>),
    Wayland(portal::PortalVideo),
}

struct VideoCapture {
    frames: Arc<Mutex<FrameStatus>>,
    backend: VideoBackend,
}

impl VideoCapture {
    fn request_frame(&self) -> Result<(), CaptureError> {
        match &self.backend {
            VideoBackend::X11(capturer) => capturer.capture_frame(),
            VideoBackend::Wayland(portal) => portal.request_frame(&self.frames)?,
        }
        Ok(())
    }

    async fn wait_for_source(&mut self) -> Result<(), CaptureError> {
        let mut ticks = interval(Duration::from_millis(32));
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            self.request_frame()?;
            let frames = self.frames.lock().unwrap();
            if frames.ended {
                return Err(CaptureError::SourceLost);
            }
            if frames.last.is_some() {
                return Ok(());
            }
        }
    }

    async fn capture(&mut self, mut sink: CaptureSink) -> Result<(), CaptureError> {
        let quality = *sink.quality.borrow_and_update();
        {
            let mut frames = self.frames.lock().unwrap();
            frames.last = Some(Instant::now());
            frames.output = Some((quality, sink.clone()));
        }
        let period = |quality: Quality| Duration::from_secs_f64(1.0 / f64::from(quality.fps));
        let mut ticks = interval(period(quality));
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                result = sink.quality.changed() => {
                    result.map_err(|_| CaptureError::Cancelled)?;
                    let quality = *sink.quality.borrow_and_update();
                    self.frames.lock().unwrap().output = Some((quality, sink.clone()));
                    ticks = interval(period(quality));
                    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    continue;
                }
                _ = ticks.tick() => {}
            }
            self.request_frame()?;
            let frames = self.frames.lock().unwrap();
            if frames.ended
                || frames
                    .last
                    .is_some_and(|last| last.elapsed() >= FRAME_TIMEOUT)
            {
                return Err(CaptureError::SourceLost);
            }
        }
    }
}

#[derive(Default)]
struct FrameStatus {
    last: Option<Instant>,
    output: Option<(Quality, CaptureSink)>,
    ended: bool,
}

struct Frames(Arc<Mutex<FrameStatus>>);

impl DesktopCapturerCallback for Frames {
    fn on_capture_result(&mut self, frame: Result<cxx::UniquePtr<DesktopFrame>, NativeError>) {
        let mut state = self.0.lock().unwrap();
        let frame = match frame {
            Ok(frame) => frame,
            Err(NativeError::Temporary) => return,
            Err(NativeError::Permanent) => {
                state.ended = true;
                return;
            }
        };
        // WebRTC's DesktopFrame owns stride × height bytes of BGRA pixels.
        unsafe { state.receive(frame.data(), frame.width(), frame.height(), frame.stride()) };
    }
}

impl FrameStatus {
    unsafe fn receive(&mut self, pixels: *const u8, width: i32, height: i32, stride: i32) {
        if width < 2 || height < 2 || width.checked_mul(4).is_none_or(|row| stride < row) {
            return;
        }
        let Some(size) = (stride as usize).checked_mul(height as usize) else {
            return;
        };
        if pixels.is_null() || size > isize::MAX as usize {
            return;
        }
        self.last = Some(Instant::now());
        if let Some((quality, sink)) = &self.output {
            let bytes = unsafe { std::slice::from_raw_parts(pixels, size) };
            if let Some(frame) = video_frame(bytes, width, height, stride, *quality) {
                sink.video(frame, *quality, quality.fps);
            }
        }
    }
}

fn video_frame(
    bytes: &[u8],
    width: i32,
    height: i32,
    stride: i32,
    quality: Quality,
) -> Option<VideoFrame<I420Buffer>> {
    let (scaled_width, scaled_height) =
        quality.dimensions(width.try_into().ok()?, height.try_into().ok()?)?;
    if stride < width.checked_mul(4)?
        || bytes.len() < (stride as usize).checked_mul(height as usize)?
    {
        return None;
    }
    let mut buffer = I420Buffer::new(width as u32, height as u32);
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    argb_to_i420(bytes, stride as u32, y, sy, u, su, v, sv, width, height);
    if (scaled_width, scaled_height) != (width as u32, height as u32) {
        buffer = buffer.scale(scaled_width as i32, scaled_height as i32);
    }
    Some(VideoFrame::new(VideoRotation::VideoRotation0, buffer))
}

#[cfg(test)]
mod tests;
