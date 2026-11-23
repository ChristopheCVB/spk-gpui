use super::{pulse::matching_sink, *};
use x11rb::rust_connection::RustConnection;

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

fn fixture_window() -> (RustConnection, u32) {
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
    (connection, window)
}

#[tokio::test]
#[ignore = "requires isolated Xvfb, PulseAudio and LiveKit fixtures"]
async fn x11_screen_share_publishes_video_only_and_preserves_voice() {
    assert!(!use_system_picker().await.unwrap());
    let (publisher, receiver, mut events) = fixture_rooms().await;
    let (connection, window) = fixture_window();
    let choices = choices().await.unwrap();
    let display = choices
        .iter()
        .find(|choice| choice.id.starts_with("display:"))
        .unwrap();
    let missing = Device {
        id: "spk-fixture-missing".into(),
        name: "Missing fixture output".into(),
    };
    let window_choice = choices
        .iter()
        .find(|choice| choice.id == format!("window:{window}"))
        .expect("fixture window must be available independently of the display");
    let available = std::env::var("SPK_TEST_PULSE_OUTPUT").unwrap();
    let available = Device {
        name: available.clone(),
        id: available,
    };
    let lose_window = || connection.destroy_window(window).unwrap().check().unwrap();
    for (choice, output, size, audio_scope, wants_audio) in [
        (display, missing, (640, 480), None, true),
        (
            display,
            available.clone(),
            (640, 480),
            Some(AudioScope::PlaybackMix),
            false,
        ),
        (window_choice, available, (320, 240), None, true),
    ] {
        let lose_source = (choice.id == window_choice.id).then_some(&lose_window as &dyn Fn());
        check_video_only_share(
            &publisher,
            &mut events,
            (
                select(choice.clone(), Some(output)),
                size,
                audio_scope,
                wants_audio,
            ),
            lose_source,
        )
        .await;
    }
    assert!(matches!(
        select(window_choice.clone(), None).await,
        Err(CaptureError::SourceLost)
    ));
    receiver.close().await.unwrap();
    publisher.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires the Wayland window portal, PulseAudio and LiveKit fixtures"]
async fn wayland_window_publishes_video_only_and_preserves_voice() {
    assert!(use_system_picker().await.unwrap());
    let (publisher, receiver, mut events) = fixture_rooms().await;
    let output = std::env::var("SPK_TEST_PULSE_OUTPUT").unwrap();
    let output = Device {
        name: output.clone(),
        id: output,
    };
    let stop = std::env::var_os("SPK_TEST_WAYLAND_SCREEN_STOP").unwrap();
    let lose_source = || std::fs::write(&stop, []).unwrap();
    for end in [None, Some(&lose_source as &dyn Fn())] {
        let selection = async {
            let selection = pick(Some(output.clone()), false).await?;
            assert_eq!(selection.label, "Screen Share · 640 × 480");
            assert!(selection.can_choose_display);
            Ok(selection)
        };
        check_video_only_share(
            &publisher,
            &mut events,
            (selection, (640, 480), None, true),
            end,
        )
        .await;
    }
    receiver.close().await.unwrap();
    publisher.close().await.unwrap();
}

async fn fixture_rooms() -> (Room, Room, tokio::sync::mpsc::UnboundedReceiver<RoomEvent>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let endpoint = std::env::var("SPK_TEST_LIVEKIT").unwrap();
    let (publisher, _) = Room::connect(
        &endpoint,
        &std::env::var("SPK_TEST_PUBLISHER").unwrap(),
        RoomOptions::default(),
    )
    .await
    .unwrap();
    let (receiver, events) = Room::connect(
        &endpoint,
        &std::env::var("SPK_TEST_RECEIVER").unwrap(),
        RoomOptions::default(),
    )
    .await
    .unwrap();

    (publisher, receiver, events)
}

async fn check_video_only_share(
    publisher: &Room,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    (selection, size, audio_scope, wants_audio): (
        impl Future<Output = Result<Selection, CaptureError>>,
        (u32, u32),
        Option<AudioScope>,
        bool,
    ),
    lose_source: Option<&dyn Fn()>,
) {
    use futures::StreamExt as _;
    use livekit::webrtc::video_stream::native::NativeVideoStream;

    let selected = timeout(Duration::from_secs(6), selection)
        .await
        .expect("selected source delivers video")
        .unwrap();
    assert_eq!(selected.audio, audio_scope);
    let quality = Quality {
        height: 720,
        fps: 5,
    };
    let (requests, mut requested) = watch::channel(Intent::default());
    requests.send_modify(|intent| {
        intent.screen.quality = quality;
        intent.screen.open(None);
    });
    let attempt = requested.borrow().screen.attempt;
    let mut cancellation = requested.clone();
    let report = |state| {
        if let State::Live {
            width,
            height,
            fps,
            audio,
            ..
        } = state
        {
            assert_eq!(((width, height), fps, audio), (size, 5, None));
        }
    };
    let share = super::super::select_until_cancelled(
        &mut cancellation,
        attempt,
        super::super::capture_and_publish(
            publisher,
            quality,
            &mut requested,
            selected.audio.filter(|_| wants_audio),
            selected.capture,
            &report,
        ),
    );
    let watcher = async {
        let (video, mut frames) = loop {
            if let RoomEvent::TrackSubscribed {
                track, publication, ..
            } = events.recv().await.unwrap()
            {
                assert_eq!(publication.source(), TrackSource::Screenshare);
                let RemoteTrack::Video(track) = track else {
                    unreachable!()
                };
                break (publication, NativeVideoStream::new(track.rtc_track()));
            }
        };
        for _ in 0..3 {
            let frame = frames.next().await.unwrap();
            assert_eq!((frame.buffer.width(), frame.buffer.height()), size);
        }
        assert_eq!(publisher.local_participant().track_publications().len(), 1);
        if let Some(lose_source) = lose_source {
            lose_source();
        } else {
            requests.send_modify(|intent| {
                assert!(intent.screen.advance(attempt, Action::Stop));
            });
        }
        loop {
            match events.recv().await.unwrap() {
                RoomEvent::TrackPublished { publication, .. } => {
                    assert_eq!(publication.sid(), video.sid(), "unexpected share track");
                }
                RoomEvent::TrackUnpublished { publication, .. }
                    if publication.sid() == video.sid() =>
                {
                    break;
                }
                _ => {}
            }
        }
    };
    let (result, ()) = timeout(Duration::from_secs(15), async {
        tokio::join!(share, watcher)
    })
    .await
    .unwrap();
    assert_eq!(
        result,
        Err(if lose_source.is_some() {
            CaptureError::SourceLost
        } else {
            CaptureError::Cancelled
        })
    );
    assert!(
        publisher
            .local_participant()
            .track_publications()
            .is_empty()
    );
    assert_eq!(publisher.connection_state(), ConnectionState::Connected);
}

#[tokio::test]
#[ignore = "requires Xvfb and a Wayland session without a compositor socket"]
async fn wayland_without_socket_does_not_offer_x11_sources() {
    assert!(matches!(
        use_system_picker().await,
        Err(CaptureError::Unavailable)
    ));
}

#[tokio::test]
#[ignore = "requires an isolated Wayland portal fixture advertising only window capture"]
async fn wayland_portal_rejection_ends_source_selection() {
    assert!(use_system_picker().await.unwrap());
    for error in [
        CaptureError::Cancelled,
        CaptureError::PortalSelection,
        CaptureError::Permission,
    ] {
        assert!(matches!(
            timeout(Duration::from_secs(3), pick(None, false)).await.unwrap(),
            Err(actual) if actual == error
        ));
    }

    // Cancel while either portal method is still waiting for the user.
    for _ in 0..2 {
        assert!(
            timeout(Duration::from_secs(1), pick(None, false))
                .await
                .is_err()
        );
    }
    assert!(matches!(
        timeout(Duration::from_secs(3), pick(None, false))
            .await
            .unwrap(),
        Err(CaptureError::PortalSelection)
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
}
