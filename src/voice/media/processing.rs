use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
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
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

use crate::voice::{
    Device, DeviceKind, NoiseSuppression, VoiceProcessing, audio_preview::cpal_device,
};

pub(super) const RATE: u32 = 48_000;
pub(super) const FRAME: usize = RATE as usize / 100;
const MAX_RENDER_AGE: Duration = Duration::from_millis(100);
const _: () = assert!(dfn3_rs::SAMPLE_RATE == RATE && dfn3_rs::FRAME_SIZE == FRAME);

pub(super) struct RenderBlock {
    pub(super) stream: Arc<()>,
    pub(super) playback: Instant,
    pub(super) rate: u32,
    pub(super) channels: u32,
    pub(super) samples: Vec<i16>,
}

struct InputBlock {
    generation: u64,
    captured_at: Instant,
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
        render: broadcast::Receiver<Arc<RenderBlock>>,
    ) -> (Self, oneshot::Receiver<()>) {
        let (preferences, updates) = watch::channel(preferences);
        let (ready, opened) = oneshot::channel();
        let (cancel, stopped) = oneshot::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let capture = tokio::spawn(capture(
            device,
            (source.clone(), track),
            updates,
            generation.clone(),
            ready,
            stopped,
            render,
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
    publication: (NativeAudioSource, LocalAudioTrack),
    preferences: watch::Receiver<VoiceProcessing>,
    generation: Arc<AtomicU64>,
    ready: oneshot::Sender<()>,
    mut stopped: oneshot::Receiver<()>,
    render: broadcast::Receiver<Arc<RenderBlock>>,
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
        result = send_audio(publication, preferences, ready, blocks, rate, generation, render) => result,
    };
    let _ = stream.pause();
    result
}

async fn send_audio(
    (source, track): (NativeAudioSource, LocalAudioTrack),
    preferences: watch::Receiver<VoiceProcessing>,
    ready: oneshot::Sender<()>,
    mut blocks: mpsc::Receiver<InputBlock>,
    rate: u32,
    generation: Arc<AtomicU64>,
    mut render: broadcast::Receiver<Arc<RenderBlock>>,
) -> Option<()> {
    let mut ready = Some(ready);
    let mut resampler = AudioResampler::default();
    let mut controls = None;
    let mut apm = None;
    let mut render_stream: Option<Arc<()>> = None;
    let mut render_delay = None;
    let mut render_resampler = AudioResampler::default();
    let mut gate = Gate::default();
    let mut denoiser = None;
    let mut last_generation = None;
    let mut frame = [0; FRAME];
    let mut reverse = [0; FRAME * 2];
    loop {
        let InputBlock {
            generation: captured,
            captured_at,
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
            render_stream = None;
            render_delay = None;
            render_resampler = AudioResampler::default();
        }
        let settings = *preferences.borrow();
        let config = WebRtcSettings::from(settings);
        if controls.replace(config) != Some(config) {
            apm = config.processor();
            render_delay = None;
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
        // Keep the render queue current even when echo cancellation is off.
        for _ in 0..16 {
            let block = match render.try_recv() {
                Ok(block) => block,
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    render_stream = None;
                    render_delay = None;
                    continue;
                }
                Err(_) => break,
            };
            if !config.echo
                || captured_at.saturating_duration_since(block.playback) > MAX_RENDER_AGE
            {
                continue;
            }
            if !render_stream
                .as_ref()
                .is_some_and(|stream| Arc::ptr_eq(stream, &block.stream))
            {
                render_stream = Some(block.stream.clone());
                render_resampler = AudioResampler::default();
                apm = config.processor();
            }
            let reverse = &mut reverse[..FRAME * block.channels as usize];
            let pcm = if block.rate == RATE {
                &block.samples
            } else {
                render_resampler.remix_and_resample(
                    &block.samples,
                    block.rate / 100,
                    block.channels,
                    block.rate,
                    block.channels,
                    RATE,
                )
            };
            reverse.copy_from_slice(pcm);
            let reverse_at = Instant::now();
            apm.as_mut()?
                .module
                .process_reverse_stream(reverse, RATE as i32, block.channels as i32)
                .ok()?;
            render_delay = Some((block.playback, reverse_at));
        }
        if let Some(apm) = apm.as_mut() {
            let delay = render_delay
                .filter(|(played_at, _)| {
                    captured_at.saturating_duration_since(*played_at) <= MAX_RENDER_AGE
                })
                .map_or(Duration::ZERO, |(playback, reverse_at)| {
                    stream_delay(playback, reverse_at, captured_at, Instant::now())
                });
            apm.process(samples, delay).ok()?;
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct WebRtcSettings {
    echo: bool,
    standard: bool,
    auto_gain: bool,
}

impl From<VoiceProcessing> for WebRtcSettings {
    fn from(settings: VoiceProcessing) -> Self {
        Self {
            echo: settings.echo_cancellation,
            standard: settings.noise_suppression == NoiseSuppression::Standard,
            auto_gain: settings.auto_gain,
        }
    }
}

impl WebRtcSettings {
    fn processor(self) -> Option<WebRtcProcessing> {
        (self.echo || self.standard || self.auto_gain).then(|| WebRtcProcessing {
            module: AudioProcessingModule::new(self.echo, self.auto_gain, false, self.standard),
            echo: self.echo,
        })
    }
}

fn stream_delay(
    playback: Instant,
    reverse_at: Instant,
    captured: Instant,
    process_at: Instant,
) -> Duration {
    // WebRTC expects the render and capture delays relative to its processing calls.
    let capture_delay = process_at.saturating_duration_since(captured);
    if playback >= reverse_at {
        capture_delay + playback.duration_since(reverse_at)
    } else {
        capture_delay.saturating_sub(reverse_at.duration_since(playback))
    }
}

struct WebRtcProcessing {
    module: AudioProcessingModule,
    echo: bool,
}

impl WebRtcProcessing {
    fn process(
        &mut self,
        samples: &mut [i16],
        delay: Duration,
    ) -> Result<(), livekit::webrtc::RtcError> {
        if self.echo {
            self.module
                .set_stream_delay_ms(delay.as_millis().min(500) as i32)?;
        }
        self.module.process_stream(samples, RATE as i32, 1)
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
    let mut captured_at = Instant::now();
    let mut fail = Some(fail);
    device.build_input_stream(
        config,
        move |samples: &[T], info| {
            let timestamp = info.timestamp();
            let started = Instant::now();
            let first_sample = started
                .checked_sub(timestamp.callback.duration_since(timestamp.capture))
                .unwrap_or(started);
            let captured = generation.load(Ordering::Relaxed);
            if last_generation != Some(captured) {
                last_generation = Some(captured);
                block.clear();
            }
            for (index, channel_frame) in samples.chunks_exact(channels).enumerate() {
                if block.is_empty() {
                    captured_at = first_sample
                        + Duration::from_secs_f64(index as f64 / f64::from(config.sample_rate));
                }
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
                            captured_at,
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
    fn combines_measured_reverse_and_capture_delay() {
        let reference = Instant::now();
        let at = |ms| reference + Duration::from_millis(ms);
        assert_eq!(
            stream_delay(at(30), at(10), at(32), at(45)),
            Duration::from_millis(33)
        );
        assert_eq!(
            stream_delay(at(10), at(15), at(20), at(30)),
            Duration::from_millis(5)
        );
        assert_eq!(stream_delay(at(5), at(20), at(22), at(25)), Duration::ZERO);
    }

    #[test]
    fn echo_cancellation_combines_with_automatic_gain_and_standard_noise_suppression() {
        let settings = VoiceProcessing::default();
        let mut processor = WebRtcSettings::from(settings).processor().unwrap();
        let mut without_echo = WebRtcSettings::from(VoiceProcessing {
            echo_cancellation: false,
            ..settings
        })
        .processor()
        .unwrap();
        let mut previous = [0; FRAME];
        let mut seed = 1_u32;
        let (mut input_power, mut output_power) = (0.0, 0.0);
        for frame in 0..500 {
            let mut render = std::array::from_fn::<_, FRAME, _>(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 16) as i16 / 4
            });
            let mut baseline = previous.map(|sample: i16| sample / 2);
            let mut capture = baseline;
            previous = render;
            without_echo.process(&mut baseline, Duration::ZERO).unwrap();
            if frame >= 400 {
                input_power += baseline
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>();
            }
            processor
                .module
                .process_reverse_stream(&mut render, RATE as i32, 1)
                .unwrap();
            processor
                .process(&mut capture, Duration::from_millis(10))
                .unwrap();
            if frame >= 400 {
                output_power += capture
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>();
            }
        }
        assert!(
            output_power < input_power / 10.0,
            "{output_power} >= {input_power} / 10"
        );
    }

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
