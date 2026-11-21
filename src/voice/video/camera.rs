use std::{
    io::{self, BufWriter, Write},
    process::{ExitCode, Stdio},
    time::{Duration, Instant},
};

use image::{
    ImageFormat, RgbaImage,
    imageops::{self, FilterType},
};
use livekit::webrtc::{
    native::yuv_helper::abgr_to_i420,
    video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation},
};
use nokhwa::{
    Buffer,
    pixel_format::RgbAFormat,
    utils::{ApiBackend, CameraInfo, FrameFormat},
};
#[cfg(not(windows))]
use nokhwa::{
    Camera,
    pixel_format::FormatDecoder,
    utils::{RequestedFormat, RequestedFormatType},
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};

use super::CameraIntent;

#[cfg(any(windows, test))]
mod rgb24;
#[cfg(windows)]
mod windows;

const CAMERA_CAPTURE: &str = "--camera-capture";
const UNAVAILABLE: &str =
    "Camera is unavailable. Check the device and its permission, then try again.";
const PERMISSION: &str = "Allow Camera access in system settings, then try again.";
pub(super) const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const START_TIMEOUT: Duration =
    Duration::from_secs(if cfg!(target_os = "macos") { 120 } else { 10 });

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CameraResolution {
    #[default]
    Auto,
    P360,
    P720,
    P1080,
}

impl CameraResolution {
    pub const ALL: [Self; 4] = [Self::Auto, Self::P360, Self::P720, Self::P1080];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::P360 => "360p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
        }
    }

    fn ceiling(self) -> (u32, u32) {
        let height = match self {
            Self::Auto | Self::P1080 => 1080,
            Self::P360 => 360,
            Self::P720 => 720,
        };
        (height * 16 / 9, height)
    }
}

pub fn devices() -> Vec<CameraDevice> {
    nokhwa::query(ApiBackend::Auto)
        .unwrap_or_default()
        .into_iter()
        .map(|camera| CameraDevice {
            id: camera_id(&camera),
            name: camera.human_name(),
        })
        .collect()
}

fn camera_id(camera: &CameraInfo) -> String {
    let identifier = camera.misc();
    if identifier.is_empty() {
        format!("{}:{}", camera.index(), camera.human_name())
    } else {
        identifier
    }
}

pub(super) struct Capture {
    child: Child,
    reader: JoinHandle<()>,
    frames: mpsc::Receiver<VideoFrame<I420Buffer>>,
}

impl Capture {
    pub(super) fn start(intent: &CameraIntent) -> Result<Self, &'static str> {
        // Native camera APIs can block indefinitely on a damaged or removed device.
        // Keep them in a child process so Stop and Voice Session teardown can release capture.
        let executable = std::env::current_exe().map_err(|_| UNAVAILABLE)?;
        let mut child = Command::new(executable)
            .arg(CAMERA_CAPTURE)
            .arg(intent.resolution.label())
            .args(intent.device.as_deref())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| UNAVAILABLE)?;
        let mut stdout = BufReader::new(child.stdout.take().expect("camera stdout is piped"));
        let (send, frames) = mpsc::channel(1);
        let reader = tokio::spawn(async move {
            while let Ok(frame) = read_frame(&mut stdout).await {
                if send.send(frame).await.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            reader,
            frames,
        })
    }

    pub(super) async fn failure(&mut self) -> &'static str {
        match tokio::time::timeout(Duration::from_millis(200), self.child.wait()).await {
            Ok(Ok(status)) if status.code() == Some(2) => PERMISSION,
            _ => UNAVAILABLE,
        }
    }

    pub(super) async fn stop(mut self) {
        let _ = self.child.kill().await;
        self.reader.abort();
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub(super) async fn next_frame(capture: &mut Option<Capture>) -> Option<VideoFrame<I420Buffer>> {
    capture.as_mut()?.frames.recv().await
}

async fn read_frame(input: &mut (impl AsyncReadExt + Unpin)) -> io::Result<VideoFrame<I420Buffer>> {
    let width = input.read_u32_le().await?;
    let height = input.read_u32_le().await?;
    let timestamp = input.read_i64_le().await?;
    if width == 0 || height == 0 || width > 1920 || height > 1080 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut buffer = I420Buffer::new(width, height);
    let (y, u, v) = buffer.data_mut();
    input.read_exact(y).await?;
    input.read_exact(u).await?;
    input.read_exact(v).await?;
    let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
    frame.timestamp_us = timestamp;
    Ok(frame)
}

pub fn run_camera_command() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.as_os_str() != CAMERA_CAPTURE {
        return None;
    }
    let result = (|| {
        let resolution = args.next().ok_or(1u8)?;
        let resolution = CameraResolution::ALL
            .into_iter()
            .find(|option| resolution == option.label())
            .ok_or(1u8)?;
        let device = args.next().and_then(|id| id.into_string().ok());
        capture(resolution, device.as_deref())
    })();
    Some(ExitCode::from(result.err().unwrap_or_default()))
}

fn capture(resolution: CameraResolution, id: Option<&str>) -> Result<(), u8> {
    let (allowed, permission) = std::sync::mpsc::sync_channel(1);
    nokhwa::nokhwa_initialize(move |granted| {
        let _ = allowed.try_send(granted);
    });
    if !permission.recv().unwrap_or(false) {
        return Err(2);
    }
    let devices = nokhwa::query(ApiBackend::Auto).map_err(|_| 3u8)?;
    let device = devices
        .iter()
        .find(|device| id.is_none_or(|id| camera_id(device) == id))
        .ok_or(3u8)?;
    let (max_width, max_height) = resolution.ceiling();
    #[cfg(windows)]
    let mut camera = windows::Camera::open(device, (max_width, max_height)).map_err(|_| 3u8)?;
    #[cfg(not(windows))]
    let mut camera = open_camera(device, (max_width, max_height))?;
    let mut output = BufWriter::new(io::stdout().lock());
    let started = Instant::now();
    let mut last_frame = None;
    loop {
        let frame = camera.frame().map_err(|_| 3u8)?;
        let now = Instant::now();
        if last_frame.is_some_and(|last| now.duration_since(last) < Duration::from_millis(30)) {
            continue;
        }
        last_frame = Some(now);
        let mut image = decode_frame(&frame).ok_or(3u8)?;
        if image.width() > max_width || image.height() > max_height {
            let scale = (max_width as f64 / image.width() as f64)
                .min(max_height as f64 / image.height() as f64);
            image = imageops::resize(
                &image,
                (image.width() as f64 * scale).round().max(1.) as u32,
                (image.height() as f64 * scale).round().max(1.) as u32,
                FilterType::Triangle,
            );
        }
        let (width, height) = image.dimensions();
        let mut buffer = I420Buffer::new(width, height);
        let (stride_y, stride_u, stride_v) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        abgr_to_i420(
            image.as_raw(),
            width * 4,
            y,
            stride_y,
            u,
            stride_u,
            v,
            stride_v,
            width as i32,
            height as i32,
        );
        let frame = VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: now.duration_since(started).as_micros() as i64,
            frame_metadata: None,
            buffer,
        };
        if write_frame(&mut output, &frame).is_err() {
            return Ok(());
        }
    }
}

#[cfg(not(windows))]
fn open_camera(device: &CameraInfo, ceiling: (u32, u32)) -> Result<Camera, u8> {
    let mut camera = Camera::new(
        device.index().clone(),
        RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None),
    )
    .map_err(|_| 3u8)?;
    if let Ok(formats) = camera.compatible_camera_formats()
        && let Some(format) = formats
            .into_iter()
            .filter(|format| RgbAFormat::FORMATS.contains(&format.format()))
            .min_by_key(|format| {
                format_preference(
                    format.resolution().width(),
                    format.resolution().height(),
                    format.frame_rate(),
                    ceiling,
                )
            })
    {
        camera
            .set_camera_requset(RequestedFormat::new::<RgbAFormat>(
                RequestedFormatType::Exact(format),
            ))
            .map_err(|_| 3u8)?;
    }
    camera.open_stream().map_err(|_| 3u8)?;
    Ok(camera)
}

fn format_preference(
    width: u32,
    height: u32,
    fps: u32,
    (max_width, max_height): (u32, u32),
) -> (bool, u64, u32) {
    let oversize = width > max_width || height > max_height;
    let distance = u64::from(max_width.abs_diff(width)) + u64::from(max_height.abs_diff(height));
    (oversize, distance, fps.abs_diff(30))
}

fn decode_frame(frame: &Buffer) -> Option<RgbaImage> {
    if frame.source_frame_format() == FrameFormat::MJPEG {
        image::load_from_memory_with_format(frame.buffer(), ImageFormat::Jpeg)
            .ok()
            .map(|image| image.into_rgba8())
    } else {
        frame.decode_image::<RgbAFormat>().ok()
    }
}

fn write_frame(output: &mut impl Write, frame: &VideoFrame<I420Buffer>) -> io::Result<()> {
    output.write_all(&frame.buffer.width().to_le_bytes())?;
    output.write_all(&frame.buffer.height().to_le_bytes())?;
    output.write_all(&frame.timestamp_us.to_le_bytes())?;
    let (y, u, v) = frame.buffer.data();
    output.write_all(y)?;
    output.write_all(u)?;
    output.write_all(v)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_decodes_mjpeg_without_a_native_jpeg_library() {
        use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};
        use nokhwa::utils::Resolution;

        let mut jpeg = Vec::new();
        JpegEncoder::new(&mut jpeg)
            .encode(&[128; 8 * 8 * 3], 8, 8, ExtendedColorType::Rgb8)
            .unwrap();
        let frame = Buffer::new(Resolution::new(8, 8), &jpeg, FrameFormat::MJPEG);
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded.dimensions(), (8, 8));
        assert_eq!(decoded.get_pixel(0, 0).0, [128, 128, 128, 255]);
    }

    #[tokio::test]
    async fn frame_crosses_capture_boundary_with_timestamp_and_planes() {
        let mut buffer = I420Buffer::new(5, 3);
        let (y, u, v) = buffer.data_mut();
        y.fill(99);
        u.fill(60);
        v.fill(20);
        let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
        frame.timestamp_us = 42;
        let mut data = Vec::new();
        write_frame(&mut data, &frame).unwrap();
        let restored = read_frame(&mut data.as_slice()).await.unwrap();
        assert_eq!(restored.timestamp_us, 42);
        assert_eq!(restored.buffer.width(), 5);
        assert_eq!(restored.buffer.height(), 3);
        assert_eq!(restored.buffer.data(), frame.buffer.data());
    }
}
