use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use gpui_kit::RenderImage;
use image::RgbaImage;
use livekit::webrtc::native::yuv_helper::i420_to_abgr;
use rusty_h264_common::nal::{emulation_unprevent, split_annex_b};
use rusty_h264_decoder::{Decoder, Pps, Sps};
use zvidlib_core::Limits;

use super::{BAD_VIDEO, preview_image};

const START: &[u8] = &[0, 0, 0, 1];

pub(super) struct H264Preview {
    decoder: Box<Decoder>,
    length_bytes: usize,
    limits: Limits,
}

impl H264Preview {
    pub(super) fn new(config: &[u8], limits: Limits) -> Result<Self, &'static str> {
        let record = match config {
            [
                0,
                0,
                0,
                1,
                b'a',
                b'v',
                b'c',
                b'C',
                _,
                _,
                _,
                _,
                _,
                _,
                _,
                _,
                record @ ..,
            ] => record,
            [_, _, _, _, b'a', b'v', b'c', b'C', record @ ..] => record,
            _ => return Err(BAD_VIDEO),
        };
        let [1, _, _, _, length, sps_count, sets @ ..] = record else {
            return Err(BAD_VIDEO);
        };
        let mut sets = sets;
        let length_bytes = usize::from(length & 3) + 1;
        let mut headers = Vec::new();
        for _ in 0..(sps_count & 31) {
            append_nal(&mut sets, 2, &mut headers)?;
        }
        let (&pps_count, mut sets) = sets.split_first().ok_or(BAD_VIDEO)?;
        for _ in 0..pps_count {
            append_nal(&mut sets, 2, &mut headers)?;
        }
        let mut preview = Self {
            decoder: Box::new(Decoder::new()),
            length_bytes,
            limits,
        };
        preview.decode(&headers, false)?;
        Ok(preview)
    }

    pub(super) fn submit(
        &mut self,
        mut packet: &[u8],
        show: bool,
    ) -> Result<Option<Arc<RenderImage>>, &'static str> {
        let mut annex_b = Vec::with_capacity(packet.len());
        while !packet.is_empty() {
            append_nal(&mut packet, self.length_bytes, &mut annex_b)?;
        }
        self.decode(&annex_b, show)
    }

    fn decode(
        &mut self,
        annex_b: &[u8],
        show: bool,
    ) -> Result<Option<Arc<RenderImage>>, &'static str> {
        // Check coded dimensions and parameter set limits before the decoder allocates.
        let frame = catch_unwind(AssertUnwindSafe(|| {
            for nal in split_annex_b(annex_b) {
                let (&header, payload) = nal.split_first().ok_or(BAD_VIDEO)?;
                match header & 31 {
                    7 => {
                        let sps =
                            Sps::parse(&emulation_unprevent(payload)).map_err(|_| BAD_VIDEO)?;
                        let (width, height) = (sps.coded_width(), sps.coded_height());
                        let picture_bytes = width as u64 * height as u64 * 3 / 2;
                        if width > self.limits.max_width as usize
                            || height > self.limits.max_height as usize
                            || sps.seq_parameter_set_id >= 32
                            || sps.max_num_ref_frames > 16
                            || picture_bytes * (u64::from(sps.max_num_ref_frames) + 2)
                                > self.limits.max_allocation_bytes
                            || sps.frame_crop_left.saturating_add(sps.frame_crop_right) as usize
                                >= sps.coded_width() / 2
                            || sps.frame_crop_top.saturating_add(sps.frame_crop_bottom) as usize
                                >= sps.coded_height() / sps.crop_unit_y()
                        {
                            return Err(BAD_VIDEO);
                        }
                    }
                    8 => {
                        let pps =
                            Pps::parse(&emulation_unprevent(payload)).map_err(|_| BAD_VIDEO)?;
                        if pps.pic_parameter_set_id >= 256 || pps.seq_parameter_set_id >= 32 {
                            return Err(BAD_VIDEO);
                        }
                    }
                    _ => {}
                }
            }
            self.decoder.decode(annex_b).map_err(|_| BAD_VIDEO)
        }))
        .map_err(|_| BAD_VIDEO)??;
        let Some(frame) = frame else {
            return Ok(None);
        };
        let image = if show {
            if !(2..=self.limits.max_width as usize).contains(&frame.width)
                || !(2..=self.limits.max_height as usize).contains(&frame.height)
                || !frame.width.is_multiple_of(2)
                || !frame.height.is_multiple_of(2)
                || !frame.is_valid()
            {
                return Err(BAD_VIDEO);
            }
            let (width, height) = (frame.width as u32, frame.height as u32);
            let mut image = RgbaImage::new(width, height);
            i420_to_abgr(
                &frame.y,
                width,
                &frame.u,
                width / 2,
                &frame.v,
                width / 2,
                &mut image,
                width * 4,
                width as i32,
                height as i32,
            );
            Some(preview_image(image))
        } else {
            None
        };
        self.decoder.recycle_frame(frame);
        Ok(image)
    }
}

fn append_nal(
    input: &mut &[u8],
    length_bytes: usize,
    output: &mut Vec<u8>,
) -> Result<(), &'static str> {
    let (length, rest) = input.split_at_checked(length_bytes).ok_or(BAD_VIDEO)?;
    let len = length
        .iter()
        .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte));
    let (nal, rest) = rest.split_at_checked(len as usize).ok_or(BAD_VIDEO)?;
    if nal.is_empty() {
        return Err(BAD_VIDEO);
    }
    output.extend_from_slice(START);
    output.extend_from_slice(nal);
    *input = rest;
    Ok(())
}
