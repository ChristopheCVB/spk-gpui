use super::*;
use crate::{api::SearchScope, notifications};

#[tokio::test]
async fn searches_messages_and_opens_authenticated_exact_targets() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (requests, search) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            if line == "POST /api/auth/sign-in/email HTTP/1.1" {
                assert_eq!(request.header("cookie"), None);
                request
                    .respond(&["session=search; Path=/; Max-Age=3600/"], "{}")
                    .await;
                continue;
            }
            assert_eq!(request.header("cookie"), Some("session=search"));
            match line.as_str() {
                "GET /api/profile/me HTTP/1.1" => {
                    request.respond(&[], r#"{"id":"alice","email":"alice@example.test","displayName":"Alice"}"#).await
                }
                "GET /api/channels HTTP/1.1" => {
                    request.respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"dm-1","type":"dm","otherUserId":"bob"}]"#).await
                }
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
                _ => requests.send((line, request)).await.unwrap(),
            }
        }
    });
    let (tx, commands) = async_channel::unbounded();
    let (views, rx) = async_channel::unbounded();
    let (activate, activation) = watch::channel(None);
    let launch = MessageTarget::new("general", "startup").unwrap();
    activate.send_replace(notifications::activation_target(
        &base,
        &notifications::activation_tag(&base, &launch),
    ));
    let session = tokio::spawn(async move {
        let (saved, _) = SavedSession::with_test_store(None);
        let mut settings = fixture_settings(socket_url, Call::start_for_test);
        settings.alert_activation = activation;
        run_session(base, saved, settings, commands, views).await;
    });
    timeout(Duration::from_secs(10), async {
        assert!(matches!(rx.recv().await.unwrap(), View::SignIn { .. }));
        assert!(search.is_empty());
        tx.send(Command::SignIn {
            email: "alice@example.test".into(),
            password: "test password".into(),
        }).await.unwrap();
        let _peer = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/general/messages?around=startup HTTP/1.1");
        request.respond(&[], &value!({
            "messages":[{"id":"startup","channelId":"general","userId":"bob","body":"First alert","createdAt":12}],
            "hasMoreBefore": false,
            "hasMoreAfter": false
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.jump.as_ref().is_some_and(|(id, _)| id == "startup"))
        }).await;
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

        let old = MessageTarget::parse(
            &Url::parse("https://spk.example.test").unwrap(),
            &Url::parse("https://spk.example.test/c/general/m/m2").unwrap(),
        ).unwrap();
        tx.send(Command::OpenMessageTarget(old.clone())).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/general/messages?around=m2 HTTP/1.1");
        request.respond(&[], &value!({
            "messages":[{"id":"m2","channelId":"general","userId":"bob","body":"word","createdAt":12}],
            "hasMoreBefore":true,
            "hasMoreAfter":true,
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.jump.as_ref().is_some_and(|(id, _)| id == "m2"))
        }).await;

        // An activated alert is checked again, even if the target exists in the current view.
        activate.send_replace(Some(old));
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/general/messages?around=m2 HTTP/1.1");
        request.respond_raw("404 Not Found", &[], b"").await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.jump_failed && chat.messages.is_empty())
        }).await;

        let private = MessageTarget::new("dm-1", "private").unwrap();
        tx.send(Command::OpenMessageTarget(private.clone())).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/dm-1/messages?around=private HTTP/1.1");
        request.respond(&[], &value!({
            "messages":[
                {"id":"neighbor","channelId":"dm-1","userId":"bob","body":"private context","createdAt":12},
                {"id":"private","channelId":"dm-1","userId":"bob","body":"private text","createdAt":13}
            ],
            "hasMoreBefore":false,
            "hasMoreAfter":false,
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.messages.len() == 2)
        }).await;

        tx.send(Command::OpenMessageTarget(private)).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/dm-1/messages?around=private HTTP/1.1");
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.channel_id == "dm-1" && chat.loading() && chat.messages.is_empty())
        }).await;
        request.respond_raw("403 Forbidden", &[], b"").await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.jump_failed && chat.unavailable())
        }).await;

        let unknown = MessageTarget::new("unknown", "private").unwrap();
        tx.send(Command::OpenMessageTarget(unknown)).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat_notice == Some("Message unavailable.")).await;

        tx.close();
        session.await.unwrap();
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
