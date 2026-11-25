use std::{
    fs::{self, File},
    io::{Cursor, Read},
    num::NonZero,
    ops::Range,
    path::Path,
    sync::Arc,
    time::Duration,
};

use rodio::{Player, Source, buffer::SamplesBuffer};
use symphonia::core::{
    formats::{TrackType, probe::Hint},
    io::MediaSourceStream,
};
use tokio::{sync::watch, task::spawn_blocking, time};

use super::devices::{MediaOutput, open_media_output};

pub(crate) const MAX_SECONDS: u32 = 10;
const MAX_WAVEFORM_PEAKS: usize = 512;

use crate::api::MAX_SOUND_BYTES;
const MAX_SOURCE_BYTES: usize = 20 * 1024 * 1024;
const BAD_AUDIO: &str = "Choose a supported mono or stereo audio file.";
const BAD_CROP: &str = "Choose a selection within the file from 1 ms to 10 seconds.";

#[derive(Clone, Copy)]
pub struct Crop {
    pub start: f64,
    pub end: f64,
    pub gain: u16,
}

#[derive(Clone, Copy)]
pub enum Trim {
    Start,
    End,
    Region,
}

#[derive(Clone)]
pub struct Waveform {
    pub duration: f64,
    pub peaks: Arc<[f32]>,
    pub peak_width: f64,
}

pub(crate) struct PreparedSound {
    pub wav: Vec<u8>,
    pub duration_ms: u32,
}

pub(crate) struct SoundSource {
    source: Vec<u8>,
    rate: u32,
    channels: u16,
    frames: usize,
    pub waveform: Waveform,
}

impl SoundSource {
    pub fn open(path: &Path) -> Result<Self, &'static str> {
        let metadata = fs::metadata(path).map_err(|_| "Could not open the audio file.")?;
        if !metadata.is_file() {
            return Err("Choose an audio file.");
        }
        if metadata.len() > MAX_SOURCE_BYTES as u64 {
            return Err("Source audio must be 20 MB or smaller.");
        }
        let file = File::open(path).map_err(|_| "Could not open the audio file.")?;
        let mut source = Vec::new();
        file.take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut source)
            .map_err(|_| "Could not read the audio file.")?;
        Self::decode(source)
    }

    fn decode(source: Vec<u8>) -> Result<Self, &'static str> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err("Source audio must be 20 MB or smaller.");
        }
        let mut peaks = Vec::with_capacity(MAX_WAVEFORM_PEAKS);
        let mut frames_per_peak = 1;
        let (rate, channels, frames) = read_audio(
            &source,
            AudioRead::Waveform(&mut peaks, &mut frames_per_peak),
        )?;
        Ok(Self {
            source,
            rate,
            channels,
            frames,
            waveform: Waveform {
                duration: frames as f64 / f64::from(rate),
                peaks: peaks.into(),
                peak_width: frames_per_peak as f64 / frames as f64,
            },
        })
    }

    pub fn initial_crop(&self) -> Crop {
        Crop {
            start: 0.0,
            end: self.waveform.duration.min(f64::from(MAX_SECONDS)),
            gain: 100,
        }
    }

    pub fn trim(&self, crop: &mut Crop, target: Trim, seconds: f64) {
        let minimum = (self.rate as usize).div_ceil(1000);
        if self.frames < minimum || !seconds.is_finite() {
            return;
        }
        let maximum = self.rate as usize * MAX_SECONDS as usize;
        let mut start = self.frame(crop.start);
        let mut end = self.frame(crop.end);
        let value = self.frame(seconds);
        match target {
            Trim::Start => start = value.clamp(end.saturating_sub(maximum), end - minimum),
            Trim::End => end = value.clamp(start + minimum, (start + maximum).min(self.frames)),
            Trim::Region => {
                let length = end - start;
                start = value.min(self.frames - length);
                end = start + length;
            }
        }
        crop.start = start as f64 / f64::from(self.rate);
        crop.end = end as f64 / f64::from(self.rate);
    }

    fn frame(&self, seconds: f64) -> usize {
        (seconds * f64::from(self.rate)).round() as usize
    }

    pub fn crop(&self, crop: Crop) -> Result<PreparedSound, &'static str> {
        if !crop.start.is_finite()
            || !crop.end.is_finite()
            || crop.start < 0.0
            || crop.end > self.waveform.duration
            || crop.end <= crop.start
        {
            return Err(BAD_CROP);
        }
        if crop.gain > 200 {
            return Err("Upload gain must be between 0% and 200%.");
        }
        let rate = f64::from(self.rate);
        let start = self.frame(crop.start);
        let end = self.frame(crop.end);
        let frames = end - start;
        let duration_ms = (frames as f64 * 1000.0 / rate).round() as u32;
        if frames * 1000 < self.rate as usize || frames > self.rate as usize * MAX_SECONDS as usize
        {
            return Err(BAD_CROP);
        }
        let block = self.channels * 3;
        let size = frames * usize::from(block);
        let padded = size + size % 2;
        if 44 + padded > MAX_SOUND_BYTES {
            return Err("The cropped sound must be 3 MB or smaller.");
        }
        let mut wav = Vec::with_capacity(44 + padded);
        wav.extend(b"RIFF");
        wav.extend((36 + padded as u32).to_le_bytes());
        wav.extend(b"WAVEfmt \x10\0\0\0\x01\0");
        wav.extend(self.channels.to_le_bytes());
        wav.extend(self.rate.to_le_bytes());
        wav.extend((self.rate * u32::from(block)).to_le_bytes());
        wav.extend(block.to_le_bytes());
        wav.extend(b"\x18\0data");
        wav.extend((size as u32).to_le_bytes());
        let mut selection = Selection {
            frames: start..end,
            gain: f32::from(crop.gain) / 100.0,
            wav: &mut wav,
        };
        read_audio(&self.source, AudioRead::Crop(&mut selection))?;
        if wav.len() != 44 + size {
            return Err(BAD_AUDIO);
        }
        wav.resize(44 + padded, 0);
        Ok(PreparedSound { wav, duration_ms })
    }
}

struct Selection<'a> {
    frames: Range<usize>,
    gain: f32,
    wav: &'a mut Vec<u8>,
}

enum AudioRead<'a> {
    Waveform(&'a mut Vec<f32>, &'a mut usize),
    Crop(&'a mut Selection<'a>),
}

fn read_audio(source: &[u8], mut read: AudioRead<'_>) -> Result<(u32, u16, usize), &'static str> {
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            MediaSourceStream::new(Box::new(Cursor::new(source)), Default::default()),
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
    let mut frames = 0;
    let mut samples = Vec::new();
    while let Some(packet) = format.next_packet().map_err(|_| BAD_AUDIO)? {
        if packet.track_id != track_id {
            continue;
        }
        let audio = decoder.decode(&packet).map_err(|_| BAD_AUDIO)?;
        let rate = audio.spec().rate();
        let channels = audio.spec().channels().count();
        if !(8_000..=192_000).contains(&rate)
            || !(1..=2).contains(&channels)
            || spec.is_some_and(|spec| spec != (rate, channels))
        {
            return Err(BAD_AUDIO);
        }
        spec = Some((rate, channels));
        let next = frames + audio.frames();
        let (from, to) = match &read {
            AudioRead::Waveform(..) => (frames, next),
            AudioRead::Crop(selection) => (
                frames.max(selection.frames.start),
                next.min(selection.frames.end),
            ),
        };
        if from < to {
            samples.resize(audio.samples_interleaved(), 0.0_f32);
            audio.copy_to_slice_interleaved(&mut samples);
            let samples = &samples[(from - frames) * channels..(to - frames) * channels];
            match &mut read {
                AudioRead::Waveform(peaks, frames_per_peak) => {
                    for (index, frame) in samples.chunks_exact(channels).enumerate() {
                        let frame_index = from + index;
                        if frame_index / **frames_per_peak == MAX_WAVEFORM_PEAKS {
                            // Merge neighboring bins as the decoded duration grows.
                            for bin in 0..MAX_WAVEFORM_PEAKS / 2 {
                                peaks[bin] = peaks[bin * 2].max(peaks[bin * 2 + 1]);
                            }
                            peaks.truncate(MAX_WAVEFORM_PEAKS / 2);
                            **frames_per_peak *= 2;
                        }
                        let bin = frame_index / **frames_per_peak;
                        if bin == peaks.len() {
                            peaks.push(0.0);
                        }
                        for sample in frame {
                            peaks[bin] = peaks[bin].max(sample.abs().min(1.0));
                        }
                    }
                }
                AudioRead::Crop(selection) => {
                    for sample in samples {
                        let sample = sample * selection.gain;
                        if !(-1.0..=1.0).contains(&sample) {
                            return Err("Lower the upload gain to avoid clipping.");
                        }
                        let scale = if sample < 0.0 {
                            8_388_608.0
                        } else {
                            8_388_607.0
                        };
                        let pcm = (f64::from(sample) * scale).round() as i32;
                        selection.wav.extend(&pcm.to_le_bytes()[..3]);
                    }
                }
            }
        }
        frames = next;
        if matches!(&read, AudioRead::Crop(selection) if frames >= selection.frames.end) {
            break;
        }
    }
    let (rate, channels) = spec.ok_or(BAD_AUDIO)?;
    if frames == 0 {
        return Err(BAD_AUDIO);
    }
    Ok((rate, channels as u16, frames))
}

fn decode(bytes: &[u8]) -> Option<SamplesBuffer> {
    let header = bytes.get(..12)?;
    if &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return None;
    }

    let mut chunks = &bytes[12..];
    let mut format = None;
    let pcm = loop {
        let header = chunks.get(..8)?;
        let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        let (body, remaining) = chunks[8..].split_at_checked(size)?;
        match &header[..4] {
            b"fmt " => format = Some(body),
            b"data" => break body,
            _ => {}
        }
        chunks = remaining.get(size % 2..)?;
    };

    // shortcut: decode the uploaders' integer PCM, extend if SPK serves other WAV encodings.
    let format = format?.get(..16)?;
    if &format[..2] != b"\x01\0" {
        return None;
    }
    let channels = NonZero::new(u16::from_le_bytes(format[2..4].try_into().ok()?))
        .filter(|channels| channels.get() <= 2)?;
    let rate = NonZero::new(u32::from_le_bytes(format[4..8].try_into().ok()?))
        .filter(|rate| (8_000..=192_000).contains(&rate.get()))?;
    let sample_bytes = match u16::from_le_bytes(format[14..16].try_into().ok()?) {
        16 => 2,
        24 => 3,
        _ => return None,
    };
    let frame_bytes = usize::from(channels.get()) * sample_bytes;
    if pcm.is_empty() || !pcm.len().is_multiple_of(frame_bytes) {
        return None;
    }
    let samples = pcm
        .chunks_exact(sample_bytes)
        .take((rate.get() * MAX_SECONDS) as usize * usize::from(channels.get()))
        .map(|sample| {
            let bytes = if sample_bytes == 2 {
                [0, 0, sample[0], sample[1]]
            } else {
                [0, sample[0], sample[1], sample[2]]
            };
            i32::from_le_bytes(bytes) as f32 / 2_147_483_648.0
        })
        .collect::<Vec<_>>();
    Some(SamplesBuffer::new(channels, rate, samples))
}

pub(crate) async fn play(
    bytes: Vec<u8>,
    mut settings: watch::Receiver<MediaOutput>,
) -> Result<(), ()> {
    let mut clip = spawn_blocking(move || decode(&bytes))
        .await
        .map_err(|_| ())?
        .ok_or(())?;
    let mut position = Duration::ZERO;
    let deadline = time::sleep(Duration::from_secs(u64::from(MAX_SECONDS) + 1));
    tokio::pin!(deadline);
    loop {
        let opened_device = settings.borrow_and_update().device.clone();
        let device = opened_device.clone();
        let (failed, errors) = async_channel::bounded(1);
        let sink = spawn_blocking(move || {
            open_media_output(device.as_ref(), move |_| {
                let _ = failed.try_send(());
            })
        });
        let sink = tokio::select! {
            _ = &mut deadline => return Err(()),
            result = sink => result.map_err(|_| ())?.map_err(|_| ())?,
        };
        let output = settings.borrow().clone();
        if output.device != opened_device {
            continue;
        }
        let player = Player::connect_new(sink.mixer());
        clip.try_seek(position).map_err(|_| ())?;
        player.set_volume(output.volume);
        player.append(clip.clone());

        let mut tick = time::interval(Duration::from_millis(25));
        let mut ended_at = None;
        loop {
            tokio::select! {
                biased;
                changed = settings.changed() => {
                    changed.map_err(|_| ())?;
                    let output = settings.borrow_and_update();
                    if output.device != opened_device {
                        position += player.get_pos();
                        break;
                    }
                    player.set_volume(output.volume);
                }
                _ = errors.recv() => return Err(()),
                _ = &mut deadline => return if player.empty() { Ok(()) } else { Err(()) },
                _ = tick.tick() => {
                    if player.empty() {
                        // Let the device play the final samples already queued by CPAL.
                        let ended = ended_at.get_or_insert_with(time::Instant::now);
                        if ended.elapsed() >= Duration::from_millis(120) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_legacy_web_pcm() {
        let wav = b"RIFF\x2A\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xBB\0\0\0\x77\x01\0\x02\0\x10\0data\x06\0\0\0\0\x80\0\0\0\x40";

        let clip = decode(wav).expect("legacy SPK WAV should decode for playback");
        assert_eq!(clip.channels().get(), 1);
        assert_eq!(clip.sample_rate().get(), 48_000);
        assert_eq!(clip.collect::<Vec<_>>(), [-1.0, 0.0, 0.5]);
    }

    #[test]
    fn decodes_pcm_with_padded_metadata_before_audio() {
        let wav = b"RIFF\x34\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xBB\0\0\0\x77\x01\0\x02\0\x10\0JUNK\x01\0\0\0x\0data\x06\0\0\0\0\x80\0\0\0\x40";

        let clip = decode(wav).expect("WAV metadata should not prevent playback");
        assert_eq!(clip.channels().get(), 1);
        assert_eq!(clip.sample_rate().get(), 48_000);
        assert_eq!(clip.collect::<Vec<_>>(), [-1.0, 0.0, 0.5]);
        assert!(decode(&wav[..wav.len() - 1]).is_none());
    }

    #[test]
    fn decodes_web_pcm_with_a_ten_second_limit() {
        let mut wav =
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xBB\0\0\x80\x32\x02\0\x03\0\x18\0data"
                .to_vec();
        let count = 48_000 * 11_u32;
        wav.extend((count * 3).to_le_bytes());
        wav.extend([0x00, 0x00, 0x80].repeat(count as usize));

        let mut clip = decode(&wav).unwrap();
        assert_eq!(clip.channels().get(), 1);
        assert_eq!(clip.sample_rate().get(), 48_000);
        assert_eq!(clip.total_duration(), Some(Duration::from_secs(10)));
        assert_eq!(clip.next(), Some(-1.0));

        wav[24..28].fill(0);
        assert!(decode(&wav).is_none());
    }
}

#[cfg(test)]
mod crop_tests {
    use super::*;

    #[test]
    fn prepares_only_the_selected_audio_with_baked_gain() {
        let source = b"RIFF\x34\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x10\0\0\0\0\x80\0\x80\0\x80\0\x80\0\x40\0\x40\0\x40\0\x40";
        let source = SoundSource::decode(source.to_vec()).unwrap();
        let clip = source
            .crop(Crop {
                start: 0.0,
                end: 0.001,
                gain: 50,
            })
            .unwrap();
        assert_eq!(clip.duration_ms, 1);
        assert_eq!(
            decode(&clip.wav).unwrap().collect::<Vec<_>>(),
            [-0.5, -0.5, -0.5, -0.5, 0.25, 0.25, 0.25, 0.25]
        );
        assert!(
            source
                .crop(Crop {
                    start: 0.0,
                    end: 0.002,
                    gain: 100
                })
                .is_err()
        );
        assert!(
            source
                .crop(Crop {
                    start: 0.0,
                    end: 0.001,
                    gain: 200
                })
                .is_err()
        );
        assert!(SoundSource::decode(b"not audio".to_vec()).is_err());
    }
}
