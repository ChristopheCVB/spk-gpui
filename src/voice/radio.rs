use std::{
    io::{self, Cursor, Read, Seek, SeekFrom},
    num::NonZero,
    sync::Arc,
    time::Duration,
};

use async_channel::{Receiver, Sender};
use oxideav_aac::{adts::ADTS_SAMPLE_RATES_HZ, asc::AudioSpecificConfig, decode::StreamDecoder};
use reqwest::{Client, redirect::Policy};
use rodio::{Player, buffer::SamplesBuffer};
use symphonia::core::{
    codecs::audio::{AudioCodecParameters, AudioDecoder, well_known::CODEC_ID_AAC},
    errors::Error as DecodeError,
    formats::{TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
    packet::Packet,
};
use tokio::{sync::watch, task::JoinHandle, time};
use url::Url;

use crate::api::{PublicDns, public_media_url};

use super::devices::{MediaOutput, open_media_output};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RadioPlayback {
    #[default]
    Idle,
    Connecting,
    Playing,
    Unavailable,
}

pub(crate) struct RadioPlayer {
    task: JoinHandle<()>,
    output: watch::Sender<MediaOutput>,
    pub playback: watch::Receiver<RadioPlayback>,
}

impl RadioPlayer {
    pub fn start(url: Option<String>, initial_output: MediaOutput) -> Self {
        let (output, settings) = watch::channel(initial_output);
        let (status, playback) = watch::channel(RadioPlayback::Connecting);
        let task = tokio::spawn(async move {
            if let Some(url) = url
                .and_then(|url| Url::parse(&url).ok())
                .filter(public_media_url)
            {
                play(url, settings, &status).await;
            }
            status.send_replace(RadioPlayback::Unavailable);
        });
        Self {
            task,
            output,
            playback,
        }
    }

    pub fn set_output(&self, output: MediaOutput) {
        self.output.send_if_modified(|current| {
            if *current == output {
                return false;
            }
            *current = output;
            true
        });
    }
}

impl Drop for RadioPlayer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn play(
    url: Url,
    mut settings: watch::Receiver<MediaOutput>,
    status: &watch::Sender<RadioPlayback>,
) {
    let (bytes, stream) = async_channel::bounded(8);
    let (samples, audio) = async_channel::bounded(8);
    tokio::task::spawn_blocking(move || decode(StationStream(stream, Cursor::default()), &samples));
    let playback = play_samples(audio, &mut settings, status);
    tokio::pin!(playback);
    // Ending playback drops the download and closes the decoder's input.
    tokio::select! {
        _ = download(url, bytes) => playback.await,
        _ = &mut playback => {}
    }
}

async fn download(url: Url, bytes: Sender<bytes::Bytes>) {
    let client = Client::builder()
        .https_only(true)
        .no_proxy()
        .referer(false)
        .connect_timeout(Duration::from_secs(8))
        .read_timeout(Duration::from_secs(10))
        .dns_resolver(Arc::new(PublicDns))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() > 5 || !public_media_url(attempt.url()) {
                attempt.error("Unsupported radio redirect")
            } else {
                attempt.follow()
            }
        }))
        .build();
    let Ok(client) = client else { return };
    let Ok(response) = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    else {
        return;
    };
    let mut response = response;
    loop {
        tokio::select! {
            _ = bytes.closed() => return,
            chunk = response.chunk() => {
                let Ok(Some(chunk)) = chunk else { return };
                if bytes.send(chunk).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn play_samples(
    samples: Receiver<SamplesBuffer>,
    settings: &mut watch::Receiver<MediaOutput>,
    status: &watch::Sender<RadioPlayback>,
) {
    let deadline = time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    let mut output = settings.borrow_and_update().clone();
    let mut sink = None;
    let mut playing = false;
    loop {
        if sink.is_none() {
            let device = output.device.clone();
            let (failed, errors) = async_channel::bounded(1);
            let opened = tokio::task::spawn_blocking(move || {
                open_media_output(device.as_ref(), move |_| {
                    let _ = failed.try_send(());
                })
            })
            .await;
            let Ok(Ok(device_sink)) = opened else { return };
            let player = Player::connect_new(device_sink.mixer());
            player.set_volume(output.volume);
            sink = Some((device_sink, player, errors));
        }
        let (_, player, errors) = sink.as_ref().expect("output opened");
        tokio::select! {
            biased;
            changed = settings.changed() => {
                if changed.is_err() { return; }
                let next = settings.borrow_and_update().clone();
                if next.device != output.device {
                    sink = None;
                } else {
                    player.set_volume(next.volume);
                }
                output = next;
            }
            _ = errors.recv() => return,
            _ = &mut deadline => return,
            block = samples.recv(), if player.len() < 6 => {
                let Ok(block) = block else { return };
                player.append(block);
                deadline.as_mut().reset(time::Instant::now() + Duration::from_secs(15));
                if !playing {
                    playing = true;
                    status.send_replace(RadioPlayback::Playing);
                }
            }
            _ = time::sleep(Duration::from_millis(10)), if player.len() >= 6 => {}
        }
    }
}

struct StationStream(Receiver<bytes::Bytes>, Cursor<bytes::Bytes>);

impl Read for StationStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while self.1.position() as usize == self.1.get_ref().len() {
            let Ok(chunk) = self.0.recv_blocking() else {
                return Ok(0);
            };
            self.1 = Cursor::new(chunk);
        }
        self.1.read(buffer)
    }
}

impl Seek for StationStream {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl MediaSource for StationStream {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

fn decode(
    stream: impl MediaSource + 'static,
    samples: &Sender<SamplesBuffer>,
) -> Result<(), DecodeError> {
    let mut format = symphonia::default::get_probe().probe(
        &Hint::new(),
        MediaSourceStream::new(Box::new(stream), Default::default()),
        Default::default(),
        Default::default(),
    )?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or(DecodeError::Unsupported("radio stream has no audio track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or(DecodeError::Unsupported(
            "radio stream codec is unavailable",
        ))?;
    let mut decoder = if params.codec == CODEC_ID_AAC {
        StationDecoder::Aac(Box::new(Aac::new(params)?))
    } else {
        StationDecoder::Mp3(
            symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())?,
        )
    };
    let track_id = track.id;
    while !samples.is_closed()
        && let Some(packet) = format.next_packet()?
    {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(Some(block)) => {
                if samples.send_blocking(block).is_err() {
                    break;
                }
            }
            Ok(None) | Err(DecodeError::DecodeError(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

enum StationDecoder {
    Mp3(Box<dyn AudioDecoder>),
    Aac(Box<Aac>),
}

impl StationDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<Option<SamplesBuffer>, DecodeError> {
        let (channels, rate, pcm) = match self {
            Self::Aac(aac) => {
                let frame = aac.read(&packet.data)?;
                let pcm = frame
                    .pcm
                    .into_iter()
                    .map(|sample| f32::from(sample) / 32768.0)
                    .collect();
                (frame.channels, frame.sample_rate, pcm)
            }
            Self::Mp3(decoder) => {
                let audio = decoder.decode(packet)?;
                let spec = audio.spec();
                let mut pcm = vec![0.0; audio.samples_interleaved()];
                audio.copy_to_slice_interleaved(&mut pcm);
                (spec.channels().count(), spec.rate(), pcm)
            }
        };
        if pcm.is_empty() {
            return Ok(None);
        }
        let channels = u16::try_from(channels)
            .ok()
            .and_then(NonZero::new)
            .ok_or(DecodeError::Unsupported("invalid radio channels"))?;
        let rate =
            NonZero::new(rate).ok_or(DecodeError::Unsupported("invalid radio sample rate"))?;
        Ok(Some(SamplesBuffer::new(channels, rate, pcm)))
    }
}

struct Aac {
    stream: StreamDecoder,
    object_type: u8,
    frequency_index: u8,
    sample_rate: u32,
    channel_config: u8,
}

impl Aac {
    fn new(params: &AudioCodecParameters) -> Result<Self, DecodeError> {
        let mut stream = StreamDecoder::new();
        if let Some(extra) = &params.extra_data {
            let (config, _) = AudioSpecificConfig::parse(extra)
                .map_err(|_| DecodeError::Unsupported("invalid radio AAC config"))?;
            stream.set_frame_family(config.ga_body.frame_length.family(config.aot));
            stream.set_sbr_downsampled(config.extension_sample_rate == Some(config.sample_rate));
            if let Some(pce) = config.ga_body.pce {
                stream.set_program_config(pce);
            }
            return Ok(Self {
                stream,
                object_type: config.aot,
                frequency_index: config.sampling_frequency_index,
                sample_rate: config.sample_rate,
                channel_config: config.channel_configuration,
            });
        }
        // ADTS carries the AAC core rate and channel configuration in its transport header.
        let sample_rate = params.sample_rate.unwrap_or(0);
        let frequency_index = ADTS_SAMPLE_RATES_HZ
            .iter()
            .position(|&rate| rate == sample_rate)
            .ok_or(DecodeError::Unsupported("invalid radio AAC rate"))?
            as u8;
        let channel_config = match params
            .channels
            .as_ref()
            .map_or(0, |channels| channels.count())
        {
            count @ 0..=6 => count as u8,
            8 => 7,
            _ => return Err(DecodeError::Unsupported("invalid radio AAC channels")),
        };
        Ok(Self {
            stream,
            object_type: 2,
            frequency_index,
            sample_rate,
            channel_config,
        })
    }

    fn read(&mut self, payload: &[u8]) -> Result<oxideav_aac::decode::DecodedFrame, DecodeError> {
        self.stream
            .decode_raw_data_block(
                self.object_type,
                self.frequency_index,
                self.sample_rate,
                self.channel_config,
                1,
                payload,
            )
            .map_err(|_| DecodeError::DecodeError("invalid radio AAC frame"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_live_mp3_and_he_aac_into_audible_samples() {
        use rodio::Source;

        for (stream, channels) in [
            (&include_bytes!("../../tests/fixtures/radio.mp3")[..], 1),
            (&include_bytes!("../../tests/fixtures/radio-he.aac")[..], 2),
        ] {
            let (download, input) = async_channel::unbounded();
            for part in stream.chunks(47) {
                download
                    .send_blocking(bytes::Bytes::copy_from_slice(part))
                    .unwrap();
            }
            drop(download);
            let (pcm, samples) = async_channel::bounded(8);
            let decoder =
                std::thread::spawn(move || decode(StationStream(input, Cursor::default()), &pcm));
            let mut audible = false;
            while let Ok(frame) = samples.recv_blocking() {
                assert_eq!(frame.sample_rate().get(), 44100);
                assert_eq!(frame.channels().get(), channels);
                audible |= frame.into_iter().any(|sample| sample.abs() > 0.01);
            }
            decoder.join().unwrap().unwrap();
            assert!(audible);
        }
    }

    #[test]
    fn station_requests_use_public_https_without_authentication() {
        for url in [
            "http://example.com/radio",
            "https://foo:bar@example.com/radio",
            "https://example.com:8443/radio",
            "https://radio.local./stream",
            "https://169.254.169.254/radio",
            "https://100.64.1.2/radio",
            "https://[::ffff:127.0.0.1]/radio",
            "https://[64:ff9b::a00:1]/radio",
            "https://[fc00::1]/radio",
        ] {
            assert!(!public_media_url(&Url::parse(url).unwrap()), "{url}");
        }
        assert!(public_media_url(
            &Url::parse("https://example.com:443/music").unwrap()
        ));
    }
}
