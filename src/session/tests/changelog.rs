use super::*;

#[tokio::test]
async fn loads_authenticated_changelog_pages_and_can_retry_a_failed_page() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (requests, changelog) = async_channel::unbounded();
    let server =
        tokio::spawn(async move {
            loop {
                let (line, request) = Request::receive(&http).await;
                if line == "POST /api/auth/sign-in/email HTTP/1.1" {
                    request
                        .respond(&["session=notes; Path=/; Max-Age=3600"], "{}")
                        .await;
                    continue;
                }
                assert_eq!(request.header("cookie"), Some("session=notes"));
                match line.as_str() {
                    "GET /api/profile/me HTTP/1.1" => request
                        .respond(
                            &[],
                            r#"{"id":"alice","email":"alice@example.test","displayName":"Alice"}"#,
                        )
                        .await,
                    "GET /api/channels HTTP/1.1"
                    | "GET /api/users HTTP/1.1"
                    | "GET /api/voice HTTP/1.1" => request.respond(&[], "[]").await,
                    "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => {
                        request.respond(&[], "{}").await
                    }
                    _ => requests.send((line, request)).await.unwrap(),
                }
            }
        });
    let (tx, commands) = async_channel::unbounded();
    let (views, rx) = async_channel::unbounded();
    let session = tokio::spawn(async move {
        let (saved, _) = SavedSession::with_test_store(None);
        run_session(
            base,
            saved,
            fixture_settings(socket_url, Call::start_for_test),
            commands,
            views,
        )
        .await;
    });
    timeout(Duration::from_secs(10), async {
        assert!(matches!(rx.recv().await.unwrap(), View::SignIn { .. }));
        tx.send(Command::SignIn {
            email: "alice@example.test".into(),
            password: "test password".into(),
        })
        .await
        .unwrap();
        let _peer = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        let sha = "c".repeat(40);
        let (reply, result) = oneshot::channel();
        tx.send(Command::LoadChangelog {
            page: 1,
            head: None,
            reply,
        })
        .await
        .unwrap();
        let (line, request) = changelog.recv().await.unwrap();
        assert_eq!(line, "GET /api/changelog?page=1 HTTP/1.1");
        request
            .respond(
                &[],
                &value!({
                    "commits": [{"sha": sha, "date": "2026-11-21", "message": "Add a shortcut"}],
                    "head": sha, "nextPage": 2
                })
                .to_string(),
            )
            .await;
        let first = result.await.unwrap().unwrap();
        assert_eq!(first.commits[0].message, "Add a shortcut");
        let next = first.next_page.unwrap();
        assert_eq!(next, 2);
        for fail in [true, false] {
            let (reply, result) = oneshot::channel();
            tx.send(Command::LoadChangelog {
                page: next,
                head: first.head.clone(),
                reply,
            })
            .await
            .unwrap();
            let (line, request) = changelog.recv().await.unwrap();
            assert_eq!(
                line,
                format!("GET /api/changelog?page=2&head={sha} HTTP/1.1")
            );
            if fail {
                request
                    .respond_raw("503 Service Unavailable", &[], b"{}")
                    .await;
                assert!(matches!(
                    result.await.unwrap(),
                    Err(Failure::Http(reqwest::StatusCode::SERVICE_UNAVAILABLE))
                ));
            } else {
                request
                    .respond(
                        &[],
                        &value!({"commits": [], "head": sha, "nextPage": null}).to_string(),
                    )
                    .await;
                assert!(result.await.unwrap().unwrap().next_page.is_none());
            }
        }
    })
    .await
    .unwrap();
    session.abort();
    server.abort();
}
