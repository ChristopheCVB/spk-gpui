use super::*;
use crate::api::SearchScope;

#[tokio::test]
async fn searches_accessible_messages_with_spk_scopes_and_pages() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (requests, search) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            assert_eq!(request.header("cookie"), Some("session=search"));
            match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    request.respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"dm-1","type":"dm","otherUserId":"bob"}]"#).await
                }
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
                _ => requests.send((line, request)).await.unwrap(),
            }
        }
    });
    let mut cookies = CookieStore::default();
    cookies.parse("session=search; Path=/", &base).unwrap();
    let client = SpkClient::new(base, cookies).unwrap();
    let profile =
        serde_json::from_value(value!({"id":"alice","email":"alice@example.test"})).unwrap();
    let (tx, commands) = async_channel::unbounded();
    let (views, rx) = async_channel::unbounded();
    let session = tokio::spawn(async move {
        let (mut saved, _) = SavedSession::with_test_store(None);
        let mut settings = fixture_settings(socket_url, Call::start_for_test);
        watch_session(
            &client,
            profile,
            None,
            &mut saved,
            &mut settings,
            &commands,
            &views,
        )
        .await
    });
    timeout(Duration::from_secs(10), async {
        let _peer = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        wait_for_sidebar(&rx, |view| {
            view.channels.is_some() && matches!(view.connection, Connection::Online)
        })
        .await;
        let cases = [
            (
                SearchScope::Conversation("general".into()),
                "scope=channel&channelId=general",
                "c1",
            ),
            (SearchScope::TextChannels, "scope=channels", "c2"),
            (SearchScope::DirectMessages, "scope=dms", "d1"),
            (SearchScope::Everything, "scope=everything", "m1"),
        ];
        for (scope, expected, found) in cases {
            tx.send(Command::SearchMessages {
                query: "from:Bob word".into(),
                scope,
            })
            .await
            .unwrap();
            let (line, request) = search.recv().await.unwrap();
            assert_eq!(
                line,
                format!("GET /api/search?q=from%3ABob+word&{expected} HTTP/1.1")
            );
            request.respond(&[], &result(found, Some("20"))).await;
            wait_for_sidebar(&rx, |view| {
                !view.search.loading
                    && view
                        .search
                        .results
                        .first()
                        .is_some_and(|hit| hit.target.message_id() == found)
            })
            .await;
        }

        tx.send(Command::MoreSearchResults).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(
            line,
            "GET /api/search?q=from%3ABob+word&scope=everything&cursor=20 HTTP/1.1"
        );
        request.respond(&[], &result("m2", None)).await;
        wait_for_sidebar(&rx, |view| {
            view.search
                .results
                .iter()
                .map(|hit| hit.target.message_id())
                .eq(["m1", "m2"])
        })
        .await;

        tx.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("loopback search timed out");
    http.abort();
}

fn result(id: &str, next: Option<&str>) -> String {
    value!({
        "results": [{
            "messageId": id,
            "channelId": "general",
            "userId": "bob",
            "createdAt": 12,
            "snippet": "\u{1}word\u{2}",
            "text": "**word**",
            "attachments": 0
        }],
        "nextCursor": next
    })
    .to_string()
}
