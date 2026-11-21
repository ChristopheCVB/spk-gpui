use std::{
    mem::MaybeUninit,
    ptr::{NonNull, null},
};

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertyScope, AudioObjectPropertySelector, kAudioDevicePropertyStreamFormat,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_audio_types::AudioStreamBasicDescription;

use super::Device;

pub(super) type PlayoutFormat = (AudioObjectID, AudioStreamBasicDescription);

pub(super) fn playout_format(device: &Device) -> Option<PlayoutFormat> {
    // SAFETY: CoreAudio returns an AudioObjectID for the default device and an
    // AudioStreamBasicDescription for its output stream format.
    unsafe {
        let id = if device.id == "default" {
            property(
                kAudioObjectSystemObject as AudioObjectID,
                kAudioHardwarePropertyDefaultOutputDevice,
                kAudioObjectPropertyScopeGlobal,
            )?
        } else {
            device.id.parse().ok()?
        };
        let format = property(
            id,
            kAudioDevicePropertyStreamFormat,
            kAudioObjectPropertyScopeOutput,
        )?;
        Some((id, format))
    }
}

// The caller must use the type CoreAudio returns for this property.
unsafe fn property<T>(
    id: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Option<T> {
    let mut address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let data_size = size_of::<T>() as u32;
    let mut size = data_size;
    let mut value = MaybeUninit::<T>::uninit();
    // SAFETY: All pointers are valid for the call. The output buffer has the
    // caller-specified property's size and is read only after a complete write.
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    if status == 0 && size == data_size {
        // SAFETY: A successful read of the expected size has initialized the value.
        Some(unsafe { value.assume_init() })
    } else {
        None
    }
}
