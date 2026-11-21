use std::{f32::consts::TAU, num::NonZero, time::Duration};

use rodio::{DeviceSinkBuilder, buffer::SamplesBuffer};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sound {
    Join,
    Leave,
    Mute,
    Deafen,
    Camera,
    Watch,
}

impl Sound {
    pub const ALL: [Self; 6] = [
        Self::Join,
        Self::Leave,
        Self::Mute,
        Self::Deafen,
        Self::Camera,
        Self::Watch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Join => "Join sound",
            Self::Leave => "Leave sound",
            Self::Mute => "Mute sound",
            Self::Deafen => "Deafen sound",
            Self::Camera => "Camera sound",
            Self::Watch => "Watch sound",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct SoundSetting {
    pub enabled: bool,
    pub volume: u8,
}

impl Default for SoundSetting {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 50,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct SoundSettings {
    pub join: SoundSetting,
    pub leave: SoundSetting,
    pub mute: SoundSetting,
    pub deafen: SoundSetting,
    pub camera: SoundSetting,
    pub watch: SoundSetting,
}

impl SoundSettings {
    pub fn get_mut(&mut self, sound: Sound) -> &mut SoundSetting {
        match sound {
            Sound::Join => &mut self.join,
            Sound::Leave => &mut self.leave,
            Sound::Mute => &mut self.mute,
            Sound::Deafen => &mut self.deafen,
            Sound::Camera => &mut self.camera,
            Sound::Watch => &mut self.watch,
        }
    }

    pub fn get(mut self, sound: Sound) -> SoundSetting {
        *self.get_mut(sound)
    }
}

pub struct VoiceSounds {
    settings: watch::Receiver<SoundSettings>,
    playback: std::sync::mpsc::SyncSender<Cue>,
}

impl VoiceSounds {
    pub fn new(settings: watch::Receiver<SoundSettings>) -> Self {
        let (playback, cues) = std::sync::mpsc::sync_channel::<Cue>(16);
        std::thread::spawn(move || {
            let mut sink = None;
            loop {
                use std::sync::mpsc::RecvTimeoutError;
                let next = if sink.is_some() {
                    cues.recv_timeout(Duration::from_millis(500))
                } else {
                    cues.recv().map_err(|_| RecvTimeoutError::Disconnected)
                };
                let cue = match next {
                    Ok(cue) => cue,
                    Err(RecvTimeoutError::Timeout) => {
                        sink = None;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                if sink.is_none() {
                    sink = DeviceSinkBuilder::from_default_device()
                        .and_then(|builder| builder.open_sink_or_fallback())
                        .map(|mut sink| {
                            sink.log_on_drop(false);
                            sink
                        })
                        .ok();
                }
                if let Some(sink) = &sink {
                    sink.mixer().add(SamplesBuffer::new(
                        NonZero::new(1).unwrap(),
                        NonZero::new(SAMPLE_RATE).unwrap(),
                        cue.samples(),
                    ));
                }
            }
        });
        Self { settings, playback }
    }

    pub fn play(&self, sound: Sound, rising: bool) {
        let setting = self.settings.borrow().get(sound);
        if setting.enabled && setting.volume > 0 {
            let _ = self.playback.try_send(Cue {
                sound,
                rising,
                volume: setting.volume.min(100),
            });
        }
    }
}

const SAMPLE_RATE: u32 = 48_000;

struct Cue {
    sound: Sound,
    rising: bool,
    volume: u8,
}

impl Cue {
    fn notes(&self) -> &'static [f32] {
        // Match SPK web's useVoiceSettings.ts voice sounds.
        match (self.sound, self.rising) {
            (Sound::Join, _) => &[523.0, 784.0],
            (Sound::Leave, _) => &[784.0, 523.0],
            (Sound::Mute, true) => &[330.0, 415.0],
            (Sound::Mute, false) => &[415.0, 262.0],
            (Sound::Deafen, true) => &[262.0, 330.0, 392.0],
            (Sound::Deafen, false) => &[392.0, 294.0, 196.0],
            (Sound::Camera, true) => &[494.0, 622.0, 740.0],
            (Sound::Camera, false) => &[740.0, 622.0, 494.0],
            (Sound::Watch, true) => &[466.0, 622.0],
            (Sound::Watch, false) => &[622.0, 466.0],
        }
    }

    fn samples(&self) -> Vec<f32> {
        const SPACING: usize = SAMPLE_RATE as usize * 80 / 1000;
        const NOTE_LENGTH: usize = SAMPLE_RATE as usize * 130 / 1000;
        let notes = self.notes();
        let mut samples = vec![0.0; (notes.len() - 1) * SPACING + NOTE_LENGTH];
        let gain = 0.3 * f32::from(self.volume) / 100.0;
        let triangle = matches!(self.sound, Sound::Mute | Sound::Deafen);
        for (note, frequency) in notes.iter().enumerate() {
            for (frame, sample) in samples[note * SPACING..][..NOTE_LENGTH]
                .iter_mut()
                .enumerate()
            {
                let time = frame as f32 / SAMPLE_RATE as f32;
                let wave = (TAU * frequency * time).sin();
                let wave = if triangle {
                    wave.asin() * (4.0 / TAU)
                } else if self.sound == Sound::Watch {
                    2.0 * (time * frequency).fract() - 1.0
                } else {
                    wave
                };
                *sample += wave * gain * (0.001 / gain).powf((time / 0.12).min(1.0));
            }
        }
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_have_web_pitch_order_duration_and_headroom() {
        let cue = |sound, rising| Cue {
            sound,
            rising,
            volume: 50,
        };
        assert_eq!(cue(Sound::Join, true).notes(), &[523.0, 784.0]);
        assert_eq!(cue(Sound::Leave, false).notes(), &[784.0, 523.0]);
        assert_eq!(cue(Sound::Mute, true).notes(), &[330.0, 415.0]);
        assert_eq!(cue(Sound::Mute, false).notes(), &[415.0, 262.0]);
        assert_eq!(cue(Sound::Deafen, true).notes(), &[262.0, 330.0, 392.0]);
        let deafen = cue(Sound::Deafen, false);
        assert_eq!(deafen.notes(), &[392.0, 294.0, 196.0]);
        let samples = deafen.samples();
        assert_eq!(samples.len(), 13_920);
        assert!(samples.iter().any(|sample| sample.abs() > 0.1));
        assert!(samples.iter().all(|sample| sample.abs() <= 0.3));
    }
}
