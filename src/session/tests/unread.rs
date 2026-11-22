use super::*;

#[tokio::test]
async fn sidebar_shows_shared_unread_counts_and_pings() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (pause_snapshot, paused) = watch::channel(false);
    let (held_snapshots, snapshots) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            assert_eq!(request.header("cookie"), Some("session=alice"));
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    r#"[{"id":"general","type":"text","name":"general"},{"id":"private","type":"dm","otherUserId":"bob","lastActivityAt":1}]"#
                }
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => "[]",
                "GET /api/users/presence HTTP/1.1" => "{}",
                "GET /api/unread HTTP/1.1" if *paused.borrow() => {
                    held_snapshots.send(request).await.unwrap();
                    continue;
                }
                "GET /api/unread HTTP/1.1" => r#"{"general":{"unread":3,"mentions":2}}"#,
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], body).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (commands, views, _, mut peer, session) =
            chat::direct_message_session(base, "alice").await;
        let view = wait_for_sidebar(&views, |view| view.unread("general").unread == 3).await;
        assert_eq!(view.unread("general").mentions, 2);
        assert_eq!(view.unread("private").unread, 0);

        peer.send(Message::text(
            r#"{"type":"unread:update","channelId":"private","unread":2,"mentions":1}"#,
        ))
        .await
        .unwrap();
        let view = wait_for_sidebar(&views, |view| view.unread("private").unread == 2).await;
        assert_eq!(view.unread("private").mentions, 1);
        assert_eq!(view.unread("general").unread, 3);

        pause_snapshot.send_replace(true);
        commands.send(Command::Refresh).await.unwrap();
        let snapshot = snapshots.recv().await.unwrap();
        peer.send(Message::text(
            r#"{"type":"unread:update","channelId":"private","unread":4,"mentions":2}"#,
        ))
        .await
        .unwrap();
        wait_for_sidebar(&views, |view| view.unread("private").unread == 4).await;
        snapshot
            .respond(
                &[],
                r#"{"general":{"unread":5,"mentions":2},"private":{"unread":2,"mentions":1}}"#,
            )
            .await;
        let view = wait_for_sidebar(&views, |view| view.unread("general").unread == 5).await;
        assert_eq!(view.unread("private").unread, 4);
        assert_eq!(view.unread("private").mentions, 2);

        commands.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("loopback unread counts timed out");
    http.abort();
}

#[tokio::test]
async fn marking_read_and_unread_changes_the_shared_read_point() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (read_changes, changes) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        let mut count = 3_usize;
        loop {
            let (line, request) = Request::receive(&http).await;
            assert_eq!(request.header("cookie"), Some("session=alice"));
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    value!([{"id":"general","type":"text","name":"general"}])
                }
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" => value!({}),
                "GET /api/unread HTTP/1.1" => {
                    value!({"general":{"unread":count,"mentions":count.saturating_sub(1)}})
                }
                "GET /api/channels/general/pins HTTP/1.1" => value!([]),
                "GET /api/channels/general/messages?unread=1 HTTP/1.1" => value!({
                    "messages":[
                        {"id":"first","channelId":"general","userId":"bob","createdAt":1,"body":"Hi <@alice>"},
                        {"id":"reply","channelId":"general","userId":"bob","createdAt":2,"body":"I agree",
                         "replyTo":{"id":"original","userId":"alice","preview":"Hello","deleted":false}},
                        {"id":"code","channelId":"general","userId":"bob","createdAt":3,"body":"`<@alice>`\n\n~~~txt\n<@alice>\n~~~"}
                    ],
                    "firstUnreadId":"first","hasMoreBefore":false,"hasMoreAfter":false
                }),
                "GET /api/channels/general/messages?around=older HTTP/1.1" => value!({
                    "messages":[{"id":"older","channelId":"general","userId":"bob","createdAt":0,"body":"Earlier"}],
                    "hasMoreBefore":true,"hasMoreAfter":true
                }),
                "POST /api/channels/general/read HTTP/1.1" => {
                    count = 0;
                    let result = value!({"unread":0,"mentions":0});
                    read_changes.send(result.clone()).await.unwrap();
                    result
                }
                "POST /api/channels/general/unread HTTP/1.1" => {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&request.body).unwrap(),
                        value!({"messageId":"reply"})
                    );
                    count = 2;
                    let result = value!({"unread":2,"mentions":1});
                    read_changes.send(result.clone()).await.unwrap();
                    result
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (commands, views, _, _peer, session) =
            chat::direct_message_session(base.clone(), "alice").await;
        let (other, other_views, other_sockets, mut other_peer, other_session) =
            chat::direct_message_session(base, "alice").await;
        wait_for_sidebar(&views, |view| view.unread("general").unread == 3).await;
        wait_for_sidebar(&other_views, |view| view.unread("general").unread == 3).await;

        commands
            .send(Command::OpenTextChannel("general".into()))
            .await
            .unwrap();
        let opened = wait_for_sidebar(&views, |view| {
            view.chat
                .as_ref()
                .and_then(|chat| chat.first_unread_id.as_deref())
                == Some("first")
        })
        .await;
        let chat = opened.chat.unwrap();
        assert!(chat.messages[0].pings("alice"));
        assert!(chat.messages[1].pings("alice"));
        assert!(!chat.messages[2].pings("alice"));
        commands
            .send(Command::ReadVisibleChat("general".into()))
            .await
            .unwrap();
        let result = changes.recv().await.unwrap();
        broadcast_count(&mut other_peer, &result).await;
        wait_for_sidebar(&views, |view| view.unread("general").unread == 0).await;
        wait_for_sidebar(&other_views, |view| view.unread("general").unread == 0).await;

        commands
            .send(Command::MarkChatUnread {
                channel_id: "general".into(),
                message_id: "reply".into(),
            })
            .await
            .unwrap();
        let result = changes.recv().await.unwrap();
        broadcast_count(&mut other_peer, &result).await;
        wait_for_sidebar(&views, |view| view.unread("general").mentions == 1).await;
        wait_for_sidebar(&other_views, |view| view.unread("general").mentions == 1).await;

        commands
            .send(Command::SetUserStatus(UserStatus::Busy))
            .await
            .unwrap();
        let busy = wait_for_sidebar(&views, |view| view.user_status == UserStatus::Busy).await;
        assert_eq!(busy.unread("general").unread, 2);
        assert_eq!(busy.unread("general").mentions, 1);

        // An ordinary focus event cannot immediately undo the explicit mark.
        commands
            .send(Command::ReadVisibleChat("general".into()))
            .await
            .unwrap();
        commands
            .send(Command::MarkChatRead("general".into()))
            .await
            .unwrap();
        let result = changes.recv().await.unwrap();
        assert_eq!(result, value!({"unread":0,"mentions":0}));
        wait_for_sidebar(&views, |view| {
            view.unread("general").unread == 0
                && view
                    .chat
                    .as_ref()
                    .is_some_and(|chat| chat.first_unread_id.is_none())
        })
        .await;

        commands
            .send(Command::JumpToChatMessage {
                channel_id: "general".into(),
                message_id: "older".into(),
            })
            .await
            .unwrap();
        let around = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(|chat| {
                chat.jump
                    .as_ref()
                    .is_some_and(|jump| jump.message_id == "older")
            })
        })
        .await;
        let chat = around.chat.unwrap();
        assert!(chat.history_ready() && chat.has_more_after && chat.first_unread_id.is_none());
        commands
            .send(Command::MarkChatRead(chat.channel_id))
            .await
            .unwrap();
        assert_eq!(
            changes.recv().await.unwrap(),
            value!({"unread":0,"mentions":0})
        );

        // This tab missed the read event while reconnecting. The fresh snapshot clears the ping.
        other_peer.close(None).await.unwrap();
        let _reconnected = accept_async(other_sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        wait_for_sidebar(&other_views, |view| {
            matches!(view.connection, Connection::Online) && view.unread("general").unread == 0
        })
        .await;

        commands.close();
        other.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(other_session.await.unwrap(), SessionEnd::Closed));
        assert!(changes.is_empty());
    })
    .await
    .expect("loopback shared read points timed out");
    http.abort();
}

async fn broadcast_count(peer: &mut WebSocketStream<TcpStream>, state: &Value) {
    peer.send(Message::text(
        value!({
            "type":"unread:update","channelId":"general",
            "unread":state["unread"],"mentions":state["mentions"],
        })
        .to_string(),
    ))
    .await
    .unwrap();
}
