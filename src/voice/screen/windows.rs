use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

use ::windows::Win32::{
    Foundation::{E_ACCESSDENIED, HANDLE, HWND, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    UI::WindowsAndMessaging::GetClassNameW,
};
use livekit::webrtc::{native::yuv_helper::argb_to_i420, video_frame::VideoRotation};
use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Role, SampleType, StreamMode,
    WasapiError, WaveFormat,
};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiError, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::{Error as GraphicsError, GraphicsCaptureApi, InternalCaptureControl},
    monitor::Monitor,
    settings::{
        CaptureItemTypes, ColorFormat, CursorCaptureSettings, DirtyRegionSettings,
        DrawBorderSettings, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
        TryIntoCaptureItemWithType,
    },
    window::Window,
};
use windows_wgc::Graphics::Capture::GraphicsCaptureItem;

use super::*;

pub(super) async fn choices() -> Result<Vec<Choice>, CaptureError> {
    tokio::task::spawn_blocking(|| {
        let _apartment = Apartment::new()?;
        if !GraphicsCaptureApi::is_supported().unwrap_or(false) {
            return Err(CaptureError::Unavailable);
        }
        Ok(sources().into_iter().map(|(choice, _)| choice).collect())
    })
    .await
    .map_err(|_| CaptureError::Unavailable)?
}

pub(super) async fn select(
    choice: Choice,
    playback: Option<Device>,
) -> Result<Selection, CaptureError> {
    let label = choice.label.clone();
    let (item, fps, audio) = tokio::task::spawn_blocking(move || {
        let _apartment = Apartment::new()?;
        let (_, target) = sources()
            .into_iter()
            .find(|(available, _)| available == &choice)
            .ok_or(CaptureError::SourceLost)?;
        let fps = match &target {
            CaptureItemTypes::Monitor(monitor) => monitor.refresh_rate().ok(),
            CaptureItemTypes::Window(window) => {
                window.monitor().and_then(|m| m.refresh_rate().ok())
            }
        };
        let audio = AudioTarget::for_item(&target, playback);
        let (item, kind) = match target {
            CaptureItemTypes::Monitor(monitor) => monitor.try_into_capture_item(),
            CaptureItemTypes::Window(window) => window.try_into_capture_item(),
        }
        .map_err(|error| {
            if error.code().0 == E_ACCESSDENIED.0 {
                CaptureError::Permission
            } else {
                CaptureError::Unavailable
            }
        })?;
        if let CaptureItemTypes::Window(window) = &kind {
            if window_id(window).as_ref() != Some(&choice.id)
                || matches!(&audio, Some(AudioTarget::Application(process)) if !process.alive())
            {
                return Err(CaptureError::SourceLost);
            }
        }
        Ok::<_, CaptureError>((SelectedItem(item, kind), fps, audio))
    })
    .await
    .map_err(|_| CaptureError::Unavailable)??;

    let (scope, audio) = match audio {
        Some(audio) => audio.prepare().await,
        None => (None, None),
    };
    Ok(Selection {
        label,
        audio: scope,
        capture: Box::new(Selected { item, fps, audio }),
    })
}

fn sources() -> Vec<(Choice, CaptureItemTypes)> {
    let displays = Monitor::enumerate()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| {
            let name = monitor
                .name()
                .or_else(|_| monitor.device_name())
                .unwrap_or_default();
            let choice = Choice {
                id: format!("display:{:p}", monitor.as_raw_hmonitor()),
                label: format!("Display {} · {name}", index + 1),
            };
            (choice, CaptureItemTypes::Monitor(monitor))
        });
    let windows = Window::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|window| {
            let title = window
                .title()
                .ok()
                .filter(|title| !title.trim().is_empty())?;
            let choice = Choice {
                id: window_id(&window)?,
                label: format!("Window · {title}"),
            };
            Some((choice, CaptureItemTypes::Window(window)))
        });
    displays.chain(windows).collect()
}

fn window_id(window: &Window) -> Option<String> {
    let pid = window.process_id().ok()?;
    Some(format!("window:{:p}:{pid}", window.as_raw_hwnd()))
}

struct Apartment;

impl Apartment {
    fn new() -> Result<Self, CaptureError> {
        wasapi::initialize_mta()
            .ok()
            .map_err(|_| CaptureError::Unavailable)?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

struct SelectedItem(GraphicsCaptureItem, CaptureItemTypes);

impl TryIntoCaptureItemWithType for SelectedItem {
    fn try_into_capture_item(
        self,
    ) -> Result<(GraphicsCaptureItem, CaptureItemTypes), windows_wgc::core::Error> {
        Ok((self.0, self.1))
    }
}

struct Selected {
    item: SelectedItem,
    fps: Option<u32>,
    audio: Option<oneshot::Sender<AudioStart>>,
}

struct StopCapture(Arc<AtomicBool>);

impl Drop for StopCapture {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Capture for Selected {
    fn start(
        self: Box<Self>,
        quality: Quality,
        audio: bool,
        sink: CaptureSink,
    ) -> BoxFuture<'static, Result<(), CaptureError>> {
        Box::pin(async move {
            let stop = Arc::new(AtomicBool::new(false));
            let _owner = StopCapture(stop.clone());
            tokio::task::spawn_blocking(move || self.run(quality, audio, sink, stop))
                .await
                .map_err(|_| CaptureError::Unavailable)?
        })
    }
}

impl Selected {
    fn run(
        self,
        quality: Quality,
        with_audio: bool,
        sink: CaptureSink,
        stop: Arc<AtomicBool>,
    ) -> Result<(), CaptureError> {
        if stop.load(Ordering::Acquire) {
            return Err(CaptureError::Cancelled);
        }
        let fps = self
            .fps
            .filter(|fps| *fps > 0)
            .map_or(quality.fps, |fps| quality.fps.min(fps));
        let settings = Settings::new(
            self.item,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            Frames {
                sink: sink.clone(),
                stop: stop.clone(),
                quality,
                fps,
                started: Instant::now(),
                next_frame: Duration::ZERO,
            },
        );
        let capture = Frames::start_free_threaded(settings).map_err(|error| match error {
            GraphicsCaptureApiError::GraphicsCaptureApiError(GraphicsError::WindowsError(
                error,
            )) if error.code().0 == E_ACCESSDENIED.0 => CaptureError::Permission,
            _ => CaptureError::Unavailable,
        })?;
        if with_audio
            && !stop.load(Ordering::Acquire)
            && self.audio.is_none_or(|audio| {
                audio
                    .send(AudioStart {
                        sink: sink.clone(),
                        stop: stop.clone(),
                    })
                    .is_err()
            })
        {
            sink.audio_unavailable();
        }
        while !stop.load(Ordering::Acquire) && !capture.is_finished() {
            thread::sleep(Duration::from_millis(50));
        }
        if capture.is_finished() {
            let _ = capture.wait();
            Err(CaptureError::SourceLost)
        } else {
            let _ = capture.stop();
            Err(CaptureError::Cancelled)
        }
    }
}

struct Frames {
    sink: CaptureSink,
    stop: Arc<AtomicBool>,
    quality: Quality,
    fps: u32,
    started: Instant,
    next_frame: Duration,
}

impl GraphicsCaptureApiHandler for Frames {
    type Flags = Self;
    type Error = CaptureError;

    fn new(context: Context<Self>) -> Result<Self, Self::Error> {
        Ok(context.flags)
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop.load(Ordering::Acquire) {
            control.stop();
            return Ok(());
        }
        let elapsed = self.started.elapsed();
        if elapsed < self.next_frame {
            return Ok(());
        }
        let interval = Duration::from_nanos(1_000_000_000 / u64::from(self.fps));
        self.next_frame = (self.next_frame + interval).max(elapsed);
        let (width, height) = (frame.width(), frame.height());
        let Some(dimensions) = self.quality.dimensions(width, height) else {
            return Ok(());
        };
        let mut pixels = frame.buffer().map_err(|_| CaptureError::SourceLost)?;
        let stride = pixels.row_pitch();
        let buffer = screen_frame(pixels.as_raw_buffer(), width, height, stride, dimensions)
            .ok_or(CaptureError::SourceLost)?;
        let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
        frame.timestamp_us = elapsed.as_micros() as i64;
        self.sink.video(frame, self.fps);
        Ok(())
    }
}

fn screen_frame(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    (scaled_width, scaled_height): (u32, u32),
) -> Option<I420Buffer> {
    let row = width.checked_mul(4)?;
    let size = height.checked_mul(stride)? as usize;
    if stride < row || bytes.len() < size || stride > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    let mut buffer = I420Buffer::new(width, height);
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    argb_to_i420(
        bytes,
        stride,
        y,
        sy,
        u,
        su,
        v,
        sv,
        width as i32,
        height as i32,
    );
    if (width, height) != (scaled_width, scaled_height) {
        buffer = buffer.scale(scaled_width as i32, scaled_height as i32);
    }
    Some(buffer)
}

enum AudioTarget {
    Endpoint(Option<String>),
    Application(Process),
}

struct Process {
    id: u32,
    // Retaining the process object prevents a reused PID from changing the consented scope.
    handle: OwnedHandle,
}

impl Process {
    fn for_window(window: &Window) -> Option<Self> {
        let mut class = [0_u16; 256];
        let len = unsafe { GetClassNameW(HWND(window.as_raw_hwnd()), &mut class) };
        if len == 0 || String::from_utf16_lossy(&class[..len as usize]) == "ApplicationFrameWindow"
        {
            return None;
        }
        let id = window.process_id().ok().filter(|id| *id != 0)?;
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, id).ok()? };
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        let process = Self { id, handle };
        process.alive().then_some(process)
    }

    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(HANDLE(self.handle.as_raw_handle()), 0) == WAIT_TIMEOUT }
    }
}

struct AudioStart {
    sink: CaptureSink,
    stop: Arc<AtomicBool>,
}

impl AudioTarget {
    fn for_item(item: &CaptureItemTypes, playback: Option<Device>) -> Option<Self> {
        match item {
            CaptureItemTypes::Monitor(_) => Some(Self::Endpoint(playback.map(|device| device.id))),
            CaptureItemTypes::Window(window) => Process::for_window(window).map(Self::Application),
        }
    }

    async fn prepare(self) -> (Option<AudioScope>, Option<oneshot::Sender<AudioStart>>) {
        let scope = match &self {
            Self::Endpoint(_) => AudioScope::PlaybackMix,
            Self::Application(_) => AudioScope::Application,
        };
        let (ready, prepared) = oneshot::channel();
        let (start, command) = oneshot::channel();
        tokio::task::spawn_blocking(move || self.audio_loop(ready, command));
        if matches!(
            tokio::time::timeout(Duration::from_secs(3), prepared).await,
            Ok(Ok(()))
        ) {
            (Some(scope), Some(start))
        } else {
            (None, None)
        }
    }

    fn audio_loop(self, ready: oneshot::Sender<()>, command: oneshot::Receiver<AudioStart>) {
        let Ok(_apartment) = Apartment::new() else {
            return;
        };
        let Ok(mut client) = self.open_audio() else {
            return;
        };
        let format = WaveFormat::new(32, 32, SampleType::Float, 48_000, 2, None);
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        };
        if client
            .initialize_client(&format, Direction::Capture, mode, None)
            .is_err()
        {
            return;
        }
        let (Ok(event), Ok(reader)) = (
            client.set_get_eventhandle(),
            client.get_audiocaptureclient(),
        ) else {
            return;
        };
        if ready.send(()).is_err() {
            return;
        }
        let Ok(start) = command.blocking_recv() else {
            return;
        };
        if start.stop.load(Ordering::Acquire) {
            return;
        }
        if matches!(&self, Self::Application(process) if !process.alive())
            || client.start_stream().is_err()
        {
            start.sink.audio_unavailable();
            return;
        }

        let mut packet = Vec::new();
        let failed = loop {
            if start.stop.load(Ordering::Acquire) {
                break false;
            }
            if matches!(&self, Self::Application(process) if !process.alive())
                || read_audio(&reader, &mut packet, &start.sink).is_err()
            {
                break true;
            }
            match event.wait_for_event(50) {
                Ok(()) | Err(WasapiError::EventTimeout) => {}
                Err(_) => break true,
            }
        };
        let _ = client.stop_stream();
        if failed {
            start.sink.audio_unavailable();
        }
    }

    fn open_audio(&self) -> Result<AudioClient, WasapiError> {
        match self {
            Self::Application(process) => {
                AudioClient::new_application_loopback_client(process.id, true)
            }
            Self::Endpoint(id) => {
                let devices = DeviceEnumerator::new()?;
                let playback = match id {
                    Some(id) => devices.get_device(id)?,
                    None => devices
                        .get_default_device_for_role(Direction::Render, Role::Communications)?,
                };
                if playback.get_direction() != Direction::Render {
                    return Err(WasapiError::UnsupportedFormat);
                }
                playback.get_iaudioclient()
            }
        }
    }
}

fn read_audio(
    reader: &AudioCaptureClient,
    packet: &mut Vec<u8>,
    sink: &CaptureSink,
) -> Result<(), WasapiError> {
    for _ in 0..8 {
        let Some(frames) = reader.get_next_packet_size()?.filter(|frames| *frames > 0) else {
            break;
        };
        if frames > 48_000 {
            return Err(WasapiError::UnsupportedFormat);
        }
        packet.resize(frames as usize * 8, 0);
        let (frames, _) = reader.read_from_device(packet)?;
        for chunk in packet[..frames as usize * 8].chunks(480 * 8) {
            sink.audio(stereo_pcm(chunk));
        }
    }
    Ok(())
}

fn stereo_pcm(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(4)
        .map(|sample| {
            let sample = f32::from_le_bytes(sample.try_into().expect("four-byte PCM sample"));
            if sample.is_finite() {
                (sample * 32768.0).round() as i16
            } else {
                0
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
