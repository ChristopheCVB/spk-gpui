use std::{
    num::NonZero,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use rodio::{
    cpal::{
        self, BufferSize, FromSample, SampleFormat, SizedSample, SupportedBufferSize,
        SupportedStreamConfig,
        traits::{DeviceTrait, StreamTrait},
    },
    mixer::{Mixer, MixerSource, mixer},
};
use tokio::sync::broadcast;

use super::processing::{RATE, RenderBlock};

pub(super) struct VoiceOutput {
    _stream: cpal::Stream,
    mixer: Mixer,
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::voice::{DeviceKind, audio_preview::cpal_device};
    use rodio::{Player, buffer::SamplesBuffer};

    #[tokio::test]
    #[ignore = "requires the isolated stereo PulseAudio output from tests/fixtures/linux-screen-share.sh"]
    async fn linux_voice_output_feeds_the_post_gain_stereo_reference() {
        let device = cpal_device(DeviceKind::Speakers, None).unwrap();
        let (render, mut frames) = broadcast::channel(16);
        let failed = Arc::new(AtomicBool::new(false));
        let output = VoiceOutput::open(device, render, failed.clone()).unwrap();
        let player = Player::connect_new(output.mixer());
        player.set_volume(0.5);
        player.append(SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(RATE).unwrap(),
            [0.25, -0.5].repeat(RATE as usize / 10),
        ));

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let frame = frames.recv().await.unwrap();
                assert_eq!((frame.channels, frame.rate), (2, RATE));
                assert_eq!(frame.samples.len(), RATE as usize / 100 * 2);
                if frame.samples.as_chunks::<2>().0.contains(&[4096, -8192]) {
                    break;
                }
                assert!(!failed.load(Relaxed));
            }
        })
        .await
        .expect("voice output should provide stereo samples at the selected gain");
    }
}

impl VoiceOutput {
    pub(super) fn open(
        device: cpal::Device,
        render: broadcast::Sender<Arc<RenderBlock>>,
        failed: Arc<AtomicBool>,
    ) -> Option<Self> {
        let mut supported = device.default_output_config().ok()?;
        let native_rate = supported.sample_rate();
        if !(8_000..=192_000).contains(&native_rate) || !native_rate.is_multiple_of(100) {
            supported = device
                .supported_output_configs()
                .ok()?
                .find_map(|config| config.try_with_sample_rate(RATE))?;
        }
        if cfg!(windows) && supported.sample_rate() != RATE {
            // WASAPI can convert a continuous stream. Rodio's per-packet conversion loses
            // samples at the end of short voice packets.
            if let Some(output) =
                Self::open_at_rate(&device, &supported, RATE, render.clone(), failed.clone())
            {
                return Some(output);
            }
        }
        Self::open_at_rate(&device, &supported, supported.sample_rate(), render, failed)
    }

    fn open_at_rate(
        device: &cpal::Device,
        supported: &SupportedStreamConfig,
        rate: u32,
        render: broadcast::Sender<Arc<RenderBlock>>,
        failed: Arc<AtomicBool>,
    ) -> Option<Self> {
        let mut config = supported.config();
        config.sample_rate = rate;
        // Short PulseAudio callbacks keep the render reference close to the capture clock.
        if cfg!(target_os = "linux")
            && let SupportedBufferSize::Range { min, max } = *supported.buffer_size()
        {
            config.buffer_size = BufferSize::Fixed((config.sample_rate / 50).clamp(min, max));
        }
        let (mixer, pcm) = mixer(
            NonZero::new(config.channels)?,
            NonZero::new(config.sample_rate)?,
        );
        let stream = match supported.sample_format() {
            SampleFormat::I8 => open::<i8>,
            SampleFormat::I16 => open::<i16>,
            SampleFormat::I24 => open::<cpal::I24>,
            SampleFormat::I32 => open::<i32>,
            SampleFormat::I64 => open::<i64>,
            SampleFormat::U8 => open::<u8>,
            SampleFormat::U16 => open::<u16>,
            SampleFormat::U24 => open::<cpal::U24>,
            SampleFormat::U32 => open::<u32>,
            SampleFormat::U64 => open::<u64>,
            SampleFormat::F32 => open::<f32>,
            SampleFormat::F64 => open::<f64>,
            _ => return None,
        }(device, config, pcm, render, failed)
        .ok()?;
        stream.play().ok()?;
        Some(Self {
            _stream: stream,
            mixer,
        })
    }

    pub(super) fn mixer(&self) -> &Mixer {
        &self.mixer
    }
}

fn open<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut pcm: MixerSource,
    render: broadcast::Sender<Arc<RenderBlock>>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error> {
    let rate = config.sample_rate;
    let channels = usize::from(config.channels);
    let reference_channels = channels.min(2);
    let block_size = rate as usize / 100 * reference_channels;
    let mut samples = Vec::with_capacity(block_size);
    let mut playback = Instant::now();
    let stream = Arc::new(());

    device.build_output_stream(
        config,
        move |output: &mut [T], info| {
            let timestamp = info.timestamp();
            let callback = Instant::now();
            let latency = timestamp.playback.duration_since(timestamp.callback);
            let first_sample = callback.checked_add(latency).unwrap_or(callback);
            let reference = render.receiver_count() != 0;
            if !reference {
                samples.clear();
            }
            for (frame, output) in output.chunks_exact_mut(channels).enumerate() {
                if samples.is_empty() && reference {
                    playback =
                        first_sample + Duration::from_secs_f64(frame as f64 / f64::from(rate));
                }
                for (channel, out) in output.iter_mut().enumerate() {
                    let sample = pcm.next().unwrap_or(0.0).clamp(-1.0, 1.0);
                    *out = T::from_sample_(sample);
                    if reference && channel < reference_channels {
                        samples.push(i16::from_sample_(sample));
                    }
                }
                if samples.len() == block_size {
                    let _ = render.send(Arc::new(RenderBlock {
                        stream: stream.clone(),
                        playback,
                        rate,
                        channels: reference_channels as u32,
                        samples: std::mem::replace(&mut samples, Vec::with_capacity(block_size)),
                    }));
                }
            }
        },
        move |_| failed.store(true, Relaxed),
        None,
    )
}
