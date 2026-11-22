use std::{
    io::Cursor,
    num::NonZero,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use rodio::{MixerDeviceSink, Player, buffer::SamplesBuffer};
use symphonia::core::{
    formats::{TrackType, probe::Hint},
    io::MediaSourceStream,
};

use crate::voice::{MediaOutput, open_media_output};

const MAX_SECONDS: usize = 30;
const MAX_SAMPLES: usize = 30 * 96_000 * 2;

pub(super) const BAD_AUDIO: &str = "Audio preview unavailable. Try opening the file.";
pub(super) const BAD_OUTPUT: &str = "Could not play audio on the selected media output.";

pub(super) struct AudioClip {
    pub samples: SamplesBuffer,
    pub truncated: bool,
}

impl AudioClip {
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        Self::decode_limited(bytes, MAX_SECONDS)
    }

    pub fn decode_limited(bytes: &[u8], seconds: usize) -> Result<Self, &'static str> {
        let mut format = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default()),
                Default::default(),
                Default::default(),
            )
            .map_err(|_| BAD_AUDIO)?;
        let track = format.default_track(TrackType::Audio).ok_or(BAD_AUDIO)?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or(BAD_AUDIO)?;
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &Default::default())
            .map_err(|_| BAD_AUDIO)?;
        let mut spec = None;
        let mut samples = Vec::new();
        let mut packet_samples = Vec::new();
        let mut truncated = false;
        while let Some(packet) = format.next_packet().map_err(|_| BAD_AUDIO)? {
            if packet.track_id != track_id {
                continue;
            }
            let audio = decoder.decode(&packet).map_err(|_| BAD_AUDIO)?;
            let rate = audio.spec().rate();
            let channels = audio.spec().channels().count();
            if !(8_000..=192_000).contains(&rate)
                || !(1..=8).contains(&channels)
                || spec.is_some_and(|spec| spec != (rate, channels))
            {
                return Err(BAD_AUDIO);
            }
            spec = Some((rate, channels));
            // shortcut: bound decoded PCM to a short clip, stream packets when full playback is needed.
            let limit = (seconds * rate as usize * channels).min(MAX_SAMPLES / channels * channels);
            let remaining = limit - samples.len();
            let count = audio.samples_interleaved();
            if remaining > 0 {
                packet_samples.resize(count, 0.0);
                audio.copy_to_slice_interleaved(&mut packet_samples);
                samples.extend_from_slice(&packet_samples[..remaining.min(count)]);
            }
            if remaining < count {
                truncated = true;
                break;
            }
        }
        let (rate, channels) = spec.filter(|_| !samples.is_empty()).ok_or(BAD_AUDIO)?;
        Ok(Self {
            samples: SamplesBuffer::new(
                NonZero::new(channels as u16).ok_or(BAD_AUDIO)?,
                NonZero::new(rate).ok_or(BAD_AUDIO)?,
                samples,
            ),
            truncated,
        })
    }
}

pub(super) struct AudioPlayback {
    pub player: Player,
    _sink: MixerDeviceSink,
    failed: Arc<AtomicBool>,
}

impl AudioPlayback {
    pub fn open(samples: SamplesBuffer, output: MediaOutput) -> Result<Self, &'static str> {
        let failed = Arc::new(AtomicBool::new(false));
        let error = failed.clone();
        let sink = open_media_output(output.device.as_ref(), move |_| {
            error.store(true, Ordering::Relaxed);
        })
        .map_err(|_| BAD_OUTPUT)?;
        let player = Player::connect_new(sink.mixer());
        player.pause();
        player.set_volume(output.volume);
        player.append(samples);
        Ok(Self {
            player,
            _sink: sink,
            failed,
        })
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioClip, MAX_SECONDS};

    #[test]
    fn limits_decoded_audio_to_the_preview_length() {
        let rate = 8_000_u32;
        let samples = (MAX_SECONDS as u32 + 1) * rate;
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(b"\x02\0\x10\0data");
        wav.extend_from_slice(&(samples * 2).to_le_bytes());
        wav.resize(44 + samples as usize * 2, 0);

        let clip = AudioClip::decode(&wav).expect("long WAV should produce a bounded preview");
        assert!(clip.truncated);
        assert_eq!(clip.samples.count(), MAX_SECONDS * rate as usize);
    }
}
