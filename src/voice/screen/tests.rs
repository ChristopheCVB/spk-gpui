use super::*;

#[test]
fn presets_bound_capture_without_upscaling_or_distorting() {
    let quality = Quality::default();
    assert_eq!(quality.dimensions(3840, 2160), Some((1920, 1080)));
    assert_eq!(quality.dimensions(600, 1200), Some((540, 1080)));
    assert_eq!(quality.dimensions(801, 601), Some((800, 600)));
    assert_eq!(quality.dimensions(1, 480), None);
    assert_eq!(quality.name(), "spk:screen:1080:30");
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

    struct Fixture(oneshot::Receiver<()>);

    impl Capture for Fixture {
        fn start(
            self: Box<Self>,
            _: Quality,
            audio: bool,
            sink: CaptureSink,
        ) -> BoxFuture<'static, Result<(), CaptureError>> {
            Box::pin(async move {
                let mut stop = self.0;
                let mut ticks = interval(Duration::from_millis(10));
                let mut elapsed = 0;
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stop => return Err(CaptureError::SourceLost),
                        _ = ticks.tick() => {
                            if elapsed % 40 == 0 {
                                let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, I420Buffer::new_black(320, 180));
                                frame.timestamp_us = elapsed * 1000;
                                sink.video(frame, 25);
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
    let (stop, stopped) = oneshot::channel();
    let (report_tx, mut report_rx) = tokio::sync::watch::channel(State::Starting);

    let share = tokio::spawn(async move {
        let result = capture_and_publish(
            &publisher,
            Quality::default(),
            Some(AudioScope::Application),
            Box::new(Fixture(stopped)),
            &|state| {
                report_tx.send_replace(state);
            },
        )
        .await;
        (publisher, result)
    });

    timeout(Duration::from_secs(8), async {
        let state = report_rx.wait_for(State::active).await.unwrap();
        assert!(matches!(
            *state,
            State::Live {
                width: 320,
                height: 180,
                fps: 25,
                audio: Some(AudioScope::Application),
                ..
            }
        ));
    })
    .await
    .unwrap();

    let (video, audio) = timeout(Duration::from_secs(8), async {
        let mut video = None;
        let mut audio = None;
        loop {
            if let Some(RoomEvent::TrackPublished { publication, .. }) = events.recv().await {
                match publication.source() {
                    TrackSource::Screenshare => {
                        assert_eq!(publication.name(), "spk:screen:1080:30");
                        video = Some(publication);
                    }
                    TrackSource::ScreenshareAudio => audio = Some(publication),
                    _ => unreachable!("share may only publish Screen Share tracks"),
                }
            }
            if video.is_some() && audio.is_some() {
                return (video.unwrap(), audio.unwrap());
            }
        }
    })
    .await
    .unwrap();
    assert!(!video.is_desired() && !audio.is_desired());
    video.set_subscribed(true);
    audio.set_subscribed(true);
    let (mut frames, mut samples) = timeout(Duration::from_secs(8), async {
        let mut frames = None;
        let mut samples = None;
        loop {
            if let Some(RoomEvent::TrackSubscribed { track, .. }) = events.recv().await {
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
            if frames.is_some() && samples.is_some() {
                return (frames.unwrap(), samples.unwrap());
            }
        }
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(8), async {
        let frame = frames.next().await.unwrap();
        assert_eq!((frame.buffer.width(), frame.buffer.height()), (320, 180));
        loop {
            let sample = samples.next().await.unwrap();
            assert_eq!(sample.sample_rate, 48_000);
            if sample.data.iter().any(|value| value.unsigned_abs() > 500) {
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
    receiver.close().await.unwrap();
    publisher.close().await.unwrap();
}
