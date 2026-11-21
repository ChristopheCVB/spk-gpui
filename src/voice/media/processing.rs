use std::{borrow::Cow, time::Duration};

use livekit::prelude::LocalAudioTrack;
use livekit::webrtc::{
    audio_frame::AudioFrame,
    audio_source::native::NativeAudioSource,
    native::{apm::AudioProcessingModule, audio_resampler::AudioResampler},
};
use rodio::cpal::{
    self, BufferSize, FromSample, SampleFormat, SizedSample, SupportedBufferSize,
    traits::{DeviceTrait, StreamTrait},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

use crate::voice::{Device, DeviceKind, VoiceProcessing, audio_preview::cpal_device};

pub(super) const RATE: u32 = 48_000;
const FRAME: usize = RATE as usize / 100;

pub(super) struct ProcessedInput {
    preferences: watch::Sender<VoiceProcessing>,
    source: NativeAudioSource,
    cancel: Option<oneshot::Sender<()>>,
    capture: JoinHandle<Option<()>>,
}

impl ProcessedInput {
    pub(super) fn start(
        device: Option<Device>,
        preferences: VoiceProcessing,
        source: NativeAudioSource,
        track: LocalAudioTrack,
    ) -> (Self, oneshot::Receiver<()>) {
        let (preferences, updates) = watch::channel(preferences);
        let (ready, opened) = oneshot::channel();
        let (cancel, stopped) = oneshot::channel();
        let capture = tokio::spawn(capture(
            device,
            source.clone(),
            track,
            updates,
            ready,
            stopped,
        ));
        (
            Self {
                preferences,
                source,
                cancel: Some(cancel),
                capture,
            },
            opened,
        )
    }

    pub(super) fn update(&self, settings: VoiceProcessing) {
        self.preferences.send_if_modified(|current| {
            if *current == settings {
                return false;
            }
            *current = settings;
            true
        });
    }

    pub(super) fn discard_buffer(&self) {
        self.source.clear_buffer();
    }

    pub(super) fn ended(&self) -> bool {
        self.capture.is_finished()
    }

    pub(super) async fn stop(mut self) {
        self.cancel.take();
        let _ = (&mut self.capture).await;
    }
}

async fn capture(
    selected: Option<Device>,
    source: NativeAudioSource,
    track: LocalAudioTrack,
    preferences: watch::Receiver<VoiceProcessing>,
    ready: oneshot::Sender<()>,
    mut stopped: oneshot::Receiver<()>,
) -> Option<()> {
    #[cfg(target_os = "macos")]
    tokio::select! {
        biased;
        _ = &mut stopped => return None,
        granted = super::microphone_permission() => if !granted { return None; },
    }
    let device = cpal_device(DeviceKind::Microphone, selected.as_ref())?;
    let mut config = device.default_input_config().ok()?;
    let default_rate = config.sample_rate();
    // The resampler consumes whole 10 ms frames.
    if !(8_000..=192_000).contains(&default_rate) || !default_rate.is_multiple_of(100) {
        config = device
            .supported_input_configs()
            .ok()?
            .find_map(|range| range.try_with_sample_rate(RATE))?;
    }
    let rate = config.sample_rate();
    let mut stream_config = config.config();
    // PulseAudio's default buffering delivers bursts that can drop 10 ms RTC frames.
    if let SupportedBufferSize::Range { min, max } = *config.buffer_size() {
        stream_config.buffer_size = BufferSize::Fixed((rate / 50).clamp(min, max));
    }
    let (send, blocks) = mpsc::channel(8);
    let (fail, failed) = oneshot::channel();
    let stream = match config.sample_format() {
        SampleFormat::I8 => input::<i8>,
        SampleFormat::I16 => input::<i16>,
        SampleFormat::I24 => input::<cpal::I24>,
        SampleFormat::I32 => input::<i32>,
        SampleFormat::I64 => input::<i64>,
        SampleFormat::U8 => input::<u8>,
        SampleFormat::U16 => input::<u16>,
        SampleFormat::U24 => input::<cpal::U24>,
        SampleFormat::U32 => input::<u32>,
        SampleFormat::U64 => input::<u64>,
        SampleFormat::F32 => input::<f32>,
        SampleFormat::F64 => input::<f64>,
        _ => return None,
    }(&device, stream_config, send, fail)
    .ok()?;
    stream.play().ok()?;

    let result = tokio::select! {
        biased;
        _ = &mut stopped => None,
        result = send_audio(source, track, preferences, ready, blocks, failed, rate) => result,
    };
    let _ = stream.pause();
    result
}

async fn send_audio(
    source: NativeAudioSource,
    track: LocalAudioTrack,
    preferences: watch::Receiver<VoiceProcessing>,
    ready: oneshot::Sender<()>,
    mut blocks: mpsc::Receiver<Vec<i16>>,
    mut failed: oneshot::Receiver<()>,
    rate: u32,
) -> Option<()> {
    let mut ready = Some(ready);
    let mut resampler = AudioResampler::default();
    let mut controls = None;
    let mut apm = None;
    let mut gate = Gate::default();
    let mut frame = [0; FRAME];
    loop {
        let mut block = tokio::select! {
            biased;
            _ = &mut failed => return None,
            block = timeout(Duration::from_secs(3), blocks.recv()) => block.ok()??,
        };
        let settings = *preferences.borrow();
        let toggles = (settings.noise_suppression, settings.auto_gain);
        if controls != Some(toggles) {
            controls = Some(toggles);
            apm = (toggles.0 || toggles.1)
                .then(|| AudioProcessingModule::new(false, toggles.1, false, toggles.0));
        }
        let samples = if rate == RATE {
            block.as_mut_slice()
        } else {
            frame.copy_from_slice(resampler.remix_and_resample(
                &block,
                rate / 100,
                1,
                rate,
                1,
                RATE,
            ));
            frame.as_mut_slice()
        };
        if let Some(apm) = apm.as_mut() {
            apm.process_stream(samples, RATE as i32, 1).ok()?;
        }
        if track.is_muted() {
            gate = Gate::default();
        } else {
            gate.apply(samples, settings.sensitivity);
            source
                .capture_frame(&AudioFrame {
                    data: Cow::Borrowed(samples),
                    sample_rate: RATE,
                    num_channels: 1,
                    samples_per_channel: FRAME as u32,
                })
                .await
                .ok()?;
        }
        if let Some(ready) = ready.take() {
            ready.send(()).ok()?;
        }
    }
}

fn input<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    send: mpsc::Sender<Vec<i16>>,
    fail: oneshot::Sender<()>,
) -> Result<cpal::Stream, cpal::Error>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let frames = config.sample_rate as usize / 100;
    let mut block = Vec::with_capacity(frames);
    let mut fail = Some(fail);
    device.build_input_stream(
        config,
        move |samples: &[T], _| {
            for channel_frame in samples.chunks_exact(channels) {
                let mono = channel_frame
                    .iter()
                    .copied()
                    .map(f32::from_sample_)
                    .sum::<f32>()
                    / channels as f32;
                block.push(i16::from_sample_(mono.clamp(-1.0, 1.0)));
                if block.len() == frames {
                    if let Ok(slot) = send.try_reserve() {
                        slot.send(std::mem::replace(&mut block, Vec::with_capacity(frames)));
                    } else {
                        block.clear();
                    }
                }
            }
        },
        move |_| {
            if let Some(fail) = fail.take() {
                let _ = fail.send(());
            }
        },
        Some(Duration::from_secs(2)),
    )
}

#[derive(Default)]
struct Gate {
    hold: usize,
    gain: f32,
}

impl Gate {
    fn apply(&mut self, samples: &mut [i16], sensitivity: u8) {
        let power = samples
            .iter()
            .map(|sample| (f32::from(*sample) / 32768.0).powi(2))
            .sum::<f32>()
            / samples.len() as f32;
        let threshold_db = -20.0 - f32::from(sensitivity.min(100)) * 0.6;
        if 10.0 * (power + 1e-12).log10() >= threshold_db {
            self.hold = RATE as usize * 300 / 1000;
        } else {
            self.hold = self.hold.saturating_sub(samples.len());
        }
        let open = self.hold > 0;
        let fade_samples = if open { RATE / 200 } else { RATE / 20 };
        let step = 1.0 / fade_samples as f32;
        for sample in samples {
            self.gain = if open {
                (self.gain + step).min(1.0)
            } else {
                (self.gain - step).max(0.0)
            };
            *sample = (f32::from(*sample) * self.gain).round() as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_opens_for_quieter_audio_and_fades_after_the_hold() {
        let mut gate = Gate::default();
        let mut samples = [100; 480];
        gate.apply(&mut samples, 0);
        assert_eq!(samples, [0; 480]);

        samples.fill(100);
        gate.apply(&mut samples, 100);
        assert!(samples[0] < 5);
        assert_eq!(samples[479], 100);

        for _ in 0..29 {
            samples.fill(1);
            gate.apply(&mut samples, 100);
            assert_eq!(samples[479], 1);
        }
        for _ in 0..5 {
            samples.fill(1_000);
            gate.apply(&mut samples, 0);
        }
        assert_eq!(samples[479], 0);
    }
}
