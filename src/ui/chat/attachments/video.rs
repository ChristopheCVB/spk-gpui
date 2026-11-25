mod h264;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use gpui_kit::RenderImage;
use image::{DynamicImage, RgbaImage};

use crate::render_image::bgra_frame;
use zvidlib_container::container::open_tracks;
use zvidlib_core::{
    Limits,
    codec::{
        CancellationToken, CodecProfile, HardwarePreference, TrackKind, VideoDecoder,
        VideoDecoderConfig, VideoDecoderFactory,
    },
    io::MemorySource,
    media::{Codec, ColorRange, PixelFormat, VideoFrame},
};

use super::audio::AudioClip;
use h264::H264Preview;

// shortcut: cache a short, reduced-size clip, stream frames when full in-app playback is needed.
const MAX_SECONDS: usize = 8;
const FRAME_INTERVAL: Duration = Duration::from_millis(80);
const MAX_DECODE_PACKETS: usize = 480;
const BAD_VIDEO: &str = "Video preview unavailable. Try opening the file.";

pub(super) struct VideoClip {
    frames: Vec<(Duration, Arc<RenderImage>)>,
    pub duration: Duration,
    pub truncated: bool,
    pub audio: Option<AudioClip>,
    pub silent: bool,
}

impl VideoClip {
    pub fn decode(bytes: Vec<u8>) -> Result<Self, &'static str> {
        futures::executor::block_on(Self::read(MemorySource::new(bytes)))
    }

    async fn read(source: MemorySource) -> Result<Self, &'static str> {
        let limits = Limits {
            max_width: 1920,
            max_height: 1920,
            max_allocation_bytes: 32 * 1024 * 1024,
            ..Limits::default()
        };
        let tracks = open_tracks(&source, &limits).await.map_err(|_| BAD_VIDEO)?;
        let track = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .ok_or(BAD_VIDEO)?;
        let mut decoder = match track.codec {
            Codec::H264 => PreviewDecoder::H264(H264Preview::new(&track.decoder_config, limits)?),
            Codec::Vp8 | Codec::Vp9 => {
                let config = VideoDecoderConfig {
                    codec: track.codec,
                    profile: if track.codec == Codec::Vp8 {
                        CodecProfile::Vp8
                    } else {
                        CodecProfile::Vp9Profile0
                    },
                    coded_dimensions: track.dimensions.ok_or(BAD_VIDEO)?,
                    output_format: PixelFormat::Rgba8,
                    color_range: ColorRange::Limited,
                    hardware: HardwarePreference::Avoid,
                    configuration: track.decoder_config.clone(),
                };
                let decoder = if track.codec == Codec::Vp8 {
                    zvidlib_vp8::native_vp8_video_decoder_factory().create(&config, &limits)
                } else {
                    zvidlib_vp9_decoder::native_vp9_video_decoder_factory().create(&config, &limits)
                };
                PreviewDecoder::Vp(decoder.map_err(|_| BAD_VIDEO)?)
            }
            _ => return Err(BAD_VIDEO),
        };
        let first_pts = track.presentation_sample(0).ok_or(BAD_VIDEO)?.pts;
        let time = |ticks: u64| {
            Duration::try_from_secs_f64(ticks as f64 / f64::from(track.timescale))
                .map_err(|_| BAD_VIDEO)
        };
        let max_duration = Duration::from_secs(MAX_SECONDS as u64);
        let mut frames = BTreeMap::new();
        let mut duration = Duration::ZERO;
        let mut truncated = false;
        let cancel = CancellationToken::new();
        for (index, packet) in track
            .to_encoded_video_samples(&source, &limits)
            .await
            .map_err(|_| BAD_VIDEO)?
            .iter()
            .enumerate()
        {
            let timing = track
                .presentation_sample(packet.presentation_index.0 as usize)
                .map(|sample| {
                    let start = time(sample.pts.abs_diff(first_pts))?;
                    let end = start.saturating_add(time(u64::from(sample.duration))?);
                    Ok::<_, &'static str>((start, end))
                })
                .transpose()?;
            let beyond_preview = timing.is_some_and(|(start, _)| start >= max_duration);
            if index >= MAX_DECODE_PACKETS || (beyond_preview && packet.random_access) {
                truncated = true;
                break;
            }
            let slot = timing
                .filter(|_| !beyond_preview)
                .map(|(start, _)| start.as_millis() / FRAME_INTERVAL.as_millis());
            let show = slot.zip(timing).is_some_and(|(slot, (candidate, _))| {
                frames
                    .get(&slot)
                    .is_none_or(|(start, _)| candidate < *start)
            });
            let image = match &mut decoder {
                PreviewDecoder::H264(decoder) => decoder.submit(&packet.data, show)?,
                PreviewDecoder::Vp(decoder) => {
                    decoder.set_output_wanted(show);
                    let mut pictures = decoder.submit(packet, &cancel).map_err(|_| BAD_VIDEO)?;
                    pictures
                        .pop()
                        .map(|picture| vp_image(picture.frame))
                        .transpose()?
                }
            };
            if let Some((start, end)) = timing {
                duration = duration.max(end.min(max_duration));
                truncated |= end > max_duration;
                if let (Some(slot), Some(image)) = (slot, image) {
                    frames.insert(slot, (start, image));
                }
            }
        }
        if frames.is_empty() || duration.is_zero() {
            return Err(BAD_VIDEO);
        }
        let audio_track = tracks.iter().find(|track| track.kind == TrackKind::Audio);
        let audio = match audio_track {
            Some(track) if track.codec == Codec::Opus => {
                AudioClip::decode_opus(&source, duration, limits).await.ok()
            }
            Some(_) => AudioClip::decode_limited(source.as_slice(), MAX_SECONDS).ok(),
            None => None,
        };
        Ok(Self {
            frames: frames.into_values().collect(),
            duration,
            truncated,
            silent: audio_track.is_some() && audio.is_none(),
            audio,
        })
    }

    fn frame_at(&self, position: Duration) -> usize {
        self.frames
            .partition_point(|(start, _)| *start <= position)
            .saturating_sub(1)
    }

    pub fn image_at(&self, position: Duration) -> Arc<RenderImage> {
        self.frames[self.frame_at(position)].1.clone()
    }

    pub fn images(self) -> impl Iterator<Item = Arc<RenderImage>> {
        self.frames.into_iter().map(|(_, image)| image)
    }
}

enum PreviewDecoder {
    H264(H264Preview),
    Vp(Box<dyn VideoDecoder>),
}

fn vp_image(frame: VideoFrame) -> Result<Arc<RenderImage>, &'static str> {
    let [plane] = frame.planes.try_into().map_err(|_| BAD_VIDEO)?;
    let dimensions = frame.dimensions;
    if plane.stride != dimensions.width as usize * 4 {
        return Err(BAD_VIDEO);
    }
    let image =
        RgbaImage::from_raw(dimensions.width, dimensions.height, plane.data).ok_or(BAD_VIDEO)?;
    Ok(preview_image(image))
}

fn preview_image(image: RgbaImage) -> Arc<RenderImage> {
    let image = DynamicImage::ImageRgba8(image)
        .thumbnail(384, 240)
        .into_rgba8();
    Arc::new(RenderImage::new(vec![bgra_frame(image)]))
}

#[cfg(test)]
mod tests {
    use super::VideoClip;
    use rodio::Source;
    use std::time::Duration;

    #[test]
    fn previews_multiple_frames_and_opus_audio_from_a_webm_attachment() {
        let bytes = include_bytes!("../../../../tests/fixtures/attachments/preview.webm");
        let clip = VideoClip::decode(bytes.to_vec()).expect("VP9 attachment should play in-app");
        assert_eq!(clip.duration, Duration::from_secs(1));
        assert!(clip.frame_at(Duration::from_millis(800)) > 0);
        let first = clip.frames[0].1.as_bytes(0).unwrap();
        let later = clip.frames[clip.frame_at(Duration::from_millis(800))]
            .1
            .as_bytes(0)
            .unwrap();
        assert_ne!(first, later);
        assert!(!clip.silent);
        let audio = clip.audio.expect("WebM Opus soundtrack should play in-app");
        assert_eq!(audio.samples.sample_rate().get(), 48_000);
        assert_eq!(audio.samples.clone().count(), 48_000);
        assert!(audio.samples.take(4800).any(|sample| sample.abs() > 0.05));
        assert!(VideoClip::decode(b"broken video".to_vec()).is_err());
    }

    #[test]
    fn previews_vp9_mp4_with_an_aac_soundtrack() {
        let bytes = include_bytes!("../../../../tests/fixtures/attachments/preview.mp4");
        let clip =
            VideoClip::decode(bytes.to_vec()).expect("VP9 MP4 attachment should play in-app");
        assert!(clip.frame_at(Duration::from_millis(800)) > 0);
        assert!(clip.audio.is_some());
        assert!(!clip.silent);
    }

    #[test]
    fn checks_h264_coded_dimensions_before_decoding() {
        use super::{H264Preview, Limits, MemorySource, TrackKind, open_tracks};

        let bytes = include_bytes!("../../../../tests/fixtures/attachments/preview-avc.mp4");
        let source = MemorySource::new(bytes.to_vec());
        let tracks = futures::executor::block_on(open_tracks(&source, &Limits::default())).unwrap();
        let config = &tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .unwrap()
            .decoder_config;
        let limits = Limits {
            max_width: 32,
            ..Limits::default()
        };
        assert!(H264Preview::new(config, limits).is_err());
    }

    #[test]
    fn previews_h264_in_presentation_order_with_an_aac_soundtrack() {
        let bytes = include_bytes!("../../../../tests/fixtures/attachments/preview-avc.mp4");
        let clip =
            VideoClip::decode(bytes.to_vec()).expect("H.264 MP4 attachment should play in-app");
        assert_eq!(clip.duration, Duration::from_secs(1));
        assert!(
            clip.frames
                .windows(2)
                .all(|frames| frames[0].0 < frames[1].0)
        );
        assert!(clip.frame_at(Duration::from_millis(800)) > 0);
        assert_ne!(
            clip.frames[0].1.as_bytes(0),
            clip.image_at(Duration::from_millis(800)).as_bytes(0)
        );
        assert!(clip.audio.is_some());
        assert!(!clip.silent);
    }
}
