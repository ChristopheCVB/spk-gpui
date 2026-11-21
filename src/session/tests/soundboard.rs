use super::*;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const CLIP: &str = "23456789-1111-4ccc-8aaa-123456789abc";

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn soundboard_previews_privately_and_plays_only_the_channels_relay() {
    let (advance, step) = async_channel::unbounded();
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
                timeout(Duration::from_millis(75), desktop.next())
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
            step.recv().await.unwrap();
            commands.send(Command::ToggleDeafen).await.unwrap();
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
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}
