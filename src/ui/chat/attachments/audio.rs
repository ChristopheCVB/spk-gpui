use std::{
    io::Cursor,
    num::NonZero,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rodio::{MixerDeviceSink, Player, Source, buffer::SamplesBuffer};
use symphonia::core::{
    codecs::audio::well_known::CODEC_ID_OPUS,
    formats::{FormatReader, TrackType, probe::Hint},
    io::MediaSourceStream,
};
use zvidlib_container::container::open_audio_track;
use zvidlib_core::{
    AudioDecoder, AudioSampleReader, CancellationToken, EncodedAudioSample, Limits, SampleRange,
    io::MemorySource,
};
use zvidlib_opus::{NativeOpusDecoder, OPUS_SAMPLE_RATE, OpusHead, opus_packet_samples};

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
    pub(super) async fn decode_opus(
        source: &MemorySource,
        duration: Duration,
        limits: Limits,
    ) -> Result<Self, &'static str> {
        let limits = Limits {
            max_audio_channels: 8,
            ..limits
        };
        let (track, timing) = open_audio_track(source, 0, &limits)
            .await
            .map_err(|_| BAD_AUDIO)?;
        let head = track.opus_config().map_err(|_| BAD_AUDIO)?;
        let decoder = NativeOpusDecoder::new(&head, limits).map_err(|_| BAD_AUDIO)?;
        let packets = track
            .to_encoded_audio_samples(source, &limits)
            .await
            .map_err(|_| BAD_AUDIO)?;
        let channels = u16::from(head.channels);
        let mut reader = AudioSampleReader::new(
            decoder,
            packets,
            OPUS_SAMPLE_RATE,
            channels,
            timing,
            0,
            limits,
        )
        .map_err(|_| BAD_AUDIO)?;
        let frames = (duration.as_secs_f64() * f64::from(OPUS_SAMPLE_RATE)).floor() as u64;
        let end = reader.presentation_length().min(frames);
        if end == 0 {
            return Err(BAD_AUDIO);
        }
        let range = SampleRange::new(0, end).map_err(|_| BAD_AUDIO)?;
        let audio = reader
            .get_range(range, &CancellationToken::new())
            .map_err(|_| BAD_AUDIO)?;
        Ok(Self {
            samples: SamplesBuffer::new(
                NonZero::new(channels).ok_or(BAD_AUDIO)?,
                NonZero::new(OPUS_SAMPLE_RATE).ok_or(BAD_AUDIO)?,
                audio.samples,
            ),
            truncated: end < reader.presentation_length(),
        })
    }

    pub(super) fn decode_webm(bytes: Vec<u8>) -> Result<Self, &'static str> {
        futures::executor::block_on(Self::decode_opus(
            &MemorySource::new(bytes),
            Duration::from_secs(MAX_SECONDS as u64),
            Limits::default(),
        ))
    }

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
        if params.codec == CODEC_ID_OPUS {
            let head = OpusHead::from_identification_header(
                params.extra_data.as_deref().ok_or(BAD_AUDIO)?,
            )
            .map_err(|_| BAD_AUDIO)?;
            return Self::decode_ogg_opus(&mut *format, track_id, head, seconds);
        }
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
            let remaining = remaining_samples(seconds, rate, channels, samples.len());
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
    fn decode_ogg_opus(
        format: &mut dyn FormatReader,
        track_id: u32,
        head: OpusHead,
        seconds: usize,
    ) -> Result<Self, &'static str> {
        let mut decoder = NativeOpusDecoder::new(
            &head,
            Limits {
                max_audio_channels: 8,
                ..Limits::default()
            },
        )
        .map_err(|_| BAD_AUDIO)?;
        let cancel = CancellationToken::new();
        let channels = usize::from(head.channels);
        // Symphonia reports Opus pre-skip as a track delay and the end padding on each packet.
        let mut skip = u64::from(head.pre_skip);
        let mut samples = Vec::new();
        let mut truncated = false;
        while let Some(packet) = format.next_packet().map_err(|_| BAD_AUDIO)? {
            if packet.track_id != track_id {
                continue;
            }
            let frames = u64::from(opus_packet_samples(&packet.data).map_err(|_| BAD_AUDIO)?);
            let start = skip.min(frames);
            skip -= start;
            let end = frames.checked_sub(packet.trim_end.get()).ok_or(BAD_AUDIO)?;
            let pcm = decoder
                .decode(
                    &EncodedAudioSample {
                        decoded_range: SampleRange::new(0, frames).map_err(|_| BAD_AUDIO)?,
                        data: packet.data.into_vec(),
                    },
                    &cancel,
                )
                .map_err(|_| BAD_AUDIO)?;
            let pcm = &pcm.samples[start.min(end) as usize * channels..end as usize * channels];
            let remaining = remaining_samples(seconds, OPUS_SAMPLE_RATE, channels, samples.len());
            samples.extend_from_slice(&pcm[..remaining.min(pcm.len())]);
            if remaining < pcm.len() {
                truncated = true;
                break;
            }
        }
        if samples.is_empty() {
            return Err(BAD_AUDIO);
        }
        Ok(Self {
            samples: SamplesBuffer::new(
                NonZero::new(u16::from(head.channels)).ok_or(BAD_AUDIO)?,
                NonZero::new(OPUS_SAMPLE_RATE).unwrap(),
                samples,
            ),
            truncated,
        })
    }
}

fn remaining_samples(seconds: usize, rate: u32, channels: usize, decoded: usize) -> usize {
    let limit = (seconds * rate as usize * channels).min(MAX_SAMPLES / channels * channels);
    limit - decoded
}

pub(super) struct AudioPlayback {
    pub player: Player,
    _sink: MixerDeviceSink,
    failed: Arc<AtomicBool>,
}

impl AudioPlayback {
    pub fn open(output: MediaOutput) -> Result<Self, &'static str> {
        let failed = Arc::new(AtomicBool::new(false));
        let error = failed.clone();
        let sink = open_media_output(output.device.as_ref(), move |_| {
            error.store(true, Ordering::Relaxed);
        })
        .map_err(|_| BAD_OUTPUT)?;
        let player = Player::connect_new(sink.mixer());
        player.pause();
        player.set_volume(output.volume);
        Ok(Self {
            player,
            _sink: sink,
            failed,
        })
    }

    pub fn attach(&self, mut samples: SamplesBuffer, position: Duration, playing: bool) {
        let _ = samples.try_seek(position);
        self.player.append(samples);
        if playing {
            self.player.play();
        }
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioClip, MAX_SECONDS};

    #[test]
    fn previews_ogg_opus_and_trims_encoder_delay_and_padding() {
        use rodio::Source;

        let bytes = include_bytes!("../../../../tests/fixtures/attachments/preview.ogg");
        let clip = AudioClip::decode(bytes).expect("Ogg Opus attachment should play in-app");
        assert_eq!(clip.samples.sample_rate().get(), 48_000);
        assert_eq!(clip.samples.clone().count(), 12_000);
        assert!(clip.samples.take(4_800).any(|sample| sample.abs() > 0.05));
        assert!(!clip.truncated);
    }

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
