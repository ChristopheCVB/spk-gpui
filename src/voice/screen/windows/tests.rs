use super::*;

#[test]
fn selected_window_audio_never_becomes_an_endpoint_mix() {
    let endpoint = Device {
        id: "endpoint".into(),
        name: "Speakers".into(),
    };
    let window = CaptureItemTypes::Window(Window::from_raw_hwnd(std::ptr::null_mut()));
    assert!(AudioTarget::for_item(&window, Some(endpoint.clone())).is_none());
    let display = CaptureItemTypes::Monitor(Monitor::from_raw_hmonitor(std::ptr::null_mut()));
    assert!(matches!(
        AudioTarget::for_item(&display, Some(endpoint)),
        Some(AudioTarget::Endpoint(Some(id))) if id == "endpoint"
    ));
}

#[test]
fn shared_audio_converts_stereo_float_pcm_to_signed_samples() {
    let bytes: Vec<_> = [0.5_f32, -0.25, 1.0, -1.0, f32::NAN, f32::INFINITY]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    assert_eq!(stereo_pcm(&bytes), [16_384, -8_192, 32_767, -32_768, 0, 0]);
}

#[test]
fn padded_bgra_capture_is_scaled_inside_the_requested_preset() {
    let original = [0_u8, 0, 255, 255].repeat(24);
    let scaled = screen_frame(&original, 4, 6, 16, (2, 4)).unwrap();
    assert_eq!((scaled.width(), scaled.height()), (2, 4));
    assert!(screen_frame(&original[..92], 4, 6, 16, (2, 4)).is_none());
}
