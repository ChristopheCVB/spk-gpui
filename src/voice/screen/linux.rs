use std::{
    ffi::c_void,
    ptr::{NonNull, null_mut},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ashpd::desktop::screencast::{Screencast, SourceType as PortalSource};
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

mod pulse;

const TYPES: [(SourceType, &str, &str, PortalSource); 2] = [
    (
        SourceType::Screen,
        "display",
        "Display",
        PortalSource::MONITOR,
    ),
    (SourceType::Window, "window", "Window", PortalSource::WINDOW),
];
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

unsafe extern "C" {
    fn g_main_context_iteration(context: *mut c_void, may_block: i32) -> i32;
    fn g_bus_get_sync(
        bus_type: i32,
        cancellable: *mut c_void,
        error: *mut *mut c_void,
    ) -> *mut c_void;
    fn g_dbus_connection_flush(
        connection: *mut c_void,
        cancellable: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)>,
        user_data: *mut c_void,
    );
    fn g_object_unref(object: *mut c_void);
}

pub async fn choices() -> Result<Vec<Choice>, CaptureError> {
    if wayland()? {
        return timeout(Duration::from_secs(3), async {
            let portal = Screencast::new()
                .await
                .map_err(|_| CaptureError::Unavailable)?;
            let sources = portal
                .available_source_types()
                .await
                .map_err(|_| CaptureError::Unavailable)?;
            Ok(portal_choices(
                sources.contains(PortalSource::MONITOR),
                sources.contains(PortalSource::WINDOW),
            ))
        })
        .await
        .map_err(|_| CaptureError::Unavailable)?;
    }
    prepare_x11_window_capture()?;
    let mut choices = Vec::new();
    for (kind, id, label, _) in TYPES {
        let capturer = new_desktop_capturer(options(kind));
        if let Some(capturer) = capturer.as_ref() {
            choices.extend(capturer.get_source_list().into_iter().enumerate().map(
                |(index, source)| Choice {
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
    let wayland = wayland()?;
    let (kind, source_id) = TYPES
        .iter()
        .find_map(|&(kind, id, _, _)| {
            if wayland {
                (choice.id == id).then_some((kind, None))
            } else {
                let (source_type, source_id) = choice.id.split_once(':')?;
                (source_type == id).then_some((kind, Some(source_id.parse::<u64>().ok()?)))
            }
        })
        .ok_or(CaptureError::Unavailable)?;

    let portal = if wayland {
        Some(
            timeout(
                Duration::from_secs(3),
                tokio::task::spawn_blocking(PortalConnection::open),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .ok_or(CaptureError::Unavailable)?,
        )
    } else {
        None
    };
    let mut capturer = new_desktop_capturer(options(kind));
    if capturer.is_null() {
        return Err(CaptureError::Unavailable);
    }
    if let Some(id) = source_id
        && (!capturer
            .get_source_list()
            .iter()
            .any(|source| source.id == id)
            || !capturer.select_source(id))
    {
        return Err(CaptureError::SourceLost);
    }

    let frames = Arc::new(Mutex::new(FrameStatus::default()));
    capturer
        .pin_mut()
        .start(Box::new(DesktopCapturerCallbackWrapper::new(Box::new(
            Frames(frames.clone()),
        ))));
    let mut video = VideoCapture {
        frames,
        capturer,
        portal,
    };
    let wait = if wayland {
        Duration::from_secs(60)
    } else {
        FRAME_TIMEOUT
    };
    timeout(wait, video.wait_for_source())
        .await
        .map_err(|_| selection_error(wayland))?
        .map_err(|()| selection_error(wayland))?;

    let monitor = if kind == SourceType::Screen {
        pulse::Monitor::open(playback).await
    } else {
        None
    };
    Ok(Selection {
        label: choice.label,
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
        Ok(session) if session.starts_with("wayland") => {
            // Without this marker libwebrtc can silently fall back to Xwayland.
            display
                .filter(|name| !name.is_empty())
                .map(|_| true)
                .ok_or(CaptureError::Unavailable)
        }
        _ if display.is_some() => Err(CaptureError::Unavailable),
        _ => Ok(false),
    }
}

fn portal_choices(display: bool, window: bool) -> Vec<Choice> {
    TYPES
        .iter()
        .zip([display, window])
        .filter(|(_, available)| *available)
        .map(|(&(_, id, label, _), _)| Choice {
            id: id.into(),
            label: label.into(),
        })
        .collect()
}

fn selection_error(wayland: bool) -> CaptureError {
    if wayland {
        CaptureError::PortalSelection
    } else {
        CaptureError::SourceLost
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

struct VideoCapture {
    frames: Arc<Mutex<FrameStatus>>,
    capturer: cxx::UniquePtr<DesktopCapturer>,
    portal: Option<PortalConnection>,
}

impl VideoCapture {
    fn request_frame(&self) {
        if self.portal.is_some() {
            // GDBus dispatches the asynchronous ScreenCast replies on the default context.
            for _ in 0..8 {
                if unsafe { g_main_context_iteration(null_mut(), 0) } == 0 {
                    break;
                }
            }
        }
        self.capturer.capture_frame();
    }

    async fn wait_for_source(&mut self) -> Result<(), ()> {
        let mut ticks = interval(Duration::from_millis(32));
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            self.request_frame();
            let frames = self.frames.lock().unwrap();
            if frames.ended {
                return Err(());
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
            self.request_frame();
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

// The capturer drops first and queues Session.Close on this shared GDBus connection.
struct PortalConnection(NonNull<c_void>);

// GDBusConnection methods can be used from any thread and this wrapper owns one reference.
unsafe impl Send for PortalConnection {}

impl PortalConnection {
    fn open() -> Option<Self> {
        const G_BUS_TYPE_SESSION: i32 = 2;
        NonNull::new(unsafe { g_bus_get_sync(G_BUS_TYPE_SESSION, null_mut(), null_mut()) })
            .map(Self)
    }
}

impl Drop for PortalConnection {
    fn drop(&mut self) {
        // Keep the connection alive for the asynchronous flush after WebRTC releases its reference.
        unsafe {
            g_dbus_connection_flush(self.0.as_ptr(), null_mut(), None, null_mut());
            g_object_unref(self.0.as_ptr());
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
        let (width, height, stride) = (frame.width(), frame.height(), frame.stride());
        if width < 2 || height < 2 || width.checked_mul(4).is_none_or(|row| stride < row) {
            return;
        }
        let Some(size) = (stride as usize).checked_mul(height as usize) else {
            return;
        };
        if frame.data().is_null() || size > isize::MAX as usize {
            return;
        }
        state.last = Some(Instant::now());
        if let Some((quality, sink)) = &state.output {
            // WebRTC's DesktopFrame owns stride × height bytes of BGRA pixels.
            let bytes = unsafe { std::slice::from_raw_parts(frame.data(), size) };
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
