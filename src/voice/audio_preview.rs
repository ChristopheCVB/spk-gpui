use std::{
    num::NonZero,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering::Relaxed},
    },
};

use rodio::{
    DeviceSinkBuilder, Player,
    buffer::SamplesBuffer,
    cpal::{
        self, FromSample, SampleFormat, SizedSample,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    },
};

use super::{Device, DeviceKind};

pub(crate) fn cpal_device(kind: DeviceKind, selected: Option<&Device>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let Some(selected) = selected else {
        return match kind {
            DeviceKind::Microphone => host.default_input_device(),
            _ => host.default_output_device(),
        };
    };
    let devices: Vec<_> = match kind {
        DeviceKind::Microphone => host.input_devices().ok()?.collect(),
        _ => host.output_devices().ok()?.collect(),
    };
    let selected_cpal_id = selected.id.parse().ok();
    if let Some(device) = devices.iter().find(|device| {
        device.id().is_ok_and(|id| {
            selected_cpal_id.as_ref() == Some(&id)
                || id.id() == selected.id
                || (cfg!(windows) && id.id().eq_ignore_ascii_case(&selected.id))
        })
    }) {
        return Some(device.clone());
    }
    let mut by_name = devices.into_iter().filter(|device| {
        device
            .description()
            .is_ok_and(|description| description.name() == selected.name)
    });
    let device = by_name.next()?;
    by_name.next().is_none().then_some(device)
}

pub(crate) struct MicrophoneTest {
    _stream: cpal::Stream,
    level: Arc<AtomicU8>,
    failed: Arc<AtomicBool>,
}

impl MicrophoneTest {
    pub(crate) async fn start(selected: Option<&Device>) -> Option<Self> {
        #[cfg(target_os = "macos")]
        if !super::media::microphone_permission().await {
            return None;
        }
        let device = cpal_device(DeviceKind::Microphone, selected)?;
        let config = device.default_input_config().ok()?;
        let level = Arc::new(AtomicU8::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match config.sample_format() {
            SampleFormat::I8 => meter::<i8>,
            SampleFormat::I16 => meter::<i16>,
            SampleFormat::I24 => meter::<cpal::I24>,
            SampleFormat::I32 => meter::<i32>,
            SampleFormat::I64 => meter::<i64>,
            SampleFormat::U8 => meter::<u8>,
            SampleFormat::U16 => meter::<u16>,
            SampleFormat::U24 => meter::<cpal::U24>,
            SampleFormat::U32 => meter::<u32>,
            SampleFormat::U64 => meter::<u64>,
            SampleFormat::F32 => meter::<f32>,
            SampleFormat::F64 => meter::<f64>,
            _ => return None,
        }(&device, config.config(), level.clone(), failed.clone())
        .ok()?;
        stream.play().ok()?;
        Some(Self {
            _stream: stream,
            level,
            failed,
        })
    }

    pub(crate) fn level(&self) -> Option<u8> {
        (!self.failed.load(Relaxed)).then(|| self.level.swap(0, Relaxed))
    }
}

fn meter<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    level: Arc<AtomicU8>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |samples: &[T], _| {
            let measured = microphone_level(samples.iter().copied().map(f32::from_sample_));
            level.fetch_max(measured, Relaxed);
        },
        move |error| {
            if error.kind() != cpal::ErrorKind::Xrun {
                failed.store(true, Relaxed);
            }
        },
        None,
    )
}

fn microphone_level(samples: impl ExactSizeIterator<Item = f32>) -> u8 {
    let count = samples.len().max(1) as f32;
    let sum = samples.map(|sample| sample * sample).sum::<f32>();
    ((sum / count).sqrt() * 400.0).min(100.0).round() as u8
}

pub(crate) fn test_playback(selected: Option<&Device>, volume: u8) -> Option<()> {
    let device = cpal_device(DeviceKind::Speakers, selected)?;
    let failed = Arc::new(AtomicBool::new(false));
    let sink_failed = failed.clone();
    let mut sink = DeviceSinkBuilder::from_device(device)
        .ok()?
        .with_error_callback(move |_| sink_failed.store(true, Relaxed))
        .open_stream()
        .ok()?;
    sink.log_on_drop(false);
    let (sample_rate, samples) = crate::sounds::test_tone(volume);
    let player = Player::connect_new(sink.mixer());
    player.append(SamplesBuffer::new(
        NonZero::new(1).unwrap(),
        NonZero::new(sample_rate).unwrap(),
        samples,
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !player.empty() && !failed.load(Relaxed) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !player.empty() {
        return None;
    }
    // Give the device time to play the final samples already queued by CPAL.
    std::thread::sleep(std::time::Duration::from_millis(120));
    (!failed.load(Relaxed)).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_meter_uses_rms_and_bounds_the_display() {
        assert_eq!(microphone_level([0.0, 0.0].into_iter()), 0);
        assert_eq!(microphone_level([0.125, -0.125].into_iter()), 50);
        assert_eq!(microphone_level([1.0, -1.0].into_iter()), 100);
    }
}
