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
    System::{
        Com::{CO_MTA_USAGE_COOKIE, CoDecrementMTAUsage, CoIncrementMTAUsage},
        Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
    UI::WindowsAndMessaging::GetClassNameW,
};
use livekit::webrtc::{native::yuv_helper::argb_to_i420, video_frame::VideoRotation};
use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Role, SampleType, StreamMode,
    WasapiError, WaveFormat,
};
use windows_capture::{
    monitor::Monitor,
    settings::{CaptureItemTypes, TryIntoCaptureItemWithType},
    window::Window,
};
use windows_wgc::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCapturePicker,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{E_ABORT, E_POINTER, HMODULE, HWND as PickerHwnd},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
                ID3D11Texture2D,
            },
            Dxgi::IDXGIDevice,
        },
        System::WinRT::Direct3D11::{
            CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
        },
        UI::Shell::IInitializeWithWindow,
    },
    core::Interface,
};

use super::*;

pub(crate) const SYSTEM_PICKER: &str = "windows-picker";

pub(crate) async fn pick_system_source(hwnd: isize) -> Result<GraphicsCaptureItem, CaptureError> {
    let picker = GraphicsCapturePicker::new().map_err(graphics_error)?;
    let owner = picker
        .cast::<IInitializeWithWindow>()
        .map_err(graphics_error)?;
    // GPUI supplies the live owner HWND and runs this operation on its OLE-initialized UI thread.
    unsafe { owner.Initialize(PickerHwnd(hwnd as *mut _)) }.map_err(graphics_error)?;
    picker
        .PickSingleItemAsync()
        .map_err(graphics_error)?
        .await
        .map_err(|error| {
            if error.code().is_ok() || matches!(error.code(), E_POINTER | E_ABORT) {
                CaptureError::Cancelled
            } else {
                graphics_error(error)
            }
        })
}

pub(super) async fn select_system(item: GraphicsCaptureItem) -> Result<Selection, CaptureError> {
    tokio::task::spawn_blocking(move || {
        let _apartment = Apartment::new()?;
        let apartment = GraphicsApartment::new()?;
        let label = item
            .DisplayName()
            .map_err(graphics_error)?
            .to_string_lossy();
        // The system picker exposes a capture item without a stable HWND or playback endpoint.
        Ok(Selection {
            label,
            audio: None,
            capture: Box::new(Selected {
                item: SelectedItem(item, apartment),
                window: None,
                fps: None,
                audio: None,
            }),
        })
    })
    .await
    .map_err(|_| CaptureError::Unavailable)?
}

pub(super) async fn choices() -> Result<Vec<Choice>, CaptureError> {
    tokio::task::spawn_blocking(|| {
        let _apartment = Apartment::new()?;
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(CaptureError::Unavailable);
        }
        Ok(std::iter::once(Choice {
            id: SYSTEM_PICKER.into(),
            label: "Windows picker (video only)".into(),
        })
        .chain(sources().into_iter().map(|(choice, _)| choice))
        .collect())
    })
    .await
    .map_err(|_| CaptureError::Unavailable)?
}

pub(super) async fn select(
    choice: Choice,
    playback: Option<Device>,
) -> Result<Selection, CaptureError> {
    let label = choice.label.clone();
    let (item, window, fps, audio) = tokio::task::spawn_blocking(move || {
        let _apartment = Apartment::new()?;
        let apartment = GraphicsApartment::new()?;
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
        .map_err(graphics_error)?;
        let window = match kind {
            CaptureItemTypes::Monitor(_) => None,
            CaptureItemTypes::Window(window) => {
                if window_id(&window).as_ref() != Some(&choice.id)
                    || matches!(&audio, Some(AudioTarget::Application(process)) if !process.alive())
                {
                    return Err(CaptureError::SourceLost);
                }
                let pid = window.process_id().map_err(|_| CaptureError::SourceLost)?;
                Some((window, pid))
            }
        };
        Ok::<_, CaptureError>((SelectedItem(item, apartment), window, fps, audio))
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
        capture: Box::new(Selected {
            item,
            window,
            fps,
            audio,
        }),
    })
}

fn sources() -> Vec<(Choice, CaptureItemTypes)> {
    let displays = Monitor::enumerate()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| {
            let name = monitor.device_name().unwrap_or_default();
            let label = name.strip_prefix(r"\\.\DISPLAY").map_or_else(
                || format!("Display {}", index + 1),
                |number| format!("Display {number}"),
            );
            let choice = Choice {
                id: format!("display:{:p}:{name}", monitor.as_raw_hmonitor()),
                label,
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

// Keep the MTA active while the selected WinRT object is held between native worker threads.
struct GraphicsApartment(CO_MTA_USAGE_COOKIE);

// Windows exposes the cookie as an opaque pointer and allows decrementing it on another thread.
unsafe impl Send for GraphicsApartment {}

impl GraphicsApartment {
    fn new() -> Result<Self, CaptureError> {
        unsafe { CoIncrementMTAUsage() }
            .map(Self)
            .map_err(|_| CaptureError::Unavailable)
    }
}

impl Drop for GraphicsApartment {
    fn drop(&mut self) {
        let _ = unsafe { CoDecrementMTAUsage(self.0) };
    }
}

struct SelectedItem(GraphicsCaptureItem, GraphicsApartment);

struct Selected {
    item: SelectedItem,
    window: Option<(Window, u32)>,
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
        let _apartment = Apartment::new()?;
        // Keep the selection MTA alive until the capture worker has entered it.
        let SelectedItem(item, apartment) = self.item;
        drop(apartment);

        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(graphics_error)?;
        let mut frames = Frames {
            device: device.ok_or(CaptureError::Unavailable)?,
            context: context.ok_or(CaptureError::Unavailable)?,
            staging: None,
            packed: Vec::new(),
        };
        let dxgi_device = frames
            .device
            .cast::<IDXGIDevice>()
            .map_err(graphics_error)?;
        let device = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
            .and_then(|device| device.cast::<IDirect3DDevice>())
            .map_err(graphics_error)?;
        let pixel = DirectXPixelFormat::B8G8R8A8UIntNormalized;
        let mut size = item.Size().map_err(graphics_error)?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, pixel, 1, size)
            .map_err(graphics_error)?;
        let session = pool.CreateCaptureSession(&item).map_err(graphics_error)?;
        let capture_thread = thread::current();
        let frame_ready = Arc::new(AtomicBool::new(false));
        let frame_event = pool
            .FrameArrived(&TypedEventHandler::new({
                let frame_ready = frame_ready.clone();
                let capture_thread = capture_thread.clone();
                move |_, _| {
                    if !frame_ready.swap(true, Ordering::AcqRel) {
                        capture_thread.unpark();
                    }
                    Ok(())
                }
            }))
            .map_err(graphics_error)?;
        let ended = Arc::new(AtomicBool::new(false));
        let closed_event = item
            .Closed(&TypedEventHandler::new({
                let ended = ended.clone();
                move |_, _| {
                    ended.store(true, Ordering::Release);
                    capture_thread.unpark();
                    Ok(())
                }
            }))
            .map_err(graphics_error)?;
        let capture = GraphicsCapture {
            item,
            pool,
            session,
            closed_event,
            frame_event,
        };
        if stop.load(Ordering::Acquire) {
            return Err(CaptureError::Cancelled);
        }
        capture.session.StartCapture().map_err(graphics_error)?;
        if with_audio
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

        let started = Instant::now();
        let interval = Duration::from_nanos(1_000_000_000 / u64::from(fps));
        let mut next_frame = started;
        while !stop.load(Ordering::Acquire) && !ended.load(Ordering::Acquire) {
            if self
                .window
                .as_ref()
                .is_some_and(|(window, pid)| window.process_id().ok() != Some(*pid))
            {
                return Err(CaptureError::SourceLost);
            }
            let now = Instant::now();
            if now < next_frame {
                thread::park_timeout((next_frame - now).min(Duration::from_millis(50)));
                continue;
            }
            if !frame_ready.swap(false, Ordering::AcqRel) {
                thread::park_timeout(Duration::from_millis(50));
                continue;
            }
            next_frame = (next_frame + interval).max(now + interval / 2);
            let frame = match capture.pool.TryGetNextFrame() {
                Ok(frame) => frame,
                Err(error) if error.code() == E_POINTER => continue,
                Err(_) => return Err(CaptureError::SourceLost),
            };
            let content = frame.ContentSize().map_err(|_| CaptureError::SourceLost)?;
            if content.Width != size.Width || content.Height != size.Height {
                frame.Close().map_err(|_| CaptureError::SourceLost)?;
                if content.Width >= 2 && content.Height >= 2 {
                    size = content;
                    capture
                        .pool
                        .Recreate(&device, pixel, 1, size)
                        .map_err(|_| CaptureError::SourceLost)?;
                }
                continue;
            }
            let output = render_frame(&mut frames, &frame, size, quality);
            frame.Close().map_err(|_| CaptureError::SourceLost)?;
            let mut output = output?;
            output.timestamp_us = now.duration_since(started).as_micros() as i64;
            sink.video(output, fps);
        }
        if ended.load(Ordering::Acquire) {
            Err(CaptureError::SourceLost)
        } else {
            Err(CaptureError::Cancelled)
        }
    }
}

fn graphics_error(error: windows_wgc::core::Error) -> CaptureError {
    if error.code().0 == E_ACCESSDENIED.0 {
        CaptureError::Permission
    } else {
        CaptureError::Unavailable
    }
}

struct GraphicsCapture {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    closed_event: i64,
    frame_event: i64,
}

impl Drop for GraphicsCapture {
    fn drop(&mut self) {
        let _ = self.item.RemoveClosed(self.closed_event);
        let _ = self.pool.RemoveFrameArrived(self.frame_event);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

struct Frames {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<((u32, u32), ID3D11Texture2D)>,
    packed: Vec<u8>,
}

fn render_frame(
    frames: &mut Frames,
    frame: &windows_wgc::Graphics::Capture::Direct3D11CaptureFrame,
    size: SizeInt32,
    quality: Quality,
) -> Result<VideoFrame<I420Buffer>, CaptureError> {
    let dimensions = quality
        .dimensions(size.Width as u32, size.Height as u32)
        .ok_or(CaptureError::SourceLost)?;
    let surface = frame
        .Surface()
        .and_then(|surface| surface.cast::<IDirect3DDxgiInterfaceAccess>())
        .map_err(|_| CaptureError::SourceLost)?;
    let texture = unsafe { surface.GetInterface::<ID3D11Texture2D>() }
        .map_err(|_| CaptureError::SourceLost)?;
    frames
        .read_frame(
            &texture,
            (size.Width as u32, size.Height as u32),
            dimensions,
        )
        .map(|buffer| VideoFrame::new(VideoRotation::VideoRotation0, buffer))
}

impl Frames {
    fn read_frame(
        &mut self,
        source: &ID3D11Texture2D,
        (width, height): (u32, u32),
        dimensions: (u32, u32),
    ) -> Result<I420Buffer, CaptureError> {
        // Own the staging resource until the mapped bytes have been read and unmapped.
        if self
            .staging
            .as_ref()
            .is_none_or(|(size, _)| *size != (width, height))
        {
            let mut descriptor = D3D11_TEXTURE2D_DESC::default();
            unsafe { source.GetDesc(&mut descriptor) };
            if descriptor.Width < width || descriptor.Height < height {
                return Err(CaptureError::SourceLost);
            }
            descriptor.Usage = D3D11_USAGE_STAGING;
            descriptor.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            descriptor.BindFlags = 0;
            descriptor.MiscFlags = 0;
            let mut texture = None;
            unsafe {
                self.device
                    .CreateTexture2D(&descriptor, None, Some(&mut texture))
            }
            .map_err(|_| CaptureError::SourceLost)?;
            self.staging = texture.map(|texture| ((width, height), texture));
        }
        let (_, staging) = self.staging.as_ref().ok_or(CaptureError::SourceLost)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(staging, source);
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|_| CaptureError::SourceLost)?;
        let row = width.checked_mul(4);
        let result = row.and_then(|row| {
            let pitch = mapped.RowPitch;
            let size = pitch.checked_mul(height)? as usize;
            if mapped.pData.is_null() || pitch < row || size > isize::MAX as usize {
                return None;
            }
            let address = mapped.pData.cast::<u8>();
            if pitch == row {
                let bytes = unsafe { std::slice::from_raw_parts(address, size) };
                return screen_frame(bytes, width, height, row, dimensions);
            }
            // Only the active pixels in each mapped row are initialized.
            self.packed.clear();
            for index in 0..height as usize {
                let bytes = unsafe {
                    std::slice::from_raw_parts(address.add(index * pitch as usize), row as usize)
                };
                self.packed.extend_from_slice(bytes);
            }
            screen_frame(&self.packed, width, height, row, dimensions)
        });
        unsafe { self.context.Unmap(staging, 0) };
        result.ok_or(CaptureError::SourceLost)
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
