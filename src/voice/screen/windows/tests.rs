use super::*;

#[test]
#[ignore = "requires an interactive Windows display"]
fn screen_sources_remain_available_after_device_discovery() {
    tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::task::spawn_blocking(crate::voice::video::camera_devices)
                .await
                .unwrap();
            tokio::task::spawn_blocking(|| {
                use rodio::cpal::{self, traits::HostTrait};
                let _ = cpal::default_host().output_devices();
            })
            .await
            .unwrap();
            let choices = choices().await.expect("Screen Share source picker opens");
            assert!(
                choices
                    .iter()
                    .any(|choice| choice.kind == SourceKind::Display)
            );
        });
}

pub(in super::super) async fn fixture_selection(
    title: &str,
    display: bool,
) -> (Selection, Option<(u32, u32)>) {
    let window = Window::from_name(title).expect("synthetic Windows fixture");
    let (choice, playback, size) = if display {
        let monitor = window.monitor().expect("display showing synthetic window");
        let choice = sources()
            .into_iter()
            .find(|(_, target)| {
                matches!(target, Source::Monitor(candidate)
                    if candidate.as_raw_hmonitor() == monitor.as_raw_hmonitor())
            })
            .expect("fixture display in source list")
            .0;
        let _apartment = Apartment::new().unwrap();
        // SoundPlayer renders through Windows' default waveform output device.
        let endpoint = DeviceEnumerator::new()
            .unwrap()
            .get_default_device_for_role(Direction::Render, Role::Console)
            .expect("fixture needs a render endpoint");
        let playback = Device {
            id: endpoint.get_id().unwrap(),
            name: "Fixture playback endpoint".into(),
        };
        let size = (monitor.width().unwrap(), monitor.height().unwrap());
        (choice, Some(playback), Some(size))
    } else {
        let choice = choices()
            .await
            .unwrap()
            .into_iter()
            .find(|choice| Some(&choice.id) == window_id(&window).as_ref())
            .expect("fixture window in source list");
        (choice, None, None)
    };
    (select(choice, playback).await.unwrap(), size)
}

#[test]
fn windows_capture_distinguishes_denied_permission_from_picker_cancellation() {
    use ::windows::{Win32::Foundation::E_FAIL, core::Error};

    for code in [E_ACCESSDENIED, ERROR_ACCESS_DISABLED_BY_POLICY.to_hresult()] {
        let denied = Error::from_hresult(code);
        assert_eq!(graphics_error(denied.clone()), CaptureError::Permission);
        assert_eq!(picker_error(denied), CaptureError::Permission);
    }
    for code in [E_POINTER, E_ABORT, ERROR_CANCELLED.to_hresult()] {
        assert_eq!(
            picker_error(Error::from_hresult(code)),
            CaptureError::Cancelled
        );
    }
    assert_eq!(
        picker_error(Error::from_hresult(E_FAIL)),
        CaptureError::Unavailable
    );
}

#[test]
fn selected_window_audio_never_becomes_an_endpoint_mix() {
    let endpoint = Device {
        id: "endpoint".into(),
        name: "Speakers".into(),
    };
    let window = Source::Window(Window::from_raw_hwnd(std::ptr::null_mut()));
    assert!(AudioTarget::for_item(&window, Some(endpoint.clone())).is_none());
    let display = Source::Monitor(Monitor::from_raw_hmonitor(std::ptr::null_mut()));
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

#[test]
#[ignore = "requires the synthetic SPK Windows capture fixture"]
fn controlled_window_video_audio_and_source_loss() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .on_thread_start(|| wasapi::initialize_sta().ok().expect("STA capture worker"))
        .on_thread_stop(wasapi::deinitialize)
        .build()
        .unwrap()
        .block_on(check_controlled_window_capture());
}

async fn check_controlled_window_capture() {
    use ::windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{IsWindow, PostMessageW, WM_CLOSE},
    };

    let title = std::env::var("SPK_TEST_SCREEN_WINDOW").unwrap();
    let window = Window::from_name(&title).unwrap();
    let choices = choices().await.unwrap();
    let display = choices
        .iter()
        .find(|choice| choice.kind == SourceKind::Display)
        .expect("display in source list");
    let playback_mix = select(display.clone(), None).await.unwrap();
    assert_eq!(playback_mix.audio, Some(AudioScope::PlaybackMix));
    drop(playback_mix);
    let video_only = select(
        display.clone(),
        Some(Device {
            id: "missing-fixture-playback-endpoint".into(),
            name: "Unavailable speakers".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(video_only.audio, None);
    let (video, mut frames) = mpsc::channel(1);
    let (audio_unavailable, _) = watch::channel(false);
    let (_quality, target) = watch::channel(Quality::default());
    let capture = tokio::spawn(video_only.capture.start(
        false,
        CaptureSink {
            video,
            quality: target,
            audio: None,
            audio_unavailable,
        },
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(8), frames.recv())
            .await
            .expect("display video starts with no playback endpoint")
            .is_some()
    );
    capture.abort();
    assert!(capture.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while frames.recv().await.is_some() {}
    })
    .await
    .expect("display capture releases its outputs after cancellation");
    let choice = choices
        .into_iter()
        .find(|choice| Some(&choice.id) == window_id(&window).as_ref())
        .expect("fixture in window list");
    let mut listed_choice = choice.clone();
    listed_choice.label = "Window · Title before selection".into();
    let selection = select(listed_choice, None).await.unwrap();
    assert_eq!(selection.label, choice.label);
    let pending = select(choice, None).await.unwrap();
    assert_eq!(selection.audio, Some(AudioScope::Application));
    let (video, mut frames) = mpsc::channel(1);
    let (audio, mut samples) = mpsc::channel(4);
    let (audio_unavailable, status) = watch::channel(false);
    let quality = Quality {
        height: 1440,
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
        assert!((400..460).contains(&frame.buffer.height()));
        assert!((600..640).contains(&frame.buffer.width()));
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
    tokio::time::timeout(Duration::from_secs(5), async {
        while frames.recv().await.is_some() {}
        while samples.recv().await.is_some() {}
    })
    .await
    .expect("both capture outputs close after source loss");

    let (sink, _) = CaptureSink::test_pair(false);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), pending.capture.start(false, sink))
            .await
            .expect("a closed source is rejected at start"),
        Err(CaptureError::SourceLost),
    );
}
