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
        SCPickerFilterOutcome, SCPickerOutcome,
    },
    cv::CVPixelBufferLockFlags,
    error::SCStreamErrorCode,
    prelude::*,
    stream::SCShareableContentStyle,
};

use super::*;

pub(super) async fn pick(_: Option<Device>) -> Result<Selection, CaptureError> {
    if !cfg!(target_arch = "aarch64")
        || !NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 15,
            minorVersion: 0,
            patchVersion: 0,
        })
    {
        return Err(CaptureError::Unavailable);
    }
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
    if NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
        majorVersion: 15,
        minorVersion: 2,
        patchVersion: 0,
    }) {
        SCContentSharingPicker::show(&config, move |outcome| {
            let selected = match outcome {
                SCPickerOutcome::Picked(result) => {
                    let filter = result.filter();
                    let label = match filter.style() {
                        Ok(SCShareableContentStyle::Display) => result
                            .displays()
                            .first()
                            .map(|display| format!("Display {}", display.display_id())),
                        Ok(SCShareableContentStyle::Window) => {
                            result.windows().first().and_then(window_label).or_else(|| {
                                result
                                    .applications()
                                    .first()
                                    .map(|app| app.application_name())
                            })
                        }
                        _ => None,
                    };
                    Ok((filter, label))
                }
                SCPickerOutcome::Cancelled => Err(CaptureError::Cancelled),
                SCPickerOutcome::Error(_) => Err(CaptureError::Unavailable),
            };
            let _ = send.send(selected);
        });
    } else {
        // The bridge's metadata path uses private KVC keys before macOS 15.2.
        SCContentSharingPicker::show_filter(&config, move |outcome| {
            let selected = match outcome {
                SCPickerFilterOutcome::Filter(filter) => Ok((filter, None)),
                SCPickerFilterOutcome::Cancelled => Err(CaptureError::Cancelled),
                _ => Err(CaptureError::Unavailable),
            };
            let _ = send.send(selected);
        });
    }
    let outcome = selected.await;
    picker.0 = false;
    let (filter, label) = outcome.map_err(|_| CaptureError::Unavailable)??;
    selection(filter, label)
}

fn window_label(window: &SCWindow) -> Option<String> {
    window
        .title()
        .filter(|title| !title.trim().is_empty())
        .or_else(|| {
            window
                .owning_application()
                .map(|app| app.application_name())
        })
}

fn selection(filter: SCContentFilter, label: Option<String>) -> Result<Selection, CaptureError> {
    let scope = match filter.style() {
        Ok(SCShareableContentStyle::Display) => AudioScope::PlaybackMix,
        Ok(SCShareableContentStyle::Window) => AudioScope::Application,
        _ => return Err(CaptureError::Unavailable),
    };
    let (width, height) = source_size(&filter).ok_or(CaptureError::Unavailable)?;
    let label = label
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| match scope {
            AudioScope::PlaybackMix => {
                let origin = filter.content_rect().origin;
                format!(
                    "Display · {width} × {height} at ({:.0}, {:.0})",
                    origin.x, origin.y
                )
            }
            AudioScope::Application => format!("Window · {width} × {height}"),
        });
    Ok(Selection {
        label,
        audio: Some(scope),
        capture: Box::new(Selected {
            filter,
            width,
            height,
        }),
    })
}

fn source_size(filter: &SCContentFilter) -> Option<(u32, u32)> {
    let size = filter.content_rect().size;
    let scale = f64::from(filter.point_pixel_scale());
    let (width, height) = (size.width * scale, size.height * scale);
    let valid = 2.0..=f64::from(u32::MAX);
    (valid.contains(&width) && valid.contains(&height)).then_some((width as u32, height as u32))
}

#[cfg(test)]
pub(super) fn fixture_selection(mode: &str, title: &str) -> (Selection, (u32, u32)) {
    // The test binary has no GPUI application to initialize CoreGraphics.
    // SAFETY: The bridge calls CGMainDisplayID without arguments or retaining any Rust state.
    unsafe { screencapturekit::ffi::sc_initialize_core_graphics() };
    let content = SCShareableContent::get().expect("list capturable macOS displays and windows");
    let builder = SCContentFilter::create();
    let (filter, label) = match mode {
        "display" => (
            builder
                .with_display(content.displays().first().expect("fixture macOS display"))
                .with_excluding_windows(&[])
                .build(),
            None,
        ),
        "window" => {
            let window = content
                .windows()
                .into_iter()
                .find(|window| window.title().as_deref() == Some(title))
                .expect("fixture macOS window");
            (builder.with_window(&window).build(), window_label(&window))
        }
        _ => panic!("invalid macOS Screen Share fixture mode"),
    };
    let filter = filter.expect("create ScreenCaptureKit fixture filter");
    let size = source_size(&filter).expect("macOS fixture source has valid dimensions");
    (
        selection(filter, label).expect("select macOS fixture source"),
        size,
    )
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
        audio: bool,
        sink: CaptureSink,
    ) -> BoxFuture<'static, Result<(), CaptureError>> {
        Box::pin(async move {
            let (signal, ended) = mpsc::sync_channel(1);
            let _owner = StopCapture(signal.clone());
            tokio::task::spawn_blocking(move || self.run(audio, sink, signal, ended))
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
        mut audio: bool,
        mut sink: CaptureSink,
        signal: mpsc::SyncSender<CaptureError>,
        ended: mpsc::Receiver<CaptureError>,
    ) -> Result<(), CaptureError> {
        let quality = *sink.quality.borrow_and_update();
        let size = quality
            .dimensions(self.width, self.height)
            .ok_or(CaptureError::Unavailable)?;
        let (format, frames) = watch::channel((quality, size));
        if let Ok(error) = ended.try_recv() {
            return Err(error);
        }
        let stream = match self.stream(audio, &sink, &signal, &frames) {
            Ok(stream) => stream,
            Err(CaptureError::Unavailable) if audio => {
                if let Ok(error) = ended.try_recv() {
                    return Err(error);
                }
                sink.audio_unavailable();
                audio = false;
                self.stream(audio, &sink, &signal, &frames)?
            }
            Err(error) => return Err(error),
        };
        let outcome = loop {
            match ended.recv_timeout(Duration::from_millis(50)) {
                Ok(error) => break error,
                Err(mpsc::RecvTimeoutError::Disconnected) => break CaptureError::SourceLost,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if sink.quality.has_changed().unwrap_or(false) {
                let next = *sink.quality.borrow_and_update();
                let (active, _) = *format.borrow();
                if next != active
                    && let Some(size) = next.dimensions(self.width, self.height)
                    && let Ok(config) = config(size, next, audio)
                    && stream.update_configuration(&config).is_ok()
                {
                    format.send_replace((next, size));
                }
            }
        };
        let _ = stream.stop_capture();
        Err(outcome)
    }

    fn stream(
        &self,
        audio: bool,
        sink: &CaptureSink,
        signal: &mpsc::SyncSender<CaptureError>,
        format: &watch::Receiver<(Quality, (u32, u32))>,
    ) -> Result<SCStream, CaptureError> {
        let (quality, size) = *format.borrow();
        let config = config(size, quality, audio)?;
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
                    let format = format.clone();
                    let started = Instant::now();
                    move |sample: CMSampleBuffer, _| match sample.frame_status() {
                        Some(SCFrameStatus::Stopped) => {
                            let _ = signal.try_send(CaptureError::SourceLost);
                        }
                        Some(SCFrameStatus::Complete | SCFrameStatus::Started) => {
                            let (quality, (width, height)) = *format.borrow();
                            if let Some(mut frame) = screen_frame(&sample, width, height) {
                                frame.timestamp_us = started.elapsed().as_micros() as i64;
                                sink.video(frame, quality, quality.fps);
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

fn config(
    (width, height): (u32, u32),
    quality: Quality,
    audio: bool,
) -> Result<SCStreamConfiguration, CaptureError> {
    SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_minimum_frame_interval(&CMTime::new(1, quality.fps as i32))
        .with_captures_audio(audio)
        .with_sample_rate(48_000)
        .with_channel_count(2)
        .with_excludes_current_process_audio(true)
        .with_preserves_aspect_ratio(true)
        .map_err(|_| CaptureError::Unavailable)
}

fn capture_error(error: SCError, fallback: CaptureError) -> CaptureError {
    match error.stream_error_code() {
        Some(SCStreamErrorCode::UserDeclined) => CaptureError::Permission,
        Some(
            SCStreamErrorCode::NoCaptureSource
            | SCStreamErrorCode::NoWindowList
            | SCStreamErrorCode::NoDisplayList
            | SCStreamErrorCode::FailedApplicationConnectionInvalid
            | SCStreamErrorCode::FailedApplicationConnectionInterrupted
            | SCStreamErrorCode::FailedNoMatchingApplicationContext
            | SCStreamErrorCode::UserStopped
            | SCStreamErrorCode::SystemStoppedStream,
        ) => CaptureError::SourceLost,
        _ if matches!(error, SCError::PermissionDenied(_)) => CaptureError::Permission,
        _ => fallback,
    }
}

#[cfg(test)]
#[test]
fn denied_or_lost_capture_does_not_qualify_for_video_only_retry() {
    for (code, expected) in [
        (SCStreamErrorCode::UserDeclined, CaptureError::Permission),
        (SCStreamErrorCode::NoCaptureSource, CaptureError::SourceLost),
        (SCStreamErrorCode::UserStopped, CaptureError::SourceLost),
        (
            SCStreamErrorCode::FailedToStartAudioCapture,
            CaptureError::Unavailable,
        ),
    ] {
        assert_eq!(
            capture_error(
                SCError::from_stream_error_code(code),
                CaptureError::Unavailable
            ),
            expected
        );
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
