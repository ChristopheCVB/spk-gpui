use std::{sync::Arc, time::Duration};

use gpui_kit::RenderImage;
use image::{DynamicImage, Frame, RgbaImage};
use zvidlib_container::container::open_tracks;
use zvidlib_core::{
    Limits,
    codec::{
        CancellationToken, CodecProfile, HardwarePreference, TrackKind, VideoDecoderConfig,
        VideoDecoderFactory,
    },
    io::MemorySource,
    media::{Codec, ColorRange, PixelFormat, VideoFrame},
};

use super::audio::AudioClip;

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
        let profile = match track.codec {
            Codec::Vp8 => CodecProfile::Vp8,
            Codec::Vp9 => CodecProfile::Vp9Profile0,
            _ => return Err(BAD_VIDEO),
        };
        let config = VideoDecoderConfig {
            codec: track.codec,
            profile,
            coded_dimensions: track.dimensions.ok_or(BAD_VIDEO)?,
            output_format: PixelFormat::Rgba8,
            color_range: ColorRange::Limited,
            hardware: HardwarePreference::Avoid,
            configuration: track.decoder_config.clone(),
        };
        let mut decoder = match track.codec {
            Codec::Vp8 => zvidlib_vp8::native_vp8_video_decoder_factory().create(&config, &limits),
            _ => zvidlib_vp9_decoder::native_vp9_video_decoder_factory().create(&config, &limits),
        }
        .map_err(|_| BAD_VIDEO)?;
        let first_pts = track.presentation_sample(0).ok_or(BAD_VIDEO)?.pts;
        let time = |ticks: u64| {
            Duration::try_from_secs_f64(ticks as f64 / f64::from(track.timescale))
                .map_err(|_| BAD_VIDEO)
        };
        let max_duration = Duration::from_secs(MAX_SECONDS as u64);
        let mut frames = Vec::new();
        let mut next_frame = Duration::ZERO;
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
            if index >= MAX_DECODE_PACKETS || timing.is_some_and(|(start, _)| start >= max_duration)
            {
                truncated = true;
                break;
            }
            let show = timing.is_some_and(|(start, _)| start >= next_frame);
            decoder.set_output_wanted(show);
            let pictures = decoder.submit(packet, &cancel).map_err(|_| BAD_VIDEO)?;
            if let Some((start, end)) = timing {
                duration = duration.max(end.min(max_duration));
                truncated |= end > max_duration;
                for picture in pictures {
                    frames.push((start, preview_image(picture.frame)?));
                    next_frame = start.saturating_add(FRAME_INTERVAL);
                }
            }
        }
        if frames.is_empty() || duration.is_zero() {
            return Err(BAD_VIDEO);
        }
        let has_audio = tracks.iter().any(|track| track.kind == TrackKind::Audio);
        let audio = has_audio
            .then(|| AudioClip::decode_limited(source.as_slice(), MAX_SECONDS).ok())
            .flatten();
        Ok(Self {
            frames,
            duration,
            truncated,
            silent: has_audio && audio.is_none(),
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

fn preview_image(frame: VideoFrame) -> Result<Arc<RenderImage>, &'static str> {
    let [plane] = frame.planes.try_into().map_err(|_| BAD_VIDEO)?;
    let dimensions = frame.dimensions;
    if plane.stride != dimensions.width as usize * 4 {
        return Err(BAD_VIDEO);
    }
    let image =
        RgbaImage::from_raw(dimensions.width, dimensions.height, plane.data).ok_or(BAD_VIDEO)?;
    let mut image = DynamicImage::ImageRgba8(image)
        .thumbnail(384, 240)
        .into_rgba8();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![Frame::new(image)])))
}

#[cfg(test)]
mod tests {
    use super::VideoClip;
    use std::time::Duration;

    #[test]
    fn previews_multiple_frames_of_a_webm_attachment() {
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
        assert!(VideoClip::decode(b"broken video".to_vec()).is_err());
    }

    #[test]
    fn previews_an_mp4_video_with_an_aac_soundtrack() {
        let bytes = include_bytes!("../../../../tests/fixtures/attachments/preview.mp4");
        let clip =
            VideoClip::decode(bytes.to_vec()).expect("VP9 MP4 attachment should play in-app");
        assert!(clip.frame_at(Duration::from_millis(800)) > 0);
        assert!(clip.audio.is_some());
        assert!(!clip.silent);
    }
}
