use super::*;

#[test]
fn participant_playback_matches_watch_and_local_volume_settings() {
    let mut intent = Intent::default();
    let mic = TrackSource::Microphone;
    let screen = TrackSource::ScreenshareAudio;
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Voice(1.0)));
    assert_eq!(route(&intent, "alice", screen, false), None);
    intent.watching.insert("alice".into());
    assert_eq!(
        route(&intent, "alice", screen, false),
        Some(Route::Media(1.0))
    );
    let volume = intent
        .preferred
        .participant_volumes
        .entry("alice".into())
        .or_default();
    volume.mic = 200;
    volume.screen = 25;
    intent.preferred.output_volume = 150;
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Voice(3.0)));
    assert_eq!(
        route(&intent, "alice", screen, false),
        Some(Route::Media(0.375))
    );
    assert_eq!(route(&intent, "bob", mic, false), Some(Route::Voice(1.5)));
    assert_eq!(route(&intent, "bob", screen, false), None);
    intent
        .preferred
        .participant_volumes
        .get_mut("alice")
        .unwrap()
        .muted = true;
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Silent));
    assert_eq!(route(&intent, "alice", screen, false), Some(Route::Silent));
    intent.mute.deafened = true;
    assert_eq!(route(&intent, "bob", mic, false), Some(Route::Silent));
    assert_eq!(route(&intent, "bob", screen, false), None);
    intent.mute.deafened = false;
    assert_eq!(route(&intent, "bob", mic, true), Some(Route::Silent));
    assert_eq!(route(&intent, "bob", mic, false), Some(Route::Voice(1.5)));
    intent.watching.clear();
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Silent));
    assert_eq!(route(&intent, "alice", screen, false), None);
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires the isolated stereo PulseAudio output from tests/fixtures/linux-screen-share.sh"]
async fn linux_voice_output_keeps_the_mixer_when_the_render_reference_is_unavailable() {
    let device = cpal_device(DeviceKind::Speakers, None).unwrap();
    let failed = Arc::new(AtomicBool::new(false));
    let output = VoiceSink::open(device, None, failed.clone()).unwrap();
    assert!(matches!(output, VoiceSink::Playback(_)));
    let player = Player::connect_new(output.mixer());
    player.set_volume(0.5);
    player.append(SamplesBuffer::new(
        NonZero::new(2).unwrap(),
        NonZero::new(48_000).unwrap(),
        vec![0.25; 4_800],
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !player.empty() {
            assert!(!failed.load(Relaxed));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the fallback device should play mixer samples");
    assert_eq!(player.volume(), 0.5);
}

#[tokio::test]
#[ignore = "requires a loopback LiveKit server and SPK_TEST_PUBLISHER/SPK_TEST_RECEIVER tokens"]
async fn livekit_watch_audio_keeps_microphone_and_recovers_after_full_reconnect() {
    use livekit::rtc_engine::SimulateScenario;

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
    let identity = publisher.local_participant().identity().0;
    let (mic, mic_source) = publish_audio(&publisher, TrackSource::Microphone, 1).await;
    let (share, share_source) = publish_audio(&publisher, TrackSource::ScreenshareAudio, 2).await;
    let mut jobs = tokio::task::JoinSet::new();
    jobs.spawn(tone(mic_source, 1, 0.03));
    jobs.spawn(tone(share_source, 2, 0.09));
    let mic_sid = mic.sid();
    let share_sid = share.sid();
    let mut intent = Intent::default();
    let mut remote = RemoteAudio::default();
    let (voice, mut voice_pcm) =
        rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());
    let (media, mut media_pcm) =
        rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48_000).unwrap());

    let routes = await_routes(&receiver, &intent, |routes| routes.contains_key(&mic_sid)).await;
    assert_eq!(routes[&mic_sid].1, Route::Voice(1.0));
    assert!(
        publication(&receiver, &identity, &share_sid)
            .is_none_or(|share| !share.is_desired() && !share.is_subscribed())
    );
    assert!(remote.tracks.is_empty());
    assert!(media_pcm.next().is_none());
    assert!(voice_pcm.next().is_none());

    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    let voice_player = remote.tracks[&mic_sid].player.clone();
    await_pcm(&mut voice_pcm).await;

    intent.watching.insert(identity.clone());
    intent
        .preferred
        .participant_volumes
        .entry(identity.clone())
        .or_default()
        .mic = 50;
    let routes = await_routes(&receiver, &intent, |routes| routes.contains_key(&share_sid)).await;
    assert_eq!(routes[&mic_sid].1, Route::Voice(0.5));
    assert_eq!(routes[&share_sid].1, Route::Media(1.0));
    let mic_track = routes[&mic_sid].0.clone();
    let share_track = routes[&share_sid].0.clone();
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    assert!(!mic_track.is_enabled());
    assert!(!share_track.is_enabled());
    assert!(Arc::ptr_eq(&remote.tracks[&mic_sid].player, &voice_player));
    assert_eq!(remote.tracks[&mic_sid].player.volume(), 0.5);
    assert_eq!(remote.tracks[&share_sid].player.volume(), 1.0);
    await_pcm(&mut voice_pcm).await;
    await_pcm(&mut media_pcm).await;

    intent.mute.deafened = true;
    let routes = sync_subscriptions(&receiver, &intent);
    assert_eq!(routes[&mic_sid].1, Route::Silent);
    assert_eq!(routes[&share_sid].1, Route::Silent);
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    assert!(remote.tracks.is_empty());
    assert!(
        publication(&receiver, &identity, &share_sid)
            .unwrap()
            .is_subscribed()
    );
    assert!(
        publication(&receiver, &identity, &mic_sid)
            .unwrap()
            .is_subscribed()
    );

    intent.mute.deafened = false;
    intent
        .preferred
        .participant_volumes
        .get_mut(&identity)
        .unwrap()
        .screen = 75;
    let routes = sync_subscriptions(&receiver, &intent);
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    assert_eq!(remote.tracks[&share_sid].player.volume(), 0.75);
    receiver
        .simulate_scenario(SimulateScenario::FullReconnect)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(12), async {
        let mut suspended = false;
        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::Reconnecting => {
                    remote.suspend();
                    assert!(remote.tracks.is_empty());
                    suspended = true;
                }
                RoomEvent::Reconnected => {
                    assert!(suspended);
                    return;
                }
                _ => {}
            }
        }
        panic!("the receiver disconnected instead of rejoining");
    })
    .await
    .unwrap();
    let routes = await_routes(&receiver, &intent, |routes| {
        routes.contains_key(&mic_sid) && routes.contains_key(&share_sid)
    })
    .await;
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    assert_eq!(remote.tracks[&share_sid].player.volume(), 0.75);
    await_pcm(&mut media_pcm).await;

    publisher
        .local_participant()
        .unpublish_track(&share_sid)
        .await
        .unwrap();
    let routes = await_routes(&receiver, &intent, |_| {
        publication(&receiver, &identity, &share_sid).is_none()
    })
    .await;
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    assert!(!remote.tracks.contains_key(&share_sid));
    let (replacement, source) = publish_audio(&publisher, TrackSource::ScreenshareAudio, 2).await;
    jobs.spawn(tone(source, 2, 0.09));
    let replacement = replacement.sid();
    let routes = await_routes(&receiver, &intent, |routes| {
        routes.contains_key(&replacement)
    })
    .await;
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    await_pcm(&mut media_pcm).await;

    intent.watching.clear();
    intent.preferred.participant_volumes.clear();
    let routes = sync_subscriptions(&receiver, &intent);
    assert_eq!(routes[&mic_sid].1, Route::Voice(1.0));
    let mic_track = routes[&mic_sid].0.clone();
    sync_playback(&mut remote.tracks, routes, Some(&voice), Some(&media));
    assert_eq!(remote.tracks.len(), 1);
    assert_eq!(remote.tracks[&mic_sid].player.volume(), 1.0);
    assert!(!mic_track.is_enabled());
    let routes = sync_subscriptions(&receiver, &intent);
    assert!(sync_playback(
        &mut remote.tracks,
        routes,
        None,
        Some(&media)
    ));
    assert!(remote.tracks.is_empty());
    assert!(mic_track.is_enabled());
    let screen = publication(&receiver, &identity, &replacement).unwrap();
    assert!(!screen.is_desired());
    assert!(!screen.is_subscribed());
    assert!(
        publication(&receiver, &identity, &mic_sid)
            .unwrap()
            .is_subscribed()
    );
    receiver.close().await.unwrap();
    jobs.abort_all();
    publisher.close().await.unwrap();
}

async fn await_routes(
    room: &Room,
    intent: &Intent,
    ready: impl Fn(&HashMap<TrackSid, (RemoteAudioTrack, Route)>) -> bool,
) -> HashMap<TrackSid, (RemoteAudioTrack, Route)> {
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let routes = sync_subscriptions(room, intent);
            if ready(&routes) {
                return routes;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

fn publication(room: &Room, id: &str, sid: &TrackSid) -> Option<RemoteTrackPublication> {
    room.remote_participants()
        .get(&ParticipantIdentity(id.to_owned()))?
        .track_publications()
        .remove(sid)
}

async fn publish_audio(
    room: &Room,
    source: TrackSource,
    channels: u32,
) -> (
    LocalTrackPublication,
    livekit::webrtc::audio_source::native::NativeAudioSource,
) {
    use livekit::{
        options::{AudioEncoding, TrackPublishOptions},
        webrtc::audio_source::{RtcAudioSource, native::NativeAudioSource},
    };
    let capture = NativeAudioSource::new(Default::default(), 48_000, channels, 100);
    let track =
        LocalAudioTrack::create_audio_track("fixture", RtcAudioSource::Native(capture.clone()));
    let publication = room
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source,
                dtx: false,
                audio_encoding: Some(AudioEncoding {
                    max_bitrate: 256_000,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(publication.source(), source);
    (publication, capture)
}

async fn tone(
    capture: livekit::webrtc::audio_source::native::NativeAudioSource,
    channels: u32,
    pitch: f32,
) {
    let mut samples = livekit::webrtc::audio_frame::AudioFrame::new(48_000, channels, 480);
    for (index, frame) in samples
        .data
        .to_mut()
        .chunks_exact_mut(channels as usize)
        .enumerate()
    {
        for (channel, sample) in frame.iter_mut().enumerate() {
            *sample = ((index as f32 * pitch * (1 + channel) as f32).sin() * 9_000.0) as i16;
        }
    }
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    loop {
        interval.tick().await;
        capture.capture_frame(&samples).await.unwrap();
    }
}

async fn await_pcm(output: &mut impl Iterator<Item = f32>) {
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if output.take(960).any(|sample| sample.abs() > 0.015) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("expected decoded audio in the output mixer");
}
