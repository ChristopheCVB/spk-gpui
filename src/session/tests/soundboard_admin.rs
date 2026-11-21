use super::{channel_admin::start_peer, *};

const ID: &str = "23456789-1111-4ccc-8aaa-123456789abc";

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn admin_soundboard_uploads_selected_audio_and_peer_refreshes_edits_and_deletion() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let endpoint = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let url = format!("http://{}/sound.wav", storage.local_addr().unwrap());
    let (uploaded, uploads) = async_channel::bounded(1);
    let storage = tokio::spawn(async move {
        Request::read(&storage, "PUT /sound.wav HTTP/1.1")
            .await
            .respond_raw("401 Unauthorized", &[], b"")
            .await;
        let put = Request::read(&storage, "PUT /sound.wav HTTP/1.1").await;
        assert_eq!(put.header("cookie"), None);
        assert_eq!(put.header("origin"), None);
        assert_eq!(put.header("content-type"), Some("audio/wav"));
        assert_eq!(put.header("content-length"), Some("236"));
        assert_eq!(&put.body[..12], b"RIFF\xe4\0\0\0WAVE");
        assert_eq!(&put.body[34..44], b"\x18\0data\xc0\0\0\0");
        assert_eq!(&put.body[44..], [0, 0, 32].repeat(64));
        uploaded.send(()).await.unwrap();
        put.respond(&[], "{}").await;
    });
    let server = tokio::spawn(async move {
        let mut clip = None;
        let mut reject_edit = true;
        loop {
            let (line, request) = Request::receive(&http).await;
            let cookie = request.header("cookie");
            assert!(matches!(cookie, Some("session=alice" | "session=bob")));
            let json = || serde_json::from_slice::<Value>(&request.body).unwrap();
            let response = match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    value!([{"id":"first","name":"Lobby","type":"voice"}])
                }
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "POST /api/voice/token HTTP/1.1" => {
                    value!({"token":"fixture-token","url":env!("LIVEKIT_URL")})
                }
                "GET /api/sounds HTTP/1.1" => value!(clip.iter().collect::<Vec<_>>()),
                _ if cookie == Some("session=bob") => panic!("User attempted {line}"),
                "POST /api/sounds/presign HTTP/1.1" => {
                    assert_eq!(json(), value!({"size":236}));
                    value!({"id":ID,"url":url,"headers":{"content-type":"audio/wav"}})
                }
                "POST /api/sounds HTTP/1.1" => {
                    uploads
                        .try_recv()
                        .expect("SPK must receive the upload before creating a clip");
                    assert_eq!(
                        json(),
                        value!({"id":ID,"name":"Ping","icon":"bell","durationMs":8,"size":236})
                    );
                    let sound = value!({"id":ID,"name":"Ping","icon":"bell","durationMs":8});
                    clip = Some(sound.clone());
                    sound
                }
                _ if line == format!("PATCH /api/sounds/{ID} HTTP/1.1") => {
                    assert_eq!(json(), value!({"name":"Bells","icon":"music"}));
                    if std::mem::take(&mut reject_edit) {
                        request.respond_raw("403 Forbidden", &[], b"").await;
                        continue;
                    }
                    let sound = value!({"id":ID,"name":"Bells","icon":"music","durationMs":8});
                    clip = Some(sound.clone());
                    sound
                }
                _ if line == format!("DELETE /api/sounds/{ID} HTTP/1.1") => {
                    clip = None;
                    value!({"ok":true})
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &response.to_string()).await;
        }
    });
    let (alice, alice_views, alice_session) = start_peer(&base, &endpoint, "alice", "admin");
    let (bob, bob_views, bob_session) = start_peer(&base, &endpoint, "bob", "user");
    let mut config = std::collections::HashMap::new();
    let files = tempfile::tempdir().unwrap();
    let sample = files.path().join("sample.wav");
    let invalid = files.path().join("invalid.wav");
    let mut wav = b"RIFF\x24\x01\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\0\x01\0\0".to_vec();
    wav.extend([0, 128].repeat(64));
    wav.extend([0, 64].repeat(64));
    std::fs::write(&sample, wav).unwrap();
    std::fs::write(&invalid, b"not audio").unwrap();
    let mutation = |action| alice.try_send(Command::AdminSoundboard(action)).unwrap();
    let crop = SoundboardCrop {
        start: 0.008,
        end: 0.016,
        gain: 50,
    };
    let add = || SoundboardAdminAction::Add {
        crop,
        name: " Ping ".into(),
        icon: "bell".into(),
    };
    let edit = || SoundboardAdminAction::Edit {
        id: ID.into(),
        name: "Bells".into(),
        icon: "music".into(),
    };
    timeout(Duration::from_secs(15), async {
        for _ in 0..2 {
            let mut cookie = None;
            let socket = accept_hdr_async(
                sockets.accept().await.unwrap().0,
                |request: &server::Request, response: server::Response| {
                    cookie = Some(request.headers()["cookie"].to_str().unwrap().to_owned());
                    Ok(response)
                },
            )
            .await
            .unwrap();
            config.insert(cookie.unwrap(), socket);
        }
        let bob_socket = config.get_mut("session=bob").unwrap();
        wait_for_sidebar(&bob_views, |view| {
            view.channels.is_some() && matches!(view.connection, Connection::Online)
        })
        .await;
        bob.send(Command::AdminSoundboard(SoundboardAdminAction::Delete(
            ID.into(),
        )))
        .await
        .unwrap();
        wait_for_sidebar(&bob_views, |view| {
            view.admin_soundboard.error == Some("Admin access is required.")
        })
        .await;
        bob.send(Command::JoinVoice("first".into())).await.unwrap();
        let join = receive_voice_frame(bob_socket).await;
        confirm_voice_join(bob_socket, &join).await;
        wait_for_sidebar(&bob_views, |view| {
            view.voice
                .as_ref()
                .is_some_and(|voice| voice.is_participating())
        })
        .await;

        mutation(SoundboardAdminAction::Show);
        wait_for_sidebar(&alice_views, |view| view.admin_soundboard.clips.is_some()).await;
        mutation(SoundboardAdminAction::ChooseFile(invalid));
        wait_for_sidebar(&alice_views, |view| {
            view.admin_soundboard.error == Some("Choose a supported mono or stereo audio file.")
        })
        .await;
        mutation(SoundboardAdminAction::ChooseFile(sample));
        wait_for_sidebar(&alice_views, |view| {
            view.admin_soundboard.duration == Some(0.016)
        })
        .await;
        mutation(SoundboardAdminAction::Preview(SoundboardCrop {
            end: 1.0,
            ..crop
        }));
        wait_for_sidebar(&alice_views, |view| {
            view.admin_soundboard.error.is_some() && !view.admin_soundboard.previewing
        })
        .await;
        mutation(SoundboardAdminAction::Preview(crop));
        wait_for_sidebar(&alice_views, |view| view.admin_soundboard.previewing).await;
        mutation(SoundboardAdminAction::StopPreview);
        wait_for_sidebar(&alice_views, |view| !view.admin_soundboard.previewing).await;
        mutation(add());
        wait_for_sidebar(&alice_views, |view| {
            view.admin_soundboard.error == Some("Could not save the sound. Try again.")
                && view.admin_soundboard.duration == Some(0.016)
        })
        .await;
        mutation(add());
        wait_for_sidebar(&alice_views, |view| {
            view.admin_soundboard
                .clips
                .as_ref()
                .is_some_and(|clips| clips.iter().any(|clip| clip.name == "Ping"))
        })
        .await;
        for expected in [Some("Ping"), Some("Bells"), None] {
            match expected {
                Some("Bells") => {
                    mutation(edit());
                    wait_for_sidebar(&alice_views, |view| {
                        view.admin_soundboard.error == Some("Admin access is required.")
                    })
                    .await;
                    mutation(edit());
                    wait_for_sidebar(&alice_views, |view| {
                        view.admin_soundboard
                            .clips
                            .as_ref()
                            .is_some_and(|clips| clips.iter().any(|clip| clip.name == "Bells"))
                    })
                    .await;
                }
                None => {
                    mutation(SoundboardAdminAction::Delete(ID.into()));
                    wait_for_sidebar(&alice_views, |view| {
                        view.admin_soundboard
                            .clips
                            .as_ref()
                            .is_some_and(|clips| clips.is_empty())
                    })
                    .await;
                }
                _ => {}
            }
            bob.send(Command::LoadSoundboard).await.unwrap();
            wait_for_sidebar(&bob_views, |view| {
                view.soundboard
                    .clips
                    .as_ref()
                    .is_some_and(|clips| clips.first().map(|clip| clip.name.as_str()) == expected)
            })
            .await;
        }
    })
    .await
    .expect("Soundboard administration workflow timed out");
    alice.close();
    bob.close();
    assert!(matches!(alice_session.await.unwrap(), SessionEnd::Closed));
    assert!(matches!(bob_session.await.unwrap(), SessionEnd::Closed));
    storage.await.unwrap();
    server.abort();
}
