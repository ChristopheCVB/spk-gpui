use super::{pulse::matching_sink, *};

#[test]
fn portal_choices_only_list_the_advertised_source_types() {
    assert!(portal_choices(false, false).is_empty());
    assert_eq!(portal_choices(true, false)[0].id, "display");
    assert_eq!(portal_choices(false, true)[0].id, "window");
    assert_eq!(portal_choices(true, true).len(), 2);
}

#[test]
fn padded_bgra_frames_convert_and_obey_the_preset_without_upscaling() {
    let width = 1920;
    let height = 1080;
    let stride = 1930 * 4;
    let mut bytes = vec![0; (stride * height) as usize];
    for pixel in bytes.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[0, 0, 255, 255]);
    }
    let frame = video_frame(
        &bytes,
        width,
        height,
        stride,
        Quality {
            height: 720,
            fps: 5,
        },
    )
    .unwrap();
    assert_eq!((frame.buffer.width(), frame.buffer.height()), (1280, 720));
    let (y, u, v) = frame.buffer.data();
    assert!(
        y[0] > 60 && u[0] < 120 && v[0] > 200,
        "Expected a red pixel"
    );
    assert!(video_frame(&bytes[..12], 4, 2, 16, Quality::default()).is_none());
    let small = video_frame(&bytes[..32], 4, 2, 16, Quality::default()).unwrap();
    assert_eq!((small.buffer.width(), small.buffer.height()), (4, 2));
}

#[test]
fn pulse_monitor_only_matches_the_selected_sink() {
    assert_eq!(
        matching_sink(
            Some(&Device {
                id: "index:2".into(),
                name: "USB audio".into()
            }),
            [("usb", "USB audio"), ("built-in", "Built-in audio")]
        ),
        Some("usb")
    );
    assert_eq!(
        matching_sink(
            Some(&Device {
                id: "index:2".into(),
                name: "USB audio".into()
            }),
            [("usb", "USB audio"), ("second-usb", "USB audio")]
        ),
        None
    );
    assert_eq!(
        matching_sink(
            Some(&Device {
                id: "usb".into(),
                name: "USB audio".into()
            }),
            [("usb", "USB audio"), ("second-usb", "USB audio")]
        ),
        Some("usb")
    );
    assert_eq!(
        matching_sink(
            Some(&Device {
                id: "unplugged".into(),
                name: "USB audio".into()
            }),
            [("usb", "USB audio")]
        ),
        None
    );
}

#[test]
fn preselection_error_distinguishes_x11_source_loss_from_portal_failure() {
    assert_eq!(selection_error(false), CaptureError::SourceLost);
    assert_eq!(selection_error(true), CaptureError::PortalSelection);
}

#[tokio::test]
#[ignore = "requires an isolated Xvfb display"]
async fn x11_sources_capture_and_end_when_the_window_disappears() {
    use x11rb::{
        connection::Connection,
        protocol::xproto::{AtomEnum, ConnectionExt, CreateWindowAux, WindowClass},
        wrapper::ConnectionExt as _,
    };

    let (connection, screen) = x11rb::connect(None).unwrap();
    let root = &connection.setup().roots[screen];
    let window = connection.generate_id().unwrap();
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root.root,
            0,
            0,
            320,
            240,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::default().background_pixel(0xff0000),
        )
        .unwrap()
        .check()
        .unwrap();
    let wm_state = connection
        .intern_atom(false, b"WM_STATE")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    connection
        .change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            wm_state,
            wm_state,
            &[1, 0],
        )
        .unwrap()
        .check()
        .unwrap();
    connection
        .change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"SPK fixture",
        )
        .unwrap()
        .check()
        .unwrap();
    connection.map_window(window).unwrap().check().unwrap();

    let choices = choices().await.unwrap();
    let display = choices
        .iter()
        .find(|choice| choice.id.starts_with("display:"))
        .unwrap();
    let missing = Device {
        id: "spk-fixture-missing".into(),
        name: "Missing fixture output".into(),
    };
    let display = select(display.clone(), Some(missing)).await.unwrap();
    assert_eq!(display.audio, None);

    let choice = choices
        .into_iter()
        .find(|choice| choice.id == format!("window:{window}"))
        .expect("fixture window must be available independently of the display");
    let selected = select(choice.clone(), None).await.unwrap();
    assert_eq!(selected.audio, None);
    let (video, mut frames) = mpsc::channel(1);
    let (audio, mut samples) = mpsc::channel(1);
    let (audio_unavailable, _) = watch::channel(false);
    let capture = tokio::spawn(selected.capture.start(
        Quality {
            height: 720,
            fps: 5,
        },
        false,
        CaptureSink {
            video,
            audio: Some(audio),
            audio_unavailable,
        },
    ));
    let (frame, fps) = timeout(Duration::from_secs(2), frames.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (frame.buffer.width(), frame.buffer.height(), fps),
        (320, 240, 5)
    );
    assert!(samples.try_recv().is_err());
    connection.destroy_window(window).unwrap().check().unwrap();
    assert_eq!(
        timeout(Duration::from_secs(6), capture)
            .await
            .unwrap()
            .unwrap(),
        Err(CaptureError::SourceLost)
    );
    assert!(matches!(
        select(choice, None).await,
        Err(CaptureError::SourceLost)
    ));
}

#[tokio::test]
#[ignore = "requires an isolated Wayland portal fixture advertising only window capture"]
async fn wayland_portal_cancellation_ends_source_selection() {
    let mut available = choices().await.unwrap();
    assert_eq!(available.len(), 1);
    let choice = available.pop().unwrap();
    assert_eq!(choice.id, "window");
    assert!(matches!(
        timeout(Duration::from_secs(3), select(choice, None))
            .await
            .unwrap(),
        Err(CaptureError::PortalSelection)
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
}
