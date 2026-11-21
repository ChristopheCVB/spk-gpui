use super::*;

#[cfg(debug_assertions)]
#[tokio::test]
async fn profile_changes_reach_another_session_and_failed_edits_keep_the_profile() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_endpoint = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (changed, updated) = watch::channel(0);
    let image = include_bytes!("../../../tests/fixtures/animated-avatar.webp");
    let (alice_tx, alice_rx) = async_channel::unbounded();
    let (bob_tx, bob_rx) = async_channel::unbounded();
    let (alice_views, alice) = async_channel::unbounded();
    let (bob_views, bob) = async_channel::unbounded();

    let serve = async {
        let mut directory = value!([
            {"id":"alice","displayName":"Alice","role":"user"},
            {"id":"bob","displayName":"Bob","role":"admin"}
        ]);
        let mut upload_attempt = 0;
        loop {
            let (line, request) = Request::receive(&http).await;
            let cookie = request.header("cookie");
            match line.as_str() {
                "GET /api/unread HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels HTTP/1.1" | "GET /api/voice HTTP/1.1" => {
                    request.respond(&[], "[]").await;
                }
                "GET /api/users HTTP/1.1" => request.respond(&[], &directory.to_string()).await,
                "GET /api/admin/users HTTP/1.1" => {
                    assert_eq!(cookie, Some("session=bob"));
                    request
                        .respond(
                            &[],
                            r#"{"users":[{"id":"alice","email":"alice@example.test"}]}"#,
                        )
                        .await;
                }
                "POST /api/profile/nickname HTTP/1.1" => {
                    assert_eq!(cookie, Some("session=alice"));
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    if body == value!({"displayName":"Bob"}) {
                        request.respond_raw("409 Conflict", &[], b"").await;
                    } else {
                        assert_eq!(body, value!({"displayName":"Alicia"}));
                        directory[0]["displayName"] = body["displayName"].clone();
                        changed.send_modify(|revision| *revision += 1);
                        request.respond(&[], &body.to_string()).await;
                    }
                }
                "POST /api/profile/avatar-upload HTTP/1.1" => {
                    assert_eq!(cookie, Some("session=alice"));
                    assert_eq!(
                        serde_json::from_slice::<Value>(&request.body).unwrap(),
                        value!({"size":image.len(),"type":"image/webp"})
                    );
                    request
                        .respond(
                            &[],
                            &value!({"key":"raw-key","url":base.join("/upload").unwrap().as_str()})
                                .to_string(),
                        )
                        .await;
                }
                "PUT /upload HTTP/1.1" => {
                    assert_eq!(cookie, None);
                    assert_eq!(request.header("origin"), None);
                    assert_eq!(request.header("content-type"), Some("image/webp"));
                    assert_eq!(request.body, image);
                    upload_attempt += 1;
                    if upload_attempt == 1 {
                        request.respond_raw("200 OK", &[], b"").await;
                    } else {
                        request.respond_raw("401 Unauthorized", &[], b"").await;
                    }
                }
                "POST /api/profile/avatar HTTP/1.1" => {
                    assert_eq!(upload_attempt, 1, "a failed storage upload must not commit");
                    assert_eq!(cookie, Some("session=alice"));
                    assert_eq!(
                        serde_json::from_slice::<Value>(&request.body).unwrap(),
                        value!({"key":"raw-key"})
                    );
                    directory[0]["avatarKey"] = value!("avatars/alice/new.webp");
                    changed.send_modify(|revision| *revision += 1);
                    request
                        .respond(&[], r#"{"avatarKey":"avatars/alice/new.webp"}"#)
                        .await;
                }
                "GET /api/avatar/alice?v=avatars%2Falice%2Fnew.webp HTTP/1.1" => {
                    assert!(matches!(cookie, Some("session=alice" | "session=bob")));
                    let location = format!("Location: {}", base.join("/avatar").unwrap());
                    request.respond_raw("302 Found", &[&location], b"").await;
                }
                "GET /avatar HTTP/1.1" => {
                    assert_eq!(cookie, None);
                    request.respond_raw("200 OK", &[], image).await;
                }
                _ => panic!("unexpected request: {line}"),
            }
        }
    };

    async fn relay(sockets: &TcpListener, mut updated: watch::Receiver<i32>) {
        let (tcp, _) = sockets.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        while updated.changed().await.is_ok() {
            if socket
                .send(Message::text(r#"{"type":"users:changed"}"#))
                .await
                .is_err()
            {
                break;
            }
        }
    }

    let client = |id: &str| {
        let mut cookies = CookieStore::default();
        cookies
            .parse(&format!("session={id}; Path=/"), &base)
            .unwrap();
        SpkClient::new(base.clone(), cookies).unwrap()
    };
    let session = async |id: &str, name: &str, role: &str, commands, views| {
        let client = client(id);
        let profile = serde_json::from_value(value!({
            "id":id, "email":format!("{id}@example.test"), "displayName":name, "role":role
        }))
        .unwrap();
        let (mut saved, _) = SavedSession::with_test_store(None);
        let mut settings = fixture_settings(socket_endpoint.clone(), |_, _, _, _| {
            panic!("opening a profile must not start voice")
        });
        watch_session(
            &client,
            profile,
            None,
            &mut saved,
            &mut settings,
            commands,
            views,
        )
        .await
    };

    let workflow = async {
        wait_for_sidebar(&alice, |s| {
            matches!(s.connection, Connection::Online) && !s.people().is_empty()
        })
        .await;
        wait_for_sidebar(&bob, |s| {
            matches!(s.connection, Connection::Online) && !s.people().is_empty()
        })
        .await;

        let (reply, result) = oneshot::channel();
        alice_tx
            .send(Command::InspectProfileEmail("bob".into(), reply))
            .await
            .unwrap();
        assert_eq!(result.await.unwrap(), None);
        let (reply, result) = oneshot::channel();
        bob_tx
            .send(Command::InspectProfileEmail("alice".into(), reply))
            .await
            .unwrap();
        assert_eq!(result.await.unwrap().as_deref(), Some("alice@example.test"));

        alice_tx
            .send(Command::SaveNickname("Alicia".into()))
            .await
            .unwrap();
        let own = wait_for_sidebar(&alice, |s| s.profile_edit == ProfileEdit::Saved).await;
        assert_eq!(own.profile.name(), "Alicia");
        let peer = wait_for_sidebar(&bob, |s| s.people() == [("alice", "Alicia")]).await;
        assert_eq!(peer.person("alice").unwrap().role, "user");

        alice_tx
            .send(Command::SaveNickname("Bob".into()))
            .await
            .unwrap();
        let own =
            wait_for_sidebar(&alice, |s| matches!(s.profile_edit, ProfileEdit::Failed(_))).await;
        assert_eq!(own.profile.name(), "Alicia");

        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, image).unwrap();
        let path = file.path().to_owned();
        alice_tx
            .send(Command::UploadAvatar(path.clone()))
            .await
            .unwrap();
        let own = wait_for_sidebar(&alice, |s| {
            s.profile_edit == ProfileEdit::Uploaded && s.avatar(Some("alice")).is_some()
        })
        .await;
        assert_eq!(
            own.profile.avatar_key.as_deref(),
            Some("avatars/alice/new.webp")
        );
        wait_for_sidebar(&bob, |s| s.avatar(Some("alice")).is_some()).await;

        alice_tx.send(Command::UploadAvatar(path)).await.unwrap();
        let own =
            wait_for_sidebar(&alice, |s| matches!(s.profile_edit, ProfileEdit::Failed(_))).await;
        assert_eq!(own.profile.name(), "Alicia");
        assert_eq!(
            own.profile.avatar_key.as_deref(),
            Some("avatars/alice/new.webp")
        );
        assert!(own.avatar(Some("alice")).is_some());
    };

    timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = serve => unreachable!(),
            () = workflow => {}
            _ = session("alice", "Alice", "user", &alice_rx, &alice_views) => panic!("Alice signed out"),
            _ = session("bob", "Bob", "admin", &bob_rx, &bob_views) => panic!("Bob signed out"),
            _ = async { tokio::join!(relay(&sockets, updated.clone()), relay(&sockets, updated)); } => unreachable!(),
        }
    }).await.expect("profile workflow timed out");
}
