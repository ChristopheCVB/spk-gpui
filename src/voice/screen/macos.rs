use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

use apple_cf::raw::CMAudioFormatDescriptionGetStreamBasicDescription;
use livekit::webrtc::{
    native::yuv_helper::argb_to_i420,
    video_frame::{VideoFrame, VideoRotation},
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM,
};
use objc2_foundation::{NSOperatingSystemVersion, NSProcessInfo};
use screencapturekit::{
    cm::SCFrameStatus,
    content_sharing_picker::{
        SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode,
        SCPickerFilterOutcome,
    },
    cv::CVPixelBufferLockFlags,
    error::SCStreamErrorCode,
    prelude::*,
    stream::SCShareableContentStyle,
};

use super::*;

pub(super) async fn choices() -> Result<Vec<Choice>, CaptureError> {
    if !cfg!(target_arch = "aarch64")
        || !NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 15,
            minorVersion: 0,
            patchVersion: 0,
        })
    {
        return Ok(Vec::new());
    }
    Ok(vec![Choice {
        id: "system-picker".into(),
        label: "Choose a display or window".into(),
    }])
}

pub(super) async fn select(_: Choice, _: Option<Device>) -> Result<Selection, CaptureError> {
    let mut config =
        SCContentSharingPickerConfiguration::new().map_err(|_| CaptureError::Unavailable)?;
    config.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
    ]);
    config.set_allows_changing_selected_content(false);
    let (send, selected) = oneshot::channel();
    // The app owns at most one picker. Deactivation is dispatched before any replacement picker.
    let mut picker = Picker(true);
    SCContentSharingPicker::show_filter(&config, move |outcome| {
        let _ = send.send(outcome);
    });
    let outcome = selected.await;
    picker.0 = false;
    let filter = match outcome {
        Ok(SCPickerFilterOutcome::Filter(filter)) => filter,
        Ok(SCPickerFilterOutcome::Cancelled) => return Err(CaptureError::Cancelled),
        _ => return Err(CaptureError::Unavailable),
    };
    let (label, scope) = match filter.style() {
        Ok(SCShareableContentStyle::Display) => ("Display", AudioScope::PlaybackMix),
        Ok(SCShareableContentStyle::Window) => ("Window", AudioScope::Application),
        _ => return Err(CaptureError::Unavailable),
    };
    let size = filter.content_rect().size;
    let scale = f64::from(filter.point_pixel_scale());
    let (width, height) = (size.width * scale, size.height * scale);
    if !(2.0..=f64::from(u32::MAX)).contains(&width)
        || !(2.0..=f64::from(u32::MAX)).contains(&height)
    {
        return Err(CaptureError::Unavailable);
    }
    Ok(Selection {
        label: label.into(),
        audio: Some(scope),
        capture: Box::new(Selected {
            filter,
            width: width as u32,
            height: height as u32,
        }),
    })
}

struct Picker(bool);

impl Drop for Picker {
    fn drop(&mut self) {
        if self.0 {
            SCContentSharingPicker::deactivate();
        }
    }
}

struct Selected {
    filter: SCContentFilter,
    width: u32,
    height: u32,
}

impl Capture for Selected {
    fn start(
        self: Box<Self>,
        quality: Quality,
        audio: bool,
        sink: CaptureSink,
    ) -> BoxFuture<'static, Result<(), CaptureError>> {
        Box::pin(async move {
            let (signal, ended) = mpsc::sync_channel(1);
            let _owner = StopCapture(signal.clone());
            tokio::task::spawn_blocking(move || self.run(quality, audio, sink, signal, ended))
                .await
                .map_err(|_| CaptureError::Unavailable)?
        })
    }
}

struct StopCapture(mpsc::SyncSender<CaptureError>);

impl Drop for StopCapture {
    fn drop(&mut self) {
        let _ = self.0.try_send(CaptureError::Cancelled);
    }
}

impl Selected {
    fn run(
        self: Box<Self>,
        quality: Quality,
        audio: bool,
        sink: CaptureSink,
        signal: mpsc::SyncSender<CaptureError>,
        ended: mpsc::Receiver<CaptureError>,
    ) -> Result<(), CaptureError> {
        let dimensions = quality
            .dimensions(self.width, self.height)
            .ok_or(CaptureError::Unavailable)?;
        if let Ok(error) = ended.try_recv() {
            return Err(error);
        }
        let stream = match self.stream(dimensions, quality.fps, audio, &sink, &signal) {
            Ok(stream) => stream,
            Err(error) if audio && error != CaptureError::Permission => {
                if let Ok(error) = ended.try_recv() {
                    return Err(error);
                }
                sink.audio_unavailable();
                self.stream(dimensions, quality.fps, false, &sink, &signal)?
            }
            Err(error) => return Err(error),
        };
        let outcome = ended.recv().unwrap_or(CaptureError::SourceLost);
        let _ = stream.stop_capture();
        Err(outcome)
    }

    fn stream(
        &self,
        (width, height): (u32, u32),
        fps: u32,
        audio: bool,
        sink: &CaptureSink,
        signal: &mpsc::SyncSender<CaptureError>,
    ) -> Result<SCStream, CaptureError> {
        let config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_minimum_frame_interval(&CMTime::new(1, fps as i32))
            .with_captures_audio(audio)
            .with_sample_rate(48_000)
            .with_channel_count(2)
            .with_excludes_current_process_audio(true)
            .with_preserves_aspect_ratio(true)
            .map_err(|_| CaptureError::Unavailable)?;
        let mut stream = SCStream::new_with_delegate(
            &self.filter,
            &config,
            ErrorHandler::new({
                let signal = signal.clone();
                move |error| {
                    let _ = signal.try_send(capture_error(error, CaptureError::SourceLost));
                }
            }),
        )
        .map_err(|error| capture_error(error, CaptureError::Unavailable))?;
        stream
            .add_output_handler(
                {
                    let sink = sink.clone();
                    let signal = signal.clone();
                    let started = Instant::now();
                    move |sample: CMSampleBuffer, _| match sample.frame_status() {
                        Some(SCFrameStatus::Stopped) => {
                            let _ = signal.try_send(CaptureError::SourceLost);
                        }
                        Some(SCFrameStatus::Complete) => {
                            if let Some(mut frame) = screen_frame(&sample, width, height) {
                                frame.timestamp_us = started.elapsed().as_micros() as i64;
                                sink.video(frame, fps);
                            }
                        }
                        _ => {}
                    }
                },
                SCStreamOutputType::Screen,
            )
            .map_err(|_| CaptureError::Unavailable)?;
        if audio {
            let sink = sink.clone();
            let available = AtomicBool::new(true);
            stream
                .add_output_handler(
                    move |sample: CMSampleBuffer, _| {
                        if !sample.data_is_ready() || !available.load(Ordering::Relaxed) {
                            return;
                        }
                        if let Some(samples) = stereo_pcm(&sample) {
                            sink.audio(samples);
                        } else {
                            available.store(false, Ordering::Relaxed);
                            sink.audio_unavailable();
                        }
                    },
                    SCStreamOutputType::Audio,
                )
                .map_err(|_| CaptureError::Unavailable)?;
        }
        if let Err(error) = stream.start_capture() {
            let _ = stream.stop_capture();
            return Err(capture_error(error, CaptureError::Unavailable));
        }
        Ok(stream)
    }
}

fn capture_error(error: SCError, fallback: CaptureError) -> CaptureError {
    if matches!(error, SCError::PermissionDenied(_))
        || error.stream_error_code() == Some(SCStreamErrorCode::UserDeclined)
    {
        CaptureError::Permission
    } else {
        fallback
    }
}

fn screen_frame(
    sample: &CMSampleBuffer,
    width: u32,
    height: u32,
) -> Option<VideoFrame<I420Buffer>> {
    let pixel = sample.pixel_buffer()?;
    if pixel.width() != width as usize
        || pixel.height() != height as usize
        || pixel.pixel_format() != u32::from_be_bytes(*b"BGRA")
    {
        return None;
    }
    let stride = pixel.bytes_per_row();
    if stride < width as usize * 4 {
        return None;
    }
    let guard = pixel.lock(CVPixelBufferLockFlags::READ_ONLY).ok()?;
    let address = guard.base_address();
    let row_width = width as usize * 4;
    let mapped = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_width)?;
    if address.is_null() || guard.data_size() < mapped || mapped > isize::MAX as usize {
        return None;
    }

    let packed;
    // ScreenCaptureKit initializes the active BGRA rows. Padding is outside that guarantee.
    let bytes = if stride == row_width {
        unsafe { std::slice::from_raw_parts(address, mapped) }
    } else {
        let mut rows = Vec::with_capacity(row_width * height as usize);
        for row in 0..height as usize {
            rows.extend_from_slice(unsafe {
                std::slice::from_raw_parts(address.add(row * stride), row_width)
            });
        }
        packed = rows;
        &packed
    };
    let mut buffer = I420Buffer::new(width, height);
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    argb_to_i420(
        bytes,
        width * 4,
        y,
        sy,
        u,
        su,
        v,
        sv,
        width as i32,
        height as i32,
    );
    Some(VideoFrame::new(VideoRotation::VideoRotation0, buffer))
}

fn stereo_pcm(sample: &CMSampleBuffer) -> Option<Vec<i16>> {
    let format = sample.format_description()?;
    // CoreMedia owns this description for the retained format's lifetime.
    let stream = unsafe {
        CMAudioFormatDescriptionGetStreamBasicDescription(format.as_ptr().cast()).as_ref()?
    };
    let required = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    if stream.mFormatID != kAudioFormatLinearPCM
        || stream.mSampleRate != 48_000.
        || stream.mChannelsPerFrame != 2
        || stream.mBitsPerChannel != 32
        || stream.mFormatFlags & !kAudioFormatFlagIsNonInterleaved != required
    {
        return None;
    }
    let count = usize::try_from(sample.num_samples()).ok()?;
    if count == 0 {
        return Some(Vec::new());
    }
    if count > 48_000 {
        return None;
    }
    let buffers = sample.audio_buffer_list().ok()?;
    let first = buffers.get(0)?;
    let bytes = first.data();
    let planar = stream.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
    match (planar, buffers.num_buffers(), first.number_channels) {
        (false, 1, 2) if bytes.len() == count * 8 => {
            Some(bytes.chunks_exact(4).map(pcm16).collect())
        }
        (true, 2, 1) if bytes.len() == count * 4 => {
            let second = buffers.get(1)?;
            let right = second.data();
            (second.number_channels == 1 && right.len() == count * 4).then(|| {
                bytes
                    .chunks_exact(4)
                    .zip(right.chunks_exact(4))
                    .flat_map(|(left, right)| [pcm16(left), pcm16(right)])
                    .collect()
            })
        }
        _ => None,
    }
}

fn pcm16(sample: &[u8]) -> i16 {
    let sample = f32::from_ne_bytes(sample.try_into().expect("four-byte PCM sample"));
    if sample.is_finite() {
        (sample * 32768.).round() as i16
    } else {
        0
    }
}
