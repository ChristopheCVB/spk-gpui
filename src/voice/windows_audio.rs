use windows::Win32::{
    Foundation::RPC_E_CHANGED_MODE,
    Media::Audio::{
        EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications, eRender,
    },
    System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize,
    },
};

use super::DeviceKind;

pub(super) fn default_id(kind: DeviceKind) -> Option<String> {
    communications_endpoint(match kind {
        DeviceKind::Microphone => eCapture,
        DeviceKind::Speakers | DeviceKind::MediaOutput => eRender,
    })
}

fn communications_endpoint(flow: EDataFlow) -> Option<String> {
    // Keep the COM interfaces and the endpoint ID on the calling thread.
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
        // CPAL can already have initialized this thread as STA.
        if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
            return None;
        }
        let result = (|| {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER).ok()?;
            let device = enumerator
                .GetDefaultAudioEndpoint(flow, eCommunications)
                .ok()?;
            let id = device.GetId().ok()?;
            let result = id.to_string().ok();
            CoTaskMemFree(Some(id.0.cast()));
            result
        })();
        if initialized.is_ok() {
            CoUninitialize();
        }
        result
    }
}
