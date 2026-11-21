use std::slice;

use ::windows::{
    Win32::{
        Foundation::E_FAIL,
        Media::MediaFoundation::{
            IMFMediaType, IMFSourceReader, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_E_NO_MORE_TYPES,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_RATE_RANGE_MAX,
            MF_MT_FRAME_RATE_RANGE_MIN, MF_MT_FRAME_SIZE, MF_MT_SUBTYPE,
            MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
            MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MF_VERSION, MFCreateAttributes,
            MFCreateDeviceSource, MFCreateSourceReaderFromMediaSource,
            MFGetStrideForBitmapInfoHeader, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup,
            MFVideoFormat_MJPG as MJPG, MFVideoFormat_NV12 as NV12, MFVideoFormat_RGB24 as RGB24,
            MFVideoFormat_YUY2 as YUY2,
        },
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
    },
    core::{GUID, HSTRING, Result},
};
use nokhwa::{
    Buffer,
    utils::{CameraInfo, FrameFormat, Resolution},
};

use super::{format_preference, rgb24::packed_rows};

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const Y800: GUID = GUID::from_u128(0x30303859_0000_0010_8000_00aa00389b71);

pub(super) struct Camera {
    reader: IMFSourceReader,
    _media_foundation: MediaFoundation,
}

impl Camera {
    pub(super) fn open(device: &CameraInfo, ceiling: (u32, u32)) -> Result<Self> {
        let media_foundation = MediaFoundation::start()?;
        // Use the source reader directly because Nokhwa's Buffer discards the signed stride.
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 3)?;
            let attributes = attributes.ok_or(E_FAIL)?;
            attributes.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )?;
            attributes.SetString(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                &HSTRING::from(device.misc()),
            )?;
            let source = MFCreateDeviceSource(&attributes)?;
            attributes.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, &attributes)?;
            let mut best = None;
            for index in 0.. {
                let media_type = match reader.GetNativeMediaType(VIDEO, index) {
                    Ok(media_type) => media_type,
                    Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                    Err(error) => return Err(error),
                };
                if frame_format(&media_type)?.is_none() {
                    continue;
                }
                let size = resolution(&media_type)?;
                let rank = [
                    MF_MT_FRAME_RATE,
                    MF_MT_FRAME_RATE_RANGE_MIN,
                    MF_MT_FRAME_RATE_RANGE_MAX,
                ]
                .into_iter()
                .filter_map(|key| media_type.GetUINT64(&key).ok())
                .filter_map(|rate| ((rate as u32) == 1).then_some((rate >> 32) as u32))
                .map(|fps| format_preference(size.width(), size.height(), fps, ceiling))
                .min();
                if let Some(rank) = rank
                    && best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank)
                {
                    best = Some((rank, media_type));
                }
            }
            if let Some((_, media_type)) = best {
                reader.SetCurrentMediaType(VIDEO, None, &media_type)?;
            }
            reader.SetStreamSelection(VIDEO, true)?;
            Ok(Self {
                reader,
                _media_foundation: media_foundation,
            })
        }
    }

    pub(super) fn frame(&mut self) -> Result<Buffer> {
        unsafe {
            let sample = loop {
                let mut sample = None;
                let mut flags = 0;
                self.reader.ReadSample(
                    VIDEO,
                    0,
                    None,
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                )?;
                if flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32 != 0
                {
                    return Err(E_FAIL.into());
                }
                if let Some(sample) = sample {
                    break sample;
                }
            };
            let media_type = self.reader.GetCurrentMediaType(VIDEO)?;
            let size = resolution(&media_type)?;
            let format = frame_format(&media_type)?.ok_or(E_FAIL)?;
            let stride = if format == FrameFormat::RAWBGR {
                media_type
                    .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                    .map(|stride| stride as i32)
                    .or_else(|_| MFGetStrideForBitmapInfoHeader(RGB24.data1, size.width()))?
            } else {
                0
            };

            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let frame = if data.is_null() || len == 0 || len as usize > isize::MAX as usize {
                None
            } else {
                // Lock keeps exactly these initialized bytes readable until Unlock.
                let bytes = slice::from_raw_parts(data, len as usize);
                if format == FrameFormat::RAWBGR {
                    packed_rows(bytes, size.width(), size.height(), stride)
                        .map(|bytes| Buffer::new(size, &bytes, format))
                } else {
                    Some(Buffer::new(size, bytes, format))
                }
            };
            buffer.Unlock()?;
            frame.ok_or_else(|| E_FAIL.into())
        }
    }
}

fn resolution(media_type: &IMFMediaType) -> Result<Resolution> {
    let size = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE)? };
    Ok(Resolution::new((size >> 32) as u32, size as u32))
}

fn frame_format(media_type: &IMFMediaType) -> Result<Option<FrameFormat>> {
    let subtype = unsafe { media_type.GetGUID(&MF_MT_SUBTYPE)? };
    Ok(match subtype {
        RGB24 => Some(FrameFormat::RAWBGR),
        YUY2 => Some(FrameFormat::YUYV),
        NV12 => Some(FrameFormat::NV12),
        MJPG => Some(FrameFormat::MJPEG),
        Y800 => Some(FrameFormat::GRAY),
        _ => None,
    })
}

struct MediaFoundation;

impl MediaFoundation {
    fn start() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
                CoUninitialize();
                return Err(error);
            }
        }
        Ok(Self)
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
