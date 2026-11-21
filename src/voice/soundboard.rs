use std::{num::NonZero, time::Duration};

use rodio::{Player, Source, buffer::SamplesBuffer};
use tokio::{sync::watch, task::spawn_blocking, time};

use super::devices::{MediaOutput, open_media_output};

const MAX_SECONDS: u32 = 10;

fn decode(bytes: &[u8]) -> Option<SamplesBuffer> {
    // shortcut: decode the web uploaders' fixed PCM layout, extend the decoder if the server adds other layouts.
    let header = bytes.get(..44)?;
    if &header[..4] != b"RIFF"
        || &header[8..16] != b"WAVEfmt "
        || &header[16..22] != b"\x10\0\0\0\x01\0"
        || &header[36..40] != b"data"
    {
        return None;
    }

    let channels = NonZero::new(u16::from_le_bytes(header[22..24].try_into().ok()?))
        .filter(|channels| channels.get() <= 2)?;
    let rate = NonZero::new(u32::from_le_bytes(header[24..28].try_into().ok()?))
        .filter(|rate| (8_000..=192_000).contains(&rate.get()))?;
    let sample_bytes = match u16::from_le_bytes(header[34..36].try_into().ok()?) {
        16 => 2,
        24 => 3,
        _ => return None,
    };
    let size = u32::from_le_bytes(header[40..44].try_into().ok()?) as usize;
    let frame_bytes = usize::from(channels.get()) * sample_bytes;
    if size == 0 || !size.is_multiple_of(frame_bytes) {
        return None;
    }
    let pcm = bytes.get(44..)?.get(..size)?;
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
