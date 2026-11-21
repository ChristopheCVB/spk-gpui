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

#[tokio::test]
#[ignore = "requires the synthetic SPK Windows capture fixture"]
async fn controlled_window_video_audio_and_source_loss() {
    use ::windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{IsWindow, PostMessageW, WM_CLOSE},
    };

    let title = std::env::var("SPK_TEST_SCREEN_WINDOW").unwrap();
    let window = Window::from_name(&title).unwrap();
    let choices = choices().await.unwrap();
    assert!(
        choices
            .iter()
            .any(|choice| choice.id.starts_with("display:"))
    );
    let choice = choices
        .into_iter()
        .find(|choice| Some(&choice.id) == window_id(&window).as_ref())
        .expect("fixture in window list");
    let selection = select(choice, None).await.unwrap();
    assert_eq!(selection.audio, Some(AudioScope::Application));
    let (video, mut frames) = mpsc::channel(1);
    let (audio, mut samples) = mpsc::channel(4);
    let (audio_unavailable, status) = watch::channel(false);
    let quality = Quality {
        height: 720,
        fps: 5,
    };
    let (_quality, target) = watch::channel(quality);
    let capture = tokio::spawn(selection.capture.start(
        true,
        CaptureSink {
            video,
            quality: target,
            audio: Some(audio),
            audio_unavailable,
        },
    ));
    tokio::time::timeout(Duration::from_secs(15), async {
        let (frame, _, fps) = frames.recv().await.unwrap();
        assert_eq!(fps, quality.fps);
        assert!(frame.buffer.height() <= quality.height);
        assert!(frame.buffer.width() > 0);
        while !samples
            .recv()
            .await
            .unwrap()
            .iter()
            .any(|sample| sample.unsigned_abs() > 100)
        {}
    })
    .await
    .expect("fixture produced captured frames and audio");
    assert!(!*status.borrow());
    unsafe {
        PostMessageW(
            Some(HWND(window.as_raw_hwnd())),
            WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        )
    }
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while unsafe { IsWindow(Some(HWND(window.as_raw_hwnd()))).as_bool() } {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("fixture closed its selected window");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), capture)
            .await
            .unwrap()
            .unwrap(),
        Err(CaptureError::SourceLost),
    );
}
