use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use dfn3_rs::Denoiser;
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

use crate::voice::{
    Device, DeviceKind, NoiseSuppression, VoiceProcessing, audio_preview::cpal_device,
};

pub(super) const RATE: u32 = 48_000;
const FRAME: usize = RATE as usize / 100;
const _: () = assert!(dfn3_rs::SAMPLE_RATE == RATE && dfn3_rs::FRAME_SIZE == FRAME);

struct InputBlock {
    generation: u64,
    samples: Vec<i16>,
}

pub(super) struct ProcessedInput {
    preferences: watch::Sender<VoiceProcessing>,
    source: NativeAudioSource,
    generation: Arc<AtomicU64>,
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
        let generation = Arc::new(AtomicU64::new(0));
        let capture = tokio::spawn(capture(
            device,
            source.clone(),
            track,
            updates,
            generation.clone(),
            ready,
            stopped,
        ));
        (
            Self {
                preferences,
                source,
                generation,
                cancel: Some(cancel),
                capture,
            },
            opened,
        )
    }

    pub(super) fn update(&self, settings: VoiceProcessing) {
        if self.preferences.borrow().noise_suppression != settings.noise_suppression {
            self.discard_buffer();
        }
        self.preferences.send_if_modified(|current| {
            if *current == settings {
                return false;
            }
            *current = settings;
            true
        });
    }

    pub(super) fn discard_buffer(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
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
    generation: Arc<AtomicU64>,
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
    let (fail, mut failed) = oneshot::channel();
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
    }(&device, stream_config, send, fail, generation.clone())
    .ok()?;
    stream.play().ok()?;

    let result = tokio::select! {
        biased;
        _ = &mut stopped => None,
        _ = &mut failed => None,
        result = send_audio(source, track, preferences, ready, blocks, rate, generation) => result,
    };
    let _ = stream.pause();
    result
}

async fn send_audio(
    source: NativeAudioSource,
    track: LocalAudioTrack,
    preferences: watch::Receiver<VoiceProcessing>,
    ready: oneshot::Sender<()>,
    mut blocks: mpsc::Receiver<InputBlock>,
    rate: u32,
    generation: Arc<AtomicU64>,
) -> Option<()> {
    let mut ready = Some(ready);
    let mut resampler = AudioResampler::default();
    let mut controls = None;
    let mut apm = None;
    let mut gate = Gate::default();
    let mut denoiser = None;
    let mut last_generation = None;
    let mut frame = [0; FRAME];
    loop {
        let InputBlock {
            generation: captured,
            samples: mut block,
        } = timeout(Duration::from_secs(3), blocks.recv())
            .await
            .ok()??;
        if let Some(ready) = ready.take() {
            ready.send(()).ok()?;
        }
        if track.is_muted() || captured != generation.load(Ordering::Relaxed) {
            continue;
        }
        // A queued frame or the model's lookahead can outlive a mute or a mode change.
        if last_generation != Some(captured) {
            last_generation = Some(captured);
            resampler = AudioResampler::default();
            gate = Gate::default();
            denoiser = None;
            controls = None;
        }
        let settings = *preferences.borrow();
        let toggles = (
            settings.noise_suppression == NoiseSuppression::Standard,
            settings.auto_gain,
        );
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
        if settings.noise_suppression == NoiseSuppression::Enhanced {
            let model = denoiser.take();
            let pcm = std::array::from_fn(|i| f32::from(samples[i]) / 32768.0);
            // Run one block at a time off the session runtime. CPAL's bounded queue drops overload.
            let (model, filtered) = tokio::task::spawn_blocking(move || {
                let mut model = model.unwrap_or_else(|| Box::new(Denoiser::new()));
                let filtered = process_enhanced(&mut model, pcm);
                (model, filtered)
            })
            .await
            .ok()?;
            denoiser = Some(model);
            if track.is_muted() || captured != generation.load(Ordering::Relaxed) {
                continue;
            }
            samples.copy_from_slice(&filtered);
        }
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
}

fn process_enhanced(denoiser: &mut Denoiser, mut pcm: [f32; FRAME]) -> [i16; FRAME] {
    denoiser.process_frame(&mut pcm);
    pcm.map(|sample| (sample * 32768.0).round() as i16)
}

fn input<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    send: mpsc::Sender<InputBlock>,
    fail: oneshot::Sender<()>,
    generation: Arc<AtomicU64>,
) -> Result<cpal::Stream, cpal::Error>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let frames = config.sample_rate as usize / 100;
    let mut block = Vec::with_capacity(frames);
    let mut last_generation = None;
    let mut fail = Some(fail);
    device.build_input_stream(
        config,
        move |samples: &[T], _| {
            let captured = generation.load(Ordering::Relaxed);
            if last_generation != Some(captured) {
                last_generation = Some(captured);
                block.clear();
            }
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
                        slot.send(InputBlock {
                            generation: captured,
                            samples: std::mem::replace(&mut block, Vec::with_capacity(frames)),
                        });
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
    fn enhanced_suppresses_stationary_noise() {
        let mut denoiser = Denoiser::new();
        let mut seed = 1_u32;
        let (mut dry_power, mut wet_power) = (0.0, 0.0);
        for frame in 0..80 {
            let noise = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                f32::from((seed >> 16) as i16) / 327_680.0
            });
            let filtered = process_enhanced(&mut denoiser, noise);
            if frame >= 40 {
                dry_power += noise.iter().map(|sample| sample.powi(2)).sum::<f32>();
                wet_power += filtered
                    .iter()
                    .map(|sample| (f32::from(*sample) / 32768.0).powi(2))
                    .sum::<f32>();
            }
        }
        assert!(
            wet_power < dry_power * 0.25,
            "{wet_power} >= {dry_power} / 4"
        );
    }

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
