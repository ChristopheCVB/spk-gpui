use super::*;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const CLIP: &str = "23456789-1111-4ccc-8aaa-123456789abc";

#[tokio::test]
async fn opening_soundboard_clears_the_new_clip_dot_and_refreshes_without_an_event() {
    const NEW_CLIP: &str = "3456789a-1111-4ccc-8aaa-123456789abc";
    let (opened, wait_for_open) = async_channel::bounded(1);
    let end = voice_session(
        Call::start_for_test,
        async |sockets, requests, _| {
            let mut desktop = accept_async(sockets.accept().await.unwrap().0)
                .await
                .unwrap();
            desktop
                .send(Message::text(
                    value!({"type": "sound:new", "id": CLIP}).to_string(),
                ))
                .await
                .unwrap();
            let join = establish_voice(&requests, &mut desktop).await;

            for (id, name) in [(CLIP, "Chime"), (NEW_CLIP, "New chime")] {
                let request = requests.recv().await.unwrap();
                assert_eq!(request.line, "GET /api/sounds HTTP/1.1");
                request
                    .respond(
                        &[],
                        &value!([{"id": id, "name": name, "icon": "bell", "durationMs": 1000}])
                            .to_string(),
                    )
                    .await;
            }
            wait_for_open.recv().await.unwrap();

            desktop
                .send(Message::text(
                    value!({"type": "sound:new", "id": CLIP}).to_string(),
                ))
                .await
                .unwrap();
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                value!({"type":"voice:leave","sid":join["sid"]})
            );
        },
        async |commands, views| {
            let waiting = wait_for_sidebar(&views, |s| {
                s.soundboard.has_new_clip
                    && s.channels.is_some()
                    && matches!(s.connection, Connection::Online)
            })
            .await;
            assert!(waiting.voice.is_none());
            assert!(waiting.soundboard.clips.is_none());
            commands
                .send(Command::JoinVoice("first".into()))
                .await
                .unwrap();
            let joined = wait_for_sidebar(&views, shows_alice_speaking).await;
            assert!(joined.soundboard.has_new_clip);

            for id in [CLIP, NEW_CLIP] {
                commands.send(Command::OpenSoundboard).await.unwrap();
                let opened = wait_for_sidebar(&views, |s| {
                    s.soundboard
                        .clips
                        .as_ref()
                        .is_some_and(|clips| clips.iter().any(|clip| clip.id == id))
                })
                .await;
                assert!(!opened.soundboard.has_new_clip);
            }
            opened.send(()).await.unwrap();

            wait_for_sidebar(&views, |s| s.soundboard.has_new_clip).await;
            commands.send(Command::LeaveVoice).await.unwrap();
            let left = wait_for_sidebar(&views, |s| s.voice.is_none()).await;
            assert!(left.soundboard.has_new_clip);
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn soundboard_previews_privately_and_plays_only_the_channels_relay() {
    let (advance, step) = async_channel::unbounded();
    let (checked, wait_for_check) = async_channel::bounded(1);
    let end = voice_session(
        Call::start_for_test,
        async |sockets, requests, _| {
            let mut desktop = accept_async(sockets.accept().await.unwrap().0)
                .await
                .unwrap();
            let join = establish_voice(&requests, &mut desktop).await;
            let sid = &join["sid"];

            requests.recv().await.unwrap().respond(&[], "[]").await;
            requests
                .recv()
                .await
                .unwrap()
                .respond_raw("503 Service Unavailable", &[], b"")
                .await;
            requests
                .recv()
                .await
                .unwrap()
                .respond(
                    &[],
                    &value!([
                        {"id": CLIP, "name": "Chime", "icon": "bell", "durationMs": 1000}
                    ])
                    .to_string(),
                )
                .await;

            let mut request = format!("ws://{}/_ws", sockets.local_addr().unwrap())
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("cookie", "session=peer".parse().unwrap());
            let (peer, accepted) = tokio::join!(tokio_tungstenite::connect_async(request), async {
                accept_hdr_async(
                    sockets.accept().await.unwrap().0,
                    |request: &server::Request, response: server::Response| {
                        assert_eq!(request.headers()["cookie"], "session=peer");
                        Ok(response)
                    },
                )
                .await
                .unwrap()
            });
            let (mut peer, _) = peer.unwrap();
            let mut peer_server = accepted;
            let peer_join = value!({"type": "voice:join", "sid": "peer-sid", "channelId": "first"});
            peer.send(Message::text(peer_join.to_string()))
                .await
                .unwrap();
            assert_eq!(receive_voice_frame(&mut peer_server).await, peer_join);
            desktop
                .send(Message::text(
                    value!({
                        "type": "voice:presence", "userId": "bob", "channelId": "first"
                    })
                    .to_string(),
                ))
                .await
                .unwrap();

            // A private preview fetches a clip without sending a voice command.
            let storage = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let file =
                value!({"url": format!("http://{}/chime.wav", storage.local_addr().unwrap())})
                    .to_string();
            requests.recv().await.unwrap().respond(&[], &file).await;
            let preview = Request::read(&storage, "GET /chime.wav HTTP/1.1").await;
            assert!(preview.header("cookie").is_none());
            preview.respond_raw("404 Not Found", &[], b"").await;
            assert!(
                timeout(
                    Duration::from_millis(75),
                    receive_non_presence_message(&mut desktop)
                )
                .await
                .is_err()
            );

            advance.send(()).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                value!({"type": "voice:sound", "sid": sid, "soundId": CLIP})
            );
            assert!(
                timeout(Duration::from_millis(75), requests.recv())
                    .await
                    .is_err()
            );
            wait_for_check.recv().await.unwrap();
            let relay = value!({"type": "voice:sound", "userId": "alice", "soundId": CLIP});
            let frame = Message::text(relay.to_string());
            desktop.send(frame.clone()).await.unwrap();
            peer_server.send(frame.clone()).await.unwrap();
            assert_eq!(peer.next().await.unwrap().unwrap(), frame);
            requests.recv().await.unwrap().respond(&[], &file).await;
            let mut pending_play = Request::read(&storage, "GET /chime.wav HTTP/1.1").await;
            assert!(pending_play.header("cookie").is_none());

            let peer_press = value!({"type": "voice:sound", "sid": "peer-sid", "soundId": CLIP});
            let peer_relay = value!({"type": "voice:sound", "userId": "bob", "soundId": CLIP});
            peer.send(Message::text(peer_press.to_string()))
                .await
                .unwrap();
            assert_eq!(receive_voice_frame(&mut peer_server).await, peer_press);
            desktop
                .send(Message::text(peer_relay.to_string()))
                .await
                .unwrap();
            requests.recv().await.unwrap().respond(&[], &file).await;
            let mut pending_peer_clip = Request::read(&storage, "GET /chime.wav HTTP/1.1").await;

            advance.send(()).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                voice_state(sid, true, true)
            );
            assert_eq!(pending_play.stream.read(&mut [0_u8]).await.unwrap(), 0);
            assert_eq!(pending_peer_clip.stream.read(&mut [0_u8]).await.unwrap(), 0);
            desktop
                .send(Message::text(peer_relay.to_string()))
                .await
                .unwrap();
            assert!(
                timeout(Duration::from_millis(75), requests.recv())
                    .await
                    .is_err()
            );

            advance.send(()).await.unwrap();
            requests.recv().await.unwrap().respond(&[], &file).await;
            let mut pending_preview = Request::read(&storage, "GET /chime.wav HTTP/1.1").await;
            advance.send(()).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                value!({"type":"voice:leave","sid": sid})
            );
            assert_eq!(pending_preview.stream.read(&mut [0_u8]).await.unwrap(), 0);
        },
        async |commands, views| {
            join_test_voice(&commands, &views).await;
            commands.send(Command::LoadSoundboard).await.unwrap();
            wait_for_sidebar(&views, |s| {
                s.soundboard
                    .clips
                    .as_ref()
                    .is_some_and(|clips| clips.is_empty())
            })
            .await;
            commands.send(Command::LoadSoundboard).await.unwrap();
            wait_for_sidebar(&views, |s| s.soundboard.directory_error.is_some()).await;
            commands.send(Command::LoadSoundboard).await.unwrap();
            wait_for_sidebar(&views, |s| {
                s.soundboard
                    .clips
                    .as_ref()
                    .is_some_and(|clips| clips.len() == 1)
            })
            .await;

            commands
                .send(Command::SetSoundboardVolume(35))
                .await
                .unwrap();
            commands
                .send(Command::PreviewSound(CLIP.into()))
                .await
                .unwrap();
            let unavailable =
                wait_for_sidebar(&views, |s| s.soundboard.playback_error.is_some()).await;
            assert_eq!(unavailable.voice_intent.preferred.soundboard_volume, 35);
            step.recv().await.unwrap();
            commands
                .send(Command::PlaySound(CLIP.into()))
                .await
                .unwrap();
            let awaiting_relay =
                wait_for_sidebar(&views, |s| s.soundboard.playback_error.is_none()).await;
            assert!(!awaiting_relay.soundboard.cooling);
            checked.send(()).await.unwrap();
            let playing = wait_for_sidebar(&views, |s| s.soundboard.cooling).await;
            assert_eq!(
                playing.soundboard.active_clips["alice"]
                    .as_ref()
                    .unwrap()
                    .name,
                "Chime"
            );
            step.recv().await.unwrap();
            let overlapping =
                wait_for_sidebar(&views, |s| s.soundboard.active_clips.len() == 2).await;
            assert!(overlapping.soundboard.active_clips.contains_key("bob"));
            commands.send(Command::ToggleDeafen).await.unwrap();
            let deafened = wait_for_sidebar(&views, |s| s.voice_intent.mute.deafened).await;
            assert!(deafened.soundboard.active_clips.contains_key("bob"));
            step.recv().await.unwrap();
            commands
                .send(Command::PreviewSound(CLIP.into()))
                .await
                .unwrap();
            step.recv().await.unwrap();
            commands.send(Command::LeaveVoice).await.unwrap();
            let left = wait_for_sidebar(&views, |s| s.voice.is_none()).await;
            assert!(left.soundboard.clips.is_none());
            assert!(left.soundboard.playback_error.is_none());
            assert!(left.soundboard.active_clips.is_empty());
            assert!(!left.soundboard.cooling);
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn soundboard_turn_ends_after_the_clip_and_releases_everyone_but_its_player() {
    const NEW_CLIP: &str = "3456789a-1111-4ccc-8aaa-123456789abc";
    let (advance, step) = async_channel::unbounded();
    let (checked, wait_for_check) = async_channel::unbounded();
    let end = voice_session(
        Call::start_for_test,
        async |sockets, requests, _| {
            let mut desktop = accept_async(sockets.accept().await.unwrap().0)
                .await
                .unwrap();
            let join = establish_voice(&requests, &mut desktop).await;
            let sid = &join["sid"];

            advance.send(()).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                voice_state(sid, true, true)
            );
            requests
                .recv()
                .await
                .unwrap()
                .respond(
                    &[],
                    &value!([{ "id": CLIP, "name": "Chime", "icon": "bell", "durationMs": 600 }])
                        .to_string(),
                )
                .await;
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                value!({"type": "voice:sound", "sid": sid, "soundId": CLIP})
            );
            desktop
                .send(Message::text(
                    value!({"type": "voice:sound", "userId": "alice", "soundId": CLIP}).to_string(),
                ))
                .await
                .unwrap();
            wait_for_check.recv().await.unwrap();
            assert!(
                timeout(
                    Duration::from_millis(100),
                    receive_non_presence_message(&mut desktop)
                )
                .await
                .is_err()
            );
            desktop
                .send(Message::text(
                    value!({"type": "voice:sound", "userId": "bob", "soundId": NEW_CLIP}).to_string(),
                ))
                .await
                .unwrap();
            let lookup = requests.recv().await.unwrap();
            wait_for_check.recv().await.unwrap();
            lookup
                .respond(
                    &[],
                    &value!([{ "id": NEW_CLIP, "name": "New chime", "icon": "bell", "durationMs": 400 }])
                        .to_string(),
                )
                .await;
            assert_eq!(
                receive_voice_frame(&mut desktop).await,
                value!({"type": "voice:sound", "sid": sid, "soundId": NEW_CLIP})
            );
            advance.send(()).await.unwrap();
        },
        async |commands, views| {
            join_test_voice(&commands, &views).await;
            step.recv().await.unwrap();
            commands.send(Command::ToggleDeafen).await.unwrap();
            wait_for_sidebar(&views, |s| s.voice_intent.mute.deafened).await;
            commands.send(Command::LoadSoundboard).await.unwrap();
            wait_for_sidebar(&views, |s| s.soundboard.clips.is_some()).await;
            commands.send(Command::PlaySound(CLIP.into())).await.unwrap();

            wait_for_sidebar(&views, |s| s.soundboard.active_clips.contains_key("alice")).await;
            wait_for_sidebar(&views, |s| s.soundboard.cooling && s.soundboard.active_clips.is_empty())
                .await;
            commands.send(Command::PlaySound(CLIP.into())).await.unwrap();
            checked.send(()).await.unwrap();

            let loading =
                wait_for_sidebar(&views, |s| s.soundboard.active_clips.contains_key("bob")).await;
            assert!(loading.soundboard.active_clips["bob"].is_none());
            checked.send(()).await.unwrap();
            wait_for_sidebar(&views, |s| {
                s.soundboard
                    .active_clips
                    .get("bob")
                    .and_then(Option::as_ref)
                    .is_some_and(|clip| clip.name == "New chime")
            })
            .await;
            wait_for_sidebar(&views, |s| !s.soundboard.cooling).await;
            commands.send(Command::PlaySound(NEW_CLIP.into())).await.unwrap();
            step.recv().await.unwrap();
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}
