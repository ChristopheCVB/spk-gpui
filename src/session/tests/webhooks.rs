mod management;
mod rich_alerts;

use super::*;

#[tokio::test]
async fn signed_in_reader_opens_visible_webhook_history() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (deletes, deleted) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        let mut fail_page = true;
        loop {
            let (line, request) = Request::receive(&http).await;
            let cookie = request.header("cookie").unwrap();
            let owner = cookie == "session=alice";
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" if owner => value!([
                    {"id":"general","type":"text","name":"general"},
                    {"id":"public","type":"webhook","name":"Builds","icon":"activity","ownerId":"alice","private":false},
                    {"id":"private","type":"webhook","name":"Personal","icon":"missing-icon","ownerId":"alice","private":true}
                ]),
                "GET /api/channels HTTP/1.1" => value!([
                    {"id":"public","type":"webhook","name":"Builds","icon":"activity","ownerId":"alice","private":false}
                ]),
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "GET /api/channels/public/messages?unread=1 HTTP/1.1" => value!({
                    "messages":[{"id":"build","channelId":"public","userId":"alice","createdAt":2,"body":"Build passed"}],
                    "hasMoreBefore":true,"hasMoreAfter":false
                }),
                "GET /api/channels/public/messages?before=build HTTP/1.1" if fail_page => {
                    fail_page = false;
                    request.respond_raw("503 Unavailable", &[], b"").await;
                    continue;
                }
                "GET /api/channels/public/messages?before=build HTTP/1.1" => value!({
                    "messages":[{"id":"older","channelId":"public","userId":"alice","createdAt":1,"body":"Earlier build"}],
                    "hasMoreBefore":false,"hasMoreAfter":false
                }),
                "GET /api/channels/private/messages?unread=1 HTTP/1.1" if owner => value!({
                    "messages":[],"hasMoreBefore":false,"hasMoreAfter":false
                }),
                "DELETE /api/messages/build HTTP/1.1" if cookie == "session=admin" => {
                    deletes.send(request).await.unwrap();
                    continue;
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (owner, owner_views, _, _owner_peer, owner_session) =
            chat::direct_message_session(base.clone(), "alice").await;
        let (reader, reader_views, _, mut reader_peer, reader_session) =
            chat::direct_message_session(base.clone(), "bob").await;
        let (admin, admin_views, _, _admin_peer, admin_session) =
            chat::signed_in_chat_session(base, "admin", "admin").await;
        let owned = wait_for_sidebar(&owner_views, |view| view.channels.is_some()).await;
        let feeds = owned.webhook_feeds().collect::<Vec<_>>();
        assert_eq!(
            feeds
                .iter()
                .map(|feed| feed.id.as_str())
                .collect::<Vec<_>>(),
            ["public", "private"]
        );
        assert_eq!(feeds[0].name.as_deref(), Some("Builds"));
        assert_eq!(feeds[0].icon.as_deref(), Some("activity"));
        assert!(feeds[1].private);

        owner
            .send(Command::OpenWebhook("private".into()))
            .await
            .unwrap();
        let opened = wait_for_sidebar(&owner_views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_ready)
        })
        .await;
        assert!(opened.chat.unwrap().messages.is_empty());

        reader
            .send(Command::OpenWebhook("private".into()))
            .await
            .unwrap();
        let denied = wait_for_sidebar(&reader_views, |view| view.chat_notice.is_some()).await;
        assert_eq!(
            denied
                .webhook_feeds()
                .map(|feed| feed.id.as_str())
                .collect::<Vec<_>>(),
            ["public"]
        );
        assert!(denied.chat.is_none());

        reader
            .send(Command::OpenWebhook("public".into()))
            .await
            .unwrap();
        let opened = wait_for_sidebar(&reader_views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_ready)
        })
        .await;
        let feed = opened.chat.unwrap();
        assert!(!feed.can_send());
        assert!(feed.pins.is_none());
        assert_eq!(feed.messages[0].body, "Build passed");
        reader.send(Command::LoadOlderMessages).await.unwrap();
        wait_for_sidebar(&reader_views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_failed)
        })
        .await;
        reader.send(Command::RetryChatHistory).await.unwrap();
        let paged = wait_for_sidebar(&reader_views, |view| {
            view.chat
                .as_ref()
                .is_some_and(|chat| chat.messages.len() == 2)
        })
        .await;
        assert_eq!(paged.chat.unwrap().messages[0].id, "older");
        let delete = || Command::MutateChatMessage {
            channel_id: "public".into(),
            message_id: "build".into(),
            action: MessageAction::Delete,
        };
        reader.send(delete()).await.unwrap();
        wait_for_sidebar(&reader_views, |view| {
            view.chat
                .as_ref()
                .and_then(|chat| chat.action_outcome.as_ref())
                .is_some_and(|result| result.error.is_some())
        })
        .await;

        admin
            .send(Command::OpenWebhook("public".into()))
            .await
            .unwrap();
        wait_for_sidebar(&admin_views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_ready)
        })
        .await;
        admin.send(delete()).await.unwrap();
        deleted.recv().await.unwrap().respond(&[], "{}").await;
        reader_peer
            .send(Message::text(
                r#"{"type":"message:delete","channelId":"public","id":"build"}"#,
            ))
            .await
            .unwrap();
        wait_for_sidebar(&reader_views, |view| {
            view.chat
                .as_ref()
                .is_some_and(|chat| chat.messages.len() == 1 && chat.messages[0].id == "older")
        })
        .await;
        reader.close();
        owner.close();
        admin.close();
        assert!(matches!(admin_session.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(reader_session.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(owner_session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("webhook history timed out");
    http.abort();
}

#[tokio::test]
async fn owner_alerts_arrive_quietly_and_share_the_feed_read_position() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (marks, marked) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            assert_eq!(request.header("cookie"), Some("session=alice"));
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" => value!([
                    {"id":"feed","type":"webhook","name":"Builds","ownerId":"alice","private":false}
                ]),
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" => value!({}),
                "GET /api/unread HTTP/1.1" => value!({"feed":{"unread":1,"mentions":1}}),
                "GET /api/channels/feed/messages?unread=1 HTTP/1.1" => value!({
                    "messages":[{"id":"old","channelId":"feed","userId":"alice","createdAt":1,"body":"Build started"}],
                    "firstUnreadId":"old","hasMoreBefore":false,"hasMoreAfter":false
                }),
                "POST /api/channels/feed/read HTTP/1.1" => {
                    marks.send(None).await.unwrap();
                    value!({"unread":0,"mentions":0})
                }
                "POST /api/channels/feed/unread HTTP/1.1" => {
                    marks
                        .send(Some(
                            serde_json::from_slice::<Value>(&request.body).unwrap(),
                        ))
                        .await
                        .unwrap();
                    value!({"unread":1,"mentions":1})
                }
                "DELETE /api/messages/old HTTP/1.1" => {
                    request.respond_raw("403 Forbidden", &[], b"").await;
                    continue;
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (commands, views, _, mut peer, session) = chat::direct_message_session(base, "alice").await;
        let listed = wait_for_sidebar(&views, |view| view.unread("feed").unread == 1).await;
        assert_eq!(listed.unread("feed").mentions, 0);
        commands.send(Command::OpenWebhook("feed".into())).await.unwrap();
        let opened = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_ready)
        }).await;
        assert_eq!(opened.chat.unwrap().first_unread_id.as_deref(), Some("old"));

        peer.send(Message::text(r#"{"type":"message:new","message":{"id":"live","channelId":"feed","userId":"alice","createdAt":2,"body":"Build passed"}}"#)).await.unwrap();
        peer.send(Message::text(r#"{"type":"message:new","message":{"id":"external","channelId":"feed","userId":"integration","createdAt":3,"body":"Hi <@alice>"}}"#)).await.unwrap();
        peer.send(Message::text(r#"{"type":"unread:update","channelId":"feed","unread":3,"mentions":1}"#)).await.unwrap();
        loop {
            let View::SignedIn(view) = views.recv().await.unwrap() else {
                panic!("expected signed-in session");
            };
            assert!(view.alerts.is_empty());
            if view.unread("feed").unread == 3 {
                assert_eq!(view.unread("feed").mentions, 0);
                assert_eq!(view.chat.unwrap().messages[1].id, "live");
                break;
            }
        }

        commands.send(Command::ReadVisibleChat("feed".into())).await.unwrap();
        assert_eq!(marked.recv().await.unwrap(), None);
        wait_for_sidebar(&views, |view| view.unread("feed").unread == 0).await;

        commands.send(Command::MarkChatUnread { channel_id: "feed".into(), message_id: "live".into() }).await.unwrap();
        assert_eq!(marked.recv().await.unwrap(), Some(value!({"messageId":"live"})));
        let unread = wait_for_sidebar(&views, |view| view.unread("feed").unread == 1).await;
        assert_eq!(unread.chat.unwrap().first_unread_id.as_deref(), Some("live"));
        commands.send(Command::MarkChatRead("feed".into())).await.unwrap();
        assert_eq!(marked.recv().await.unwrap(), None);
        wait_for_sidebar(&views, |view| view.unread("feed").unread == 0).await;

        for command in [
            Command::UpdateChatDraft { channel_id: "feed".into(), body: "blocked".into() },
            Command::SetChatReply { channel_id: "feed".into(), message_id: Some("old".into()) },
            Command::SendChatMessage { channel_id: "feed".into(), body: "blocked".into() },
        ] {
            commands.send(command).await.unwrap();
        }
        for action in [
            MessageAction::Edit("blocked".into()),
            MessageAction::React { emoji: "👍".into(), on: true },
            MessageAction::Pin(true),
            MessageAction::Delete,
        ] {
            commands.send(Command::MutateChatMessage { channel_id: "feed".into(), message_id: "old".into(), action }).await.unwrap();
        }
        let denied = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().and_then(|chat| chat.action_outcome.as_ref())
                .is_some_and(|result| result.error.is_some())
        }).await;
        let feed = denied.chat.unwrap();
        assert!(feed.draft.body.is_empty());
        assert!(feed.draft.reply.is_none());
        assert!(feed.outgoing.is_none());
        assert!(!feed.can_pin(true));
        assert!(feed.messages.iter().any(|message| message.id == "old"));

        commands.send(Command::SetUserStatus(UserStatus::Busy)).await.unwrap();
        while let Some(frame) = peer.next().await {
            let frame = frame.unwrap();
            if let Message::Text(text) = frame {
                let payload: Value = serde_json::from_str(&text).unwrap();
                assert_ne!(payload["type"], "message:send");
                assert_ne!(payload["type"], "typing");
                if payload == value!({"type":"presence:set","status":"busy"}) {
                    break;
                }
            }
        }
        commands.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
    }).await.expect("webhook live read position timed out");
    assert!(!http.is_finished());
    http.abort();
}

#[tokio::test]
async fn channel_refresh_discards_revoked_webhook_history_and_in_flight_pages() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (revoke, revoked) = watch::channel(false);
    let (requests, pages) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" if !*revoked.borrow() => value!([
                    {"id":"feed","type":"webhook","name":"Builds","ownerId":"alice","private":false}
                ]),
                "GET /api/channels HTTP/1.1"
                | "GET /api/users HTTP/1.1"
                | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "GET /api/channels/feed/messages?unread=1 HTTP/1.1" => value!({
                    "messages":[{"id":"open","channelId":"feed","userId":"alice","createdAt":2,"body":"Build passed"}],
                    "hasMoreBefore":true,"hasMoreAfter":false
                }),
                "GET /api/channels/feed/messages?before=open HTTP/1.1"
                | "GET /api/search?q=build&scope=everything&cursor=20 HTTP/1.1" => {
                    requests.send((line, request)).await.unwrap();
                    continue;
                }
                "GET /api/search?q=build&scope=everything HTTP/1.1" => value!({
                    "results":[{
                        "messageId":"open","channelId":"feed","userId":"alice","createdAt":2,
                        "snippet":"Build passed","text":"Build passed","attachments":0
                    }],
                    "nextCursor":"20"
                }),
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (commands, views, _, mut peer, session) = chat::direct_message_session(base, "bob").await;
        commands.send(Command::OpenWebhook("feed".into())).await.unwrap();
        wait_for_sidebar(&views, |view| view.chat.as_ref().is_some_and(ChatView::history_ready)).await;
        commands.send(Command::LoadOlderMessages).await.unwrap();
        let (line, mut page) = pages.recv().await.unwrap();
        assert!(line.contains("before=open"));
        commands.send(Command::SearchMessages {
            query: "build".into(),
            scope: crate::api::SearchScope::Everything,
        }).await.unwrap();
        wait_for_sidebar(&views, |view| view.search.results.len() == 1).await;
        commands.send(Command::MoreSearchResults).await.unwrap();
        let (line, search_page) = pages.recv().await.unwrap();
        assert!(line.contains("cursor=20"));

        revoke.send_replace(true);
        peer.send(Message::text(r#"{"type":"channels:changed"}"#)).await.unwrap();
        let missing = wait_for_sidebar(&views, |view| view.webhook_feeds().next().is_none()).await;
        assert!(missing.chat.is_none());
        assert!(missing.search.results.is_empty());
        assert_eq!(missing.chat_notice, Some("Webhook feed unavailable."));

        let late_page = r#"{"messages":[{"id":"older","channelId":"feed","userId":"alice","createdAt":1,"body":"Earlier build"}],"hasMoreBefore":false,"hasMoreAfter":false}"#;
        let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{late_page}", late_page.len());
        // The client may close the request socket when it cancels the page.
        let _ = page.stream.write_all(response.as_bytes()).await;
        search_page.respond(&[], r#"{"results":[{"messageId":"older","channelId":"feed","userId":"alice","createdAt":1,"snippet":"Earlier build","text":"Earlier build","attachments":0}],"nextCursor":null}"#).await;
        wait_for_sidebar(&views, |view| !view.search.loading).await;
        peer.send(Message::text(r#"{"type":"message:new","message":{"id":"late","channelId":"feed","userId":"alice","createdAt":3,"body":"Late alert"}}"#)).await.unwrap();
        peer.send(Message::text(r#"{"type":"presence:update","userId":"alice","status":"busy"}"#)).await.unwrap();
        let after = wait_for_sidebar(&views, |view| view.user_presence("alice") == UserPresence::Busy).await;
        assert!(after.chat.is_none());
        assert!(after.search.results.is_empty());
        assert!(after.alerts.is_empty());
        assert_eq!(after.chat_notice, Some("Webhook feed unavailable."));

        commands.send(Command::SignOut).await.unwrap();
        assert!(matches!(session.await.unwrap(), SessionEnd::SignOut));
    }).await.expect("webhook access revocation timed out");
    assert!(!http.is_finished());
    http.abort();
}

mod creation {
    use super::*;
    use crate::{api::WebhookSettings, session::WebhookAction};

    #[tokio::test]
    async fn ordinary_user_creates_and_opens_webhook_with_one_time_url() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (requests, submissions) = async_channel::unbounded();
        let (created, listed) = watch::channel(false);
        let (hold_channels, holding_channels) = watch::channel(false);
        let (channel_requests, channel_replies) = async_channel::unbounded();
        let http = tokio::spawn(async move {
            loop {
                let (line, request) = Request::receive(&listener).await;
                assert_eq!(request.header("cookie"), Some("session=alice"));
                let body = match line.as_str() {
                    "GET /api/channels HTTP/1.1" if *holding_channels.borrow() => {
                        channel_requests.send(request).await.unwrap();
                        continue;
                    }
                    "GET /api/channels HTTP/1.1" if *listed.borrow() => value!([feed()]),
                    "GET /api/channels HTTP/1.1"
                    | "GET /api/users HTTP/1.1"
                    | "GET /api/voice HTTP/1.1" => value!([]),
                    "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                    "GET /api/channels/feed/messages?unread=1 HTTP/1.1" => value!({
                        "messages":[],"hasMoreBefore":false,"hasMoreAfter":false
                    }),
                    "POST /api/webhooks HTTP/1.1" => {
                        requests.send(request).await.unwrap();
                        continue;
                    }
                    _ => panic!("unexpected request {line}"),
                };
                request.respond(&[], &body.to_string()).await;
            }
        });

        timeout(Duration::from_secs(10), async {
            let (commands, views, _, mut peer, session) =
                chat::direct_message_session(base.clone(), "alice").await;
            let initial = wait_for_sidebar(&views, |_| true).await;
            assert!(!initial.is_admin());
            assert!(initial.webhook_feeds().next().is_none());

            hold_channels.send_replace(true);
            commands.send(Command::Refresh).await.unwrap();
            let stale_channels = channel_replies.recv().await.unwrap();
            let result = create(&commands, " \0Builds\u{feff} ", true).await;
            let request = submissions.recv().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).unwrap(),
                value!({"name":"Builds","icon":"webhook","private":true}),
            );
            assert!(matches!(
                create(&commands, "Duplicate", false).await.await.unwrap(),
                Err("A webhook change is already in progress.")
            ));
            created.send_replace(true);
            let url = format!("{base}api/hooks/feed/test-fixture-secret");
            request
                .respond(&[], &value!({"channel":feed(),"url":url}).to_string())
                .await;
            let issued = result.await.unwrap().unwrap().expect("webhook URL");
            assert!(issued.as_str() == url);
            let opened = wait_for_sidebar(&views, |view| {
                view.chat.as_ref().is_some_and(ChatView::history_ready)
            })
            .await;
            assert_eq!(opened.chat.unwrap().channel_id, "feed");

            stale_channels.respond(&[], "[]").await;
            let fresh_channels = channel_replies.recv().await.unwrap();
            peer.send(Message::text(
                r#"{"type":"presence:update","userId":"bob","status":"busy"}"#,
            ))
            .await
            .unwrap();
            let reconciling = wait_for_sidebar(&views, |view| {
                view.user_presence("bob") == UserPresence::Busy
            })
            .await;
            assert!(reconciling.webhook_feed("feed").is_some());
            assert_eq!(reconciling.chat.unwrap().channel_id, "feed");
            hold_channels.send_replace(false);
            fresh_channels
                .respond(&[], &value!([feed()]).to_string())
                .await;
            commands.send(Command::SignOut).await.unwrap();
            assert!(matches!(session.await.unwrap(), SessionEnd::SignOut));
        })
        .await
        .expect("webhook creation timed out");
        assert!(!http.is_finished());
        http.abort();
    }

    #[tokio::test]
    async fn webhook_create_errors_leave_existing_chat_available() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (requests, submissions) = async_channel::unbounded();
        let http = tokio::spawn(async move {
            loop {
                let (line, request) = Request::receive(&listener).await;
                assert_eq!(request.header("cookie"), Some("session=alice"));
                let body = match line.as_str() {
                    "GET /api/channels HTTP/1.1" => {
                        value!([{"id":"general","type":"text","name":"General"}])
                    }
                    "GET /api/users HTTP/1.1"
                    | "GET /api/voice HTTP/1.1"
                    | "GET /api/channels/general/pins HTTP/1.1" => value!([]),
                    "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                    "GET /api/channels/general/messages?unread=1 HTTP/1.1" => value!({
                        "messages":[],"hasMoreBefore":false,"hasMoreAfter":false
                    }),
                    "POST /api/webhooks HTTP/1.1" => {
                        requests.send(request).await.unwrap();
                        continue;
                    }
                    _ => panic!("unexpected request {line}"),
                };
                request.respond(&[], &body.to_string()).await;
            }
        });

        timeout(Duration::from_secs(10), async {
            let (commands, views, _, _peer, session) =
                chat::direct_message_session(base, "alice").await;
            assert!(matches!(
                create(&commands, &"😀".repeat(17), false)
                    .await
                    .await
                    .unwrap(),
                Err("Webhook names must be 1–32 characters.")
            ));
            for (status, expected_error) in [
                (
                    "404 Not Found",
                    "This SPK server needs a version that supports Webhooks.",
                ),
                (
                    "403 Forbidden",
                    "SPK denied permission to create this webhook.",
                ),
                (
                    "422 Unprocessable Entity",
                    "SPK rejected the webhook settings. Check the name and icon, then try again.",
                ),
                (
                    "503 Service Unavailable",
                    "Could not create the webhook. Refresh the feed list before trying again.",
                ),
            ] {
                let result = create(&commands, "Deployments", false).await;
                let request = submissions.recv().await.unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&request.body).unwrap(),
                    value!({"name":"Deployments","icon":"webhook","private":false}),
                );
                request
                    .respond_raw(status, &[], b"untrusted server error body")
                    .await;
                assert!(matches!(result.await.unwrap(), Err(message) if message == expected_error));
            }

            commands
                .send(Command::OpenTextChannel("general".into()))
                .await
                .unwrap();
            let opened = wait_for_sidebar(&views, |view| {
                view.chat.as_ref().is_some_and(ChatView::history_ready)
            })
            .await;
            assert!(opened.webhook_feeds().next().is_none());
            assert_eq!(opened.chat.unwrap().channel_id, "general");

            let signed_out = create(&commands, "Deployments", false).await;
            let pending = submissions.recv().await.unwrap();
            commands.send(Command::SignOut).await.unwrap();
            assert!(signed_out.await.is_err());
            drop(pending);
            assert!(matches!(session.await.unwrap(), SessionEnd::SignOut));
        })
        .await
        .expect("webhook failure handling timed out");
        assert!(!http.is_finished());
        http.abort();
    }

    async fn create(
        commands: &Sender<Command>,
        name: &str,
        private: bool,
    ) -> oneshot::Receiver<Result<Option<crate::api::WebhookUrl>, &'static str>> {
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::MutateWebhook {
                action: WebhookAction::Create(WebhookSettings {
                    name: name.to_owned(),
                    icon: "webhook".to_owned(),
                    private,
                }),
                reply,
            })
            .await
            .unwrap();
        result
    }

    fn feed() -> Value {
        value!({"id":"feed","type":"webhook","name":"Builds","icon":"webhook","ownerId":"alice","private":true})
    }
}
