use super::*;
use crate::{
    api::{SearchScope, conversation_link, conversation_permalink},
    notifications,
};

#[tokio::test]
async fn searches_messages_and_opens_authenticated_exact_targets() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (requests, search) = async_channel::unbounded();
    let (hold_channels, channels_held) = watch::channel(false);
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
                "GET /api/channels HTTP/1.1" if *channels_held.borrow() => {
                    requests.send((line, request)).await.unwrap()
                }
                "GET /api/channels HTTP/1.1" => {
                    request.respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"dm-1","type":"dm","otherUserId":"bob"},{"id":"builds","type":"webhook","name":"Builds","ownerId":"bob"}]"#).await
                }
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels/general/pins HTTP/1.1" | "GET /api/channels/dm-1/pins HTTP/1.1" => request.respond(&[], "[]").await,
                _ => requests.send((line, request)).await.unwrap(),
            }
        }
    });
    let link_base = base.clone();
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
        request.respond_raw("401 Unauthorized", &[], b"").await;
        while !matches!(rx.recv().await.unwrap(), View::SignIn { .. }) {}
        tx.send(Command::SignIn {
            email: "alice@example.test".into(),
            password: "test password".into(),
        }).await.unwrap();
        let mut peer = accept_async(sockets.accept().await.unwrap().0)
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
            view.chat.as_ref().is_some_and(|chat| chat.jump.as_ref().is_some_and(|jump| jump.message_id == "startup"))
        }).await;
        let cases = [
            (
                SearchScope::Conversation("general".into()),
                "scope=channel&channelId=general",
                "general",
                "c1",
            ),
            (SearchScope::TextChannels, "scope=channels", "general", "c2"),
            (SearchScope::DirectMessages, "scope=dms", "dm-1", "d1"),
            (SearchScope::Everything, "scope=everything", "builds", "m1"),
        ];
        for (scope, expected, channel, found) in cases {
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
            request.respond(&[], &result_in(channel, found, Some("20"))).await;
            wait_for_sidebar(&rx, |view| {
                !view.search.loading
                    && view
                        .search
                        .results
                        .first()
                        .is_some_and(|hit| hit.target.channel_id() == channel && hit.target.message_id() == found)
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

        hold_channels.send_replace(true);
        peer.send(Message::text(r#"{"type":"channels:changed"}"#)).await.unwrap();
        let (line, channel_refresh) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels HTTP/1.1");
        for (scope, query, channel) in [
            (SearchScope::TextChannels, "channels", "general"),
            (SearchScope::DirectMessages, "dms", "dm-1"),
        ] {
            tx.send(Command::SearchMessages { query: "word".into(), scope }).await.unwrap();
            let (line, request) = search.recv().await.unwrap();
            assert_eq!(line, format!("GET /api/search?q=word&scope={query} HTTP/1.1"));
            request.respond(&[], &result_in(channel, query, None)).await;
            wait_for_sidebar(&rx, |view| {
                view.search.results.first().is_some_and(|hit| {
                    hit.target.channel_id() == channel && hit.target.message_id() == query
                })
            }).await;
        }

        channel_refresh.respond_raw("503 Service Unavailable", &[], b"").await;
        wait_for_sidebar(&rx, |view| view.load_failed).await;

        tx.send(Command::SearchMessages { query: "word".into(), scope: SearchScope::Everything }).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/search?q=word&scope=everything HTTP/1.1");
        request.respond(&[], &value!({
            "results": [
                {"messageId":"webhook","channelId":"unlisted-builds","userId":"bob","createdAt":2,"snippet":"Build word","text":"Build word","attachments":0},
                {"messageId":"text","channelId":"general","userId":"bob","createdAt":1,"snippet":"Text word","text":"Text word","attachments":0}
            ],
            "nextCursor":"20"
        }).to_string()).await;
        let (line, access_check) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels HTTP/1.1");
        hold_channels.send_replace(false);
        access_check.respond_raw("503 Service Unavailable", &[], b"").await;
        let found = wait_for_sidebar(&rx, |view| view.search.next_cursor.as_deref() == Some("20")).await;
        assert!(found.search.results.iter().map(|hit| hit.target.message_id()).eq(["text"]));
        assert_eq!(found.search.next_cursor.as_deref(), Some("20"));

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
            view.chat.as_ref().is_some_and(|chat| chat.jump.as_ref().is_some_and(|jump| jump.message_id == "m2"))
        }).await;

        // An activated alert is checked again, even if the target exists in the current view.
        activate.send_replace(Some(old));
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/general/messages?around=m2 HTTP/1.1");
        request.respond_raw("404 Not Found", &[], b"").await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.jump_failed && chat.messages.is_empty())
        }).await;

        for (channel, status, reselect) in [
            ("general", "403 Forbidden", Command::OpenTextChannel("general".into())),
            ("dm-1", "503 Service Unavailable", Command::OpenDirectMessage("dm-1".into())),
        ] {
            let target = MessageTarget::new(channel, "target").unwrap();
            let around = format!("GET /api/channels/{channel}/messages?around=target HTTP/1.1");
            tx.send(Command::OpenMessageTarget(target.clone())).await.unwrap();
            let (line, request) = search.recv().await.unwrap();
            assert_eq!(line, around);
            request.respond(&[], &value!({
                "messages":[
                    {"id":"neighbor","channelId":channel,"userId":"bob","body":"Earlier message","createdAt":12},
                    {"id":"target","channelId":channel,"userId":"bob","body":"Alert target","createdAt":13}
                ],
                "hasMoreBefore":false,
                "hasMoreAfter":false,
            }).to_string()).await;
            wait_for_sidebar(&rx, |view| {
                view.chat.as_ref().is_some_and(|chat| chat.messages.len() == 2)
            }).await;

            activate.send_replace(Some(target));
            let (line, request) = search.recv().await.unwrap();
            assert_eq!(line, around);
            let pending = wait_for_sidebar(&rx, |view| view.chat_notice == Some("Opening message…")).await;
            assert!(pending.chat.is_none(), "authorization must finish before opening an alert's conversation");

            tx.send(reselect).await.unwrap();
            tx.send(Command::SearchMessages {
                query: "reselected".into(),
                scope: SearchScope::Everything,
            }).await.unwrap();
            let after_reselect = wait_for_sidebar(&rx, |view| view.chat.is_some() || view.search.loading).await;
            assert!(after_reselect.chat.is_none(), "reselecting {channel} must not expose it before authorization");
            let (line, search_request) = search.recv().await.unwrap();
            assert_eq!(line, "GET /api/search?q=reselected&scope=everything HTTP/1.1");
            search_request.respond(&[], &result("search-hit", None)).await;
            wait_for_sidebar(&rx, |view| !view.search.loading).await;

            request.respond_raw(status, &[], b"").await;
            let finished = wait_for_sidebar(&rx, |view| {
                view.action_results == [ActionResult::error("Message unavailable.")]
            }).await;
            assert!(finished.chat.is_none(), "{status} must keep the alert's conversation closed");
            assert_eq!(finished.chat_notice, None);
        }

        let unknown = MessageTarget::new("unknown", "private").unwrap();
        tx.send(Command::OpenMessageTarget(unknown)).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat_notice == Some("Opening message…")).await;
        wait_for_sidebar(&rx, |view| view.action_results == [ActionResult::error("Message unavailable.")]).await;

        tx.send(Command::OpenConversationTarget("dm-1".into())).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/dm-1/messages?unread=1 HTTP/1.1");
        request.respond(&[], r#"{"messages":[],"hasMoreBefore":false,"hasMoreAfter":false}"#).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.channel_id == "dm-1" && chat.history_ready())
        }).await;

        let feed_link = conversation_permalink(&link_base, "builds").unwrap();
        assert_eq!(feed_link.path(), "/c/builds");
        tx.send(Command::OpenConversationTarget(
            conversation_link(&link_base, &feed_link).unwrap().into(),
        )).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/builds/messages?unread=1 HTTP/1.1");
        request.respond(&[], r#"{"messages":[],"hasMoreBefore":false,"hasMoreAfter":false}"#).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| chat.webhook && chat.history_ready())).await;

        tx.send(Command::SearchMessages {
            query: "build".into(),
            scope: SearchScope::Conversation("builds".into()),
        }).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/search?q=build&scope=channel&channelId=builds HTTP/1.1");
        request.respond(&[], &result_in("builds", "b1", None)).await;
        let results = wait_for_sidebar(&rx, |view| {
            view.search.results.first().is_some_and(|hit| hit.target.message_id() == "b1")
        }).await;
        let hit = &results.search.results[0];
        assert_eq!(hit.target.channel_id(), "builds");
        tx.send(Command::OpenMessageTarget(hit.target.clone())).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/builds/messages?around=b1 HTTP/1.1");
        request.respond(&[], r#"{"messages":[{"id":"b1","channelId":"builds","userId":"bob","body":"Build passed","createdAt":9}],"hasMoreBefore":false,"hasMoreAfter":false}"#).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.jump.as_ref().is_some_and(|jump| jump.message_id == "b1"))
        }).await;

        let missing_alert = link_base.join("/c/builds/m/removed").unwrap();
        tx.send(Command::OpenMessageTarget(MessageTarget::parse(&link_base, &missing_alert).unwrap())).await.unwrap();
        let (line, request) = search.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/builds/messages?around=removed HTTP/1.1");
        request.respond_raw("404 Not Found", &[], b"").await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| chat.webhook && chat.jump_failed)).await;

        let private_feed = link_base.join("/c/private-feed").unwrap();
        tx.send(Command::OpenConversationTarget(conversation_link(&link_base, &private_feed).unwrap().into())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.action_results == [ActionResult::error("Conversation unavailable.")]).await;
        let private_alert = link_base.join("/c/private-feed/m/hidden").unwrap();
        tx.send(Command::OpenMessageTarget(MessageTarget::parse(&link_base, &private_alert).unwrap())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.action_results == [ActionResult::error("Message unavailable.")]).await;
        assert!(search.is_empty(), "the session must not fetch history for an unlisted feed");

        tx.close();
        session.await.unwrap();
    })
    .await
    .expect("loopback search timed out");
    http.abort();
}

fn result(id: &str, next: Option<&str>) -> String {
    result_in("general", id, next)
}

fn result_in(channel: &str, id: &str, next: Option<&str>) -> String {
    value!({
        "results": [{
            "messageId": id,
            "channelId": channel,
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
