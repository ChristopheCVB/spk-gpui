use super::*;

#[test]
fn presets_bound_capture_without_upscaling_or_distorting() {
    let quality = Quality::default();
    assert_eq!(quality.dimensions(3840, 2160), Some((1920, 1080)));
    assert_eq!(quality.dimensions(600, 1200), Some((540, 1080)));
    assert_eq!(quality.dimensions(801, 601), Some((800, 600)));
    assert_eq!(quality.dimensions(1, 480), None);
    assert_eq!(
        Quality::publication_name(1920, 1080, 30),
        "spk:screen:1080:30"
    );
    assert_eq!(Quality::publication_name(1280, 720, 5), "spk:screen:720:5");
    assert_eq!(Quality::publication_name(320, 180, 25), "screen");
    assert_eq!(Quality::publication_name(1920, 1080, 25), "screen");
    assert_eq!(Quality::publication_name(1680, 1080, 30), "screen");
}

#[test]
fn screen_audio_preferences_survive_reload_and_default_to_web_behavior() {
    let previous: super::super::DevicePreferences = serde_json::from_str("{}").unwrap();
    assert!(previous.screen_audio);
    let changed = super::super::DevicePreferences {
        screen_audio: false,
        ..previous
    };
    let round_trip = serde_json::to_string(&changed).unwrap();
    let restored: super::super::DevicePreferences = serde_json::from_str(&round_trip).unwrap();
    assert!(!restored.screen_audio);
}

#[tokio::test]
async fn cancellation_ignores_late_or_unconfirmed_capture() {
    let (sender, mut requests) = watch::channel(Intent::default());
    sender.send_modify(|intent| intent.screen.open());
    let attempt = requests.borrow().screen.attempt;
    assert!(
        !sender.send_if_modified(|intent| intent.screen.advance(attempt - 1, Action::Start(true)))
    );
    let result = select_until_cancelled(&mut requests, attempt, async {
        sender.send_modify(|intent| intent.screen.open());
        Ok(())
    })
    .await;
    assert_eq!(result, Err(CaptureError::Cancelled));
}

#[test]
fn capture_sinks_reject_unconfirmed_audio() {
    let (sink, mut samples) = CaptureSink::test_pair(false);
    sink.audio(vec![1, 2]);
    assert!(samples.try_recv().is_err());
    let (sink, mut samples) = CaptureSink::test_pair(true);
    sink.audio(vec![1, 2, 3]);
    assert!(samples.try_recv().is_err());
    sink.audio(vec![1, 2]);
    assert_eq!(samples.try_recv().unwrap(), [1, 2]);
}

#[tokio::test]
#[ignore = "requires loopback LiveKit and SPK_TEST_PUBLISHER/SPK_TEST_RECEIVER tokens"]
async fn livekit_screen_share_publishes_video_and_separate_audio() {
    use futures::StreamExt;
    use livekit::webrtc::{
        audio_stream::native::NativeAudioStream, video_frame::VideoRotation,
        video_stream::native::NativeVideoStream,
    };
    use tokio::time::{interval, timeout};

    struct Fixture(oneshot::Receiver<()>, Option<Box<dyn Capture>>);

    impl Capture for Fixture {
        fn start(
            self: Box<Self>,
            audio: bool,
            sink: CaptureSink,
        ) -> BoxFuture<'static, Result<(), CaptureError>> {
            Box::pin(async move {
                let mut stop = self.0;
                if let Some(native) = self.1 {
                    return tokio::select! {
                        result = native.start(audio, sink) => result,
                        _ = &mut stop => Err(CaptureError::SourceLost),
                    };
                }
                let mut ticks = interval(Duration::from_millis(10));
                let mut elapsed = 0;
                let mut quality = *sink.quality.borrow();
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stop => return Err(CaptureError::SourceLost),
                        _ = ticks.tick() => {
                            let requested = *sink.quality.borrow();
                            if requested.fps != 60 {
                                quality = requested;
                            }
                            if elapsed % (if quality.fps == 5 { 200 } else { 40 }) == 0 {
                                let (width, height) = quality.dimensions(1920, 1080).unwrap();
                                let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, I420Buffer::new_black(width, height));
                                frame.timestamp_us = elapsed * 1000;
                                sink.video(frame, quality, quality.fps);
                            }
                            if audio {
                                let samples = (0..480)
                                    .flat_map(|index| {
                                        let phase = (index as f32 + elapsed as f32 * 48.) * 0.057595866;
                                        [(phase.sin() * 7000.) as i16, (phase.cos() * 4000.) as i16]
                                    })
                                    .collect();
                                sink.audio(samples);
                            }
                            elapsed += 10;
                        }
                    }
                }
            })
        }
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let endpoint =
        std::env::var("SPK_TEST_LIVEKIT").unwrap_or_else(|_| "ws://127.0.0.1:17880".into());
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    let (publisher, _) = Room::connect(
        &endpoint,
        &std::env::var("SPK_TEST_PUBLISHER").unwrap(),
        options.clone(),
    )
    .await
    .unwrap();
    let (receiver, mut events) = Room::connect(
        &endpoint,
        &std::env::var("SPK_TEST_RECEIVER").unwrap(),
        options,
    )
    .await
    .unwrap();
    let (native, scope) = (None, AudioScope::Application);
    #[cfg(target_os = "linux")]
    let (native, scope) = if let Ok(output) = std::env::var("SPK_TEST_PULSE_OUTPUT") {
        let choice = linux::choices()
            .await
            .unwrap()
            .into_iter()
            .find(|choice| choice.id == "display" || choice.id.starts_with("display:"))
            .expect("fixture Linux display");
        let selected = linux::select(
            choice,
            Some(Device {
                id: output.clone(),
                name: output,
            }),
        )
        .await
        .unwrap();
        assert_eq!(selected.audio, Some(AudioScope::PlaybackMix));
        (Some(selected.capture), AudioScope::PlaybackMix)
    } else {
        (native, scope)
    };
    #[cfg(windows)]
    let (native, scope) = if let Ok(title) = std::env::var("SPK_TEST_SCREEN_WINDOW") {
        let choice = windows::choices()
            .await
            .unwrap()
            .into_iter()
            .find(|choice| choice.label == format!("Window · {title}"))
            .expect("synthetic Windows fixture");
        let selected = windows::select(choice, None).await.unwrap();
        assert_eq!(selected.audio, Some(AudioScope::Application));
        (Some(selected.capture), AudioScope::Application)
    } else {
        (native, scope)
    };
    let synthetic = native.is_none();
    let audio_threshold = if !synthetic && scope == AudioScope::Application {
        100
    } else {
        500
    };
    let check_peer_dimensions = |actual: (u32, u32), expected: (u32, u32)| {
        if !synthetic && scope == AudioScope::Application {
            // WebRTC aligns native capture dimensions for the encoder.
            assert!(actual.0.abs_diff(expected.0) <= 4 && actual.1.abs_diff(expected.1) <= 4);
        } else {
            assert_eq!(actual, expected);
        }
    };
    let (stop, stopped) = oneshot::channel();
    let (report_tx, mut report_rx) = tokio::sync::watch::channel(State::Starting);
    let (requests, mut requested) = watch::channel(Intent::default());

    let share = tokio::spawn(async move {
        let result = capture_and_publish(
            &publisher,
            Quality::default(),
            &mut requested,
            Some(scope),
            Box::new(Fixture(stopped, native)),
            &|state| {
                report_tx.send_replace(state);
            },
        )
        .await;
        (publisher, result)
    });

    let initial_size = timeout(Duration::from_secs(8), async {
        let state = report_rx.wait_for(State::active).await.unwrap();
        let State::Live {
            width,
            height,
            fps,
            audio,
            ..
        } = *state
        else {
            unreachable!();
        };
        assert_eq!((fps, audio), (30, Some(scope)));
        if synthetic {
            assert_eq!((width, height), (1920, 1080));
        } else if scope == AudioScope::PlaybackMix {
            assert_eq!((width, height), (640, 480));
        } else {
            assert!((600..640).contains(&width) && (400..460).contains(&height));
        }
        (width, height)
    })
    .await
    .unwrap();

    let (video, audio) = timeout(Duration::from_secs(8), async {
        let mut video = None;
        let mut audio = None;
        while video.is_none() || audio.is_none() {
            if let RoomEvent::TrackPublished { publication, .. } =
                events.recv().await.expect("peer disconnected")
            {
                match publication.source() {
                    TrackSource::Screenshare => {
                        assert_eq!(
                            publication.name(),
                            if synthetic {
                                "spk:screen:1080:30"
                            } else {
                                "screen"
                            }
                        );
                        video = Some(publication);
                    }
                    TrackSource::ScreenshareAudio => audio = Some(publication),
                    _ => unreachable!("share may only publish Screen Share tracks"),
                }
            }
        }
        (video.unwrap(), audio.unwrap())
    })
    .await
    .unwrap();
    assert!(!video.is_desired() && !audio.is_desired());
    video.set_subscribed(true);
    audio.set_subscribed(true);
    let (mut frames, mut samples) = timeout(Duration::from_secs(8), async {
        let mut frames = None;
        let mut samples = None;
        while frames.is_none() || samples.is_none() {
            if let RoomEvent::TrackSubscribed { track, .. } =
                events.recv().await.expect("peer disconnected")
            {
                match track {
                    RemoteTrack::Video(track) => {
                        frames = Some(NativeVideoStream::new(track.rtc_track()))
                    }
                    RemoteTrack::Audio(track) => {
                        track.disable();
                        samples = Some(NativeAudioStream::new(track.rtc_track(), 48_000, 2));
                    }
                }
            }
        }
        (frames.unwrap(), samples.unwrap())
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(8), async {
        let frame = frames.next().await.unwrap();
        check_peer_dimensions((frame.buffer.width(), frame.buffer.height()), initial_size);
        loop {
            let sample = samples.next().await.unwrap();
            assert_eq!(sample.sample_rate, 48_000);
            if sample
                .data
                .iter()
                .any(|value| value.unsigned_abs() > audio_threshold)
            {
                break;
            }
        }
    })
    .await
    .unwrap();

    let preset = Quality {
        height: 720,
        fps: 5,
    };
    let replacement_size = if synthetic { (1280, 720) } else { initial_size };
    requests.send_modify(|intent| intent.screen.quality = preset);
    timeout(Duration::from_secs(8), async {
        let state = report_rx
            .wait_for(|state| matches!(state, State::Live { fps: 5, .. }))
            .await
            .unwrap();
        assert!(
            matches!(*state, State::Live { width, height, quality, audio: Some(audio), .. }
            if quality == preset && (width, height) == replacement_size && audio == scope)
        );
    })
    .await
    .expect("active Screen Share follows the new preset");

    let replacement = timeout(Duration::from_secs(8), async {
        loop {
            match events.recv().await.expect("peer disconnected") {
                RoomEvent::TrackPublished { publication, .. }
                    if publication.source() == TrackSource::Screenshare =>
                {
                    break publication;
                }
                RoomEvent::TrackUnpublished { publication, .. }
                    if publication.source() == TrackSource::ScreenshareAudio =>
                {
                    panic!("quality change interrupted Screen Share audio");
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_ne!(replacement.sid(), video.sid());
    assert_eq!(
        replacement.name(),
        if synthetic {
            "spk:screen:720:5"
        } else {
            "screen"
        }
    );
    assert!(audio.is_desired());

    timeout(Duration::from_secs(8), async {
        loop {
            if let Some(RoomEvent::TrackUnpublished { publication, .. }) = events.recv().await
                && publication.sid() == video.sid()
            {
                break;
            }
        }
    })
    .await
    .expect("the old video publication is released");

    replacement.set_subscribed(true);
    let mut replacement_frames = timeout(Duration::from_secs(8), async {
        loop {
            if let Some(RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(track),
                publication,
                ..
            }) = events.recv().await
                && publication.sid() == replacement.sid()
            {
                break NativeVideoStream::new(track.rtc_track());
            }
        }
    })
    .await
    .unwrap();

    if synthetic {
        let rejected = Quality {
            height: 1440,
            fps: 60,
        };
        requests.send_modify(|intent| intent.screen.quality = rejected);
        timeout(Duration::from_secs(8), async {
            report_rx
                .wait_for(|state| {
                    matches!(state, State::Live {
                quality, quality_failed: Some(failed), audio: Some(AudioScope::Application), ..
            } if *quality == preset && *failed == rejected)
                })
                .await
                .unwrap();
        })
        .await
        .expect("an unsupported live change keeps the previous preset and audio");
    }

    timeout(Duration::from_secs(8), async {
        let frame = replacement_frames.next().await.unwrap();
        check_peer_dimensions(
            (frame.buffer.width(), frame.buffer.height()),
            replacement_size,
        );
        loop {
            let sample = samples.next().await.unwrap();
            if sample
                .data
                .iter()
                .any(|value| value.unsigned_abs() > audio_threshold)
            {
                break;
            }
        }
    })
    .await
    .unwrap();

    stop.send(()).unwrap();
    let (publisher, result) = share.await.unwrap();
    assert_eq!(result, Err(CaptureError::SourceLost));
    timeout(Duration::from_secs(8), async {
        let mut unpublished = 0;
        while unpublished < 2 {
            if let Some(RoomEvent::TrackUnpublished { publication, .. }) = events.recv().await
                && matches!(
                    publication.source(),
                    TrackSource::Screenshare | TrackSource::ScreenshareAudio
                )
            {
                unpublished += 1;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        publisher
            .local_participant()
            .track_publications()
            .is_empty()
    );
    assert_eq!(publisher.connection_state(), ConnectionState::Connected);
    receiver.close().await.unwrap();
    publisher.close().await.unwrap();
}
