use std::{
    io::{BufRead, Cursor, Seek},
    sync::Arc,
    time::Duration,
};

use gif::{
    Extension, Repeat,
    streaming_decoder::{Block, Decoded, OutputBuffer, StreamingDecoder},
};
use gpui_kit::RenderImage;
use image::{AnimationDecoder, Frame, ImageDecoder, ImageFormat, ImageReader, Limits, RgbaImage};

const MAX_GIF_FRAMES: usize = 512;
const MAX_GIF_SOURCE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_GIF_PREVIEW_BYTES: usize = 64 * 1024 * 1024;

pub struct ChatImage {
    pub pixels: Arc<RenderImage>,
    frame_ends: Vec<Duration>,
    repeat: Repeat,
}

impl ChatImage {
    pub fn frame_at(&self, elapsed: Duration) -> (usize, Option<Duration>) {
        if self.frame_ends.len() <= 1 {
            return (0, None);
        }
        let cycle = self.frame_ends.last().unwrap().as_nanos();
        let elapsed = elapsed.as_nanos();
        if let Repeat::Finite(repeats) = self.repeat
            && elapsed / cycle > u128::from(repeats)
        {
            return (self.frame_ends.len() - 1, None);
        }

        let position = Duration::from_nanos((elapsed % cycle) as u64);
        let frame = self.frame_ends.partition_point(|end| *end <= position);
        (frame, Some(self.frame_ends[frame] - position))
    }
}

pub fn image_preview(bytes: &[u8]) -> Option<Arc<ChatImage>> {
    raster_thumbnail(bytes, 768, 512)
}

pub(super) fn raster_thumbnail(bytes: &[u8], width: u32, height: u32) -> Option<Arc<ChatImage>> {
    thumbnail(Cursor::new(bytes), width, height)
}

pub(super) fn thumbnail(
    reader: impl BufRead + Seek,
    width: u32,
    height: u32,
) -> Option<Arc<ChatImage>> {
    let reader = ImageReader::new(reader).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let preview = if reader.format() == Some(ImageFormat::Gif) {
        gif_thumbnail(reader.into_inner(), limits, width, height)?
    } else {
        still_thumbnail(reader, limits, width, height)?
    };
    Some(Arc::new(preview))
}

fn still_thumbnail(
    mut reader: ImageReader<impl BufRead + Seek>,
    limits: Limits,
    width: u32,
    height: u32,
) -> Option<ChatImage> {
    reader.limits(limits);
    let preview = reader.decode().ok()?.thumbnail(width, height).into_rgba8();
    Some(ChatImage {
        pixels: Arc::new(RenderImage::new(vec![preview_frame(preview)])),
        frame_ends: Vec::new(),
        repeat: Repeat::Finite(0),
    })
}

struct GifMetadata {
    dimensions: (u32, u32),
    frame_ends: Vec<Duration>,
    repeat: Repeat,
}

fn gif_metadata(reader: &mut impl BufRead, limits: &Limits) -> Option<GifMetadata> {
    enum ApplicationBlock {
        Header,
        LoopCount,
        Skip,
    }

    let mut decoder = StreamingDecoder::new();
    let mut frame_ends = Vec::new();
    let mut end = Duration::ZERO;
    let mut repeat = Repeat::Finite(0);
    let mut application_block = ApplicationBlock::Header;
    loop {
        let input = reader.fill_buf().ok()?;
        if input.is_empty() {
            return None;
        }
        let (consumed, event) = decoder.update(input, &mut OutputBuffer::None).ok()?;
        reader.consume(consumed);
        match event {
            Decoded::BlockStart(Block::Trailer) => break,
            Decoded::SubBlock { ext, is_last }
                if ext.into_known() == Some(Extension::Application) =>
            {
                let data = decoder.last_ext_sub_block();
                application_block = match application_block {
                    ApplicationBlock::Header if matches!(data, b"NETSCAPE2.0" | b"ANIMEXTS1.0") => {
                        ApplicationBlock::LoopCount
                    }
                    ApplicationBlock::LoopCount => {
                        if let [1, low, high] = *data {
                            repeat = match u16::from_le_bytes([low, high]) {
                                0 => Repeat::Infinite,
                                count => Repeat::Finite(count),
                            };
                        }
                        ApplicationBlock::Skip
                    }
                    _ => ApplicationBlock::Skip,
                };
                if is_last {
                    application_block = ApplicationBlock::Header;
                }
            }
            Decoded::FrameMetadata(_) => {
                let width = decoder.width();
                let height = decoder.height();
                limits
                    .check_dimensions(u32::from(width), u32::from(height))
                    .ok()?;
                let frame = decoder.current_frame();
                let count = frame_ends.len() + 1;
                let canvas_pixels = u64::from(width) * u64::from(height);
                if frame.width == 0
                    || frame.height == 0
                    || width.checked_sub(frame.width) < Some(frame.left)
                    || height.checked_sub(frame.height) < Some(frame.top)
                    || count > MAX_GIF_FRAMES
                    || canvas_pixels * count as u64 > MAX_GIF_SOURCE_PIXELS
                {
                    return None;
                }
                // Give frames without an explicit delay a visible duration.
                let delay_ms = if frame.delay == 0 {
                    100
                } else {
                    u64::from(frame.delay) * 10
                };
                end += Duration::from_millis(delay_ms);
                frame_ends.push(end);
            }
            _ => {}
        }
    }
    (!frame_ends.is_empty()).then_some(GifMetadata {
        dimensions: (u32::from(decoder.width()), u32::from(decoder.height())),
        frame_ends,
        repeat,
    })
}

fn gif_thumbnail(
    mut reader: impl BufRead + Seek,
    limits: Limits,
    width: u32,
    height: u32,
) -> Option<ChatImage> {
    // Read the frame budget and loop extension before allocating composited frames.
    let GifMetadata {
        dimensions,
        frame_ends,
        repeat,
    } = gif_metadata(&mut reader, &limits)?;
    reader.rewind().ok()?;
    if frame_ends.len() == 1 {
        return still_thumbnail(
            ImageReader::with_format(reader, ImageFormat::Gif),
            limits,
            width,
            height,
        );
    }

    let mut decoder = image::codecs::gif::GifDecoder::new(reader).ok()?;
    decoder.set_limits(limits).ok()?;
    if decoder.dimensions() != dimensions {
        return None;
    }
    let mut frames = Vec::new();
    for frame in decoder.into_frames() {
        if frames.len() == frame_ends.len() {
            return None;
        }
        let source = frame.ok()?.into_buffer();
        let preview = if dimensions.0 > width || dimensions.1 > height {
            image::DynamicImage::ImageRgba8(source)
                .thumbnail(width, height)
                .into_rgba8()
        } else {
            source
        };
        if preview.as_raw().len() * frame_ends.len() > MAX_GIF_PREVIEW_BYTES {
            return None;
        }
        frames.push(preview_frame(preview));
    }
    if frames.len() != frame_ends.len() {
        return None;
    }
    Some(ChatImage {
        pixels: Arc::new(RenderImage::new(frames)),
        frame_ends,
        repeat,
    })
}

fn preview_frame(mut preview: RgbaImage) -> Frame {
    for pixel in preview.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Frame::new(preview)
}
