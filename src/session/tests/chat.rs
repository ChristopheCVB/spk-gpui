use super::*;

#[tokio::test]
async fn draft_text_survives_restart_by_conversation_and_account() {
    const GENERAL: &str = "**Hello** <@bob>\n\n> Keep the Markdown";
    const GENERAL_AT_QUIT: &str = "**New Draft** <@bob>";
    const PRIVATE: &str = "_Unsent_ <@alice> 📝";

    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    r#"[
                    {"id":"general","type":"text","name":"general"},
                    {"id":"private","type":"dm","otherUserId":"bob"}
                ]"#
                }
                "GET /api/channels/general/messages?unread=1 HTTP/1.1" => {
                    r#"{"messages":[{"id":"first","channelId":"general","userId":"bob","body":"Existing message","createdAt":1}],"hasMoreBefore":false}"#
                }
                "GET /api/channels/private/messages?unread=1 HTTP/1.1" => {
                    r#"{"messages":[],"hasMoreBefore":false}"#
                }
                "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => "{}",
                "GET /api/users HTTP/1.1"
                | "GET /api/voice HTTP/1.1"
                | "GET /api/channels/general/pins HTTP/1.1"
                | "GET /api/channels/private/pins HTTP/1.1" => "[]",
                _ => panic!("unexpected request: {line}"),
            };
            request.respond(&[], body).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        for (account, first_launch) in [("alice", true), ("charlie", false), ("alice", false)] {
            let client = SpkClient::new(base.clone(), CookieStore::default()).unwrap();
            let (tx, commands) = async_channel::unbounded();
            let (views, rx) = async_channel::unbounded();
            let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut settings = fixture_settings(
                format!("ws://{}/_ws", sockets.local_addr().unwrap()),
                Call::start_for_test,
            );
            settings.drafts_path = Some(directory.path().join("drafts.json"));
            let session = tokio::spawn(async move {
                let (mut saved, _) = SavedSession::with_test_store(None);
                let profile =
                    serde_json::from_value(value!({"id":account,"email":"member@example.test"}))
                        .unwrap();
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
            let mut peer = accept_async(sockets.accept().await.unwrap().0)
                .await
                .unwrap();
            wait_for_sidebar(&rx, |view| {
                view.channels.is_some() && matches!(view.connection, Connection::Online)
            })
            .await;

            for (conversation, open, body) in [
                (
                    "general",
                    Command::OpenTextChannel("general".into()),
                    GENERAL,
                ),
                (
                    "private",
                    Command::OpenDirectMessage("private".into()),
                    PRIVATE,
                ),
            ] {
                tx.send(open).await.unwrap();
                let ready = wait_for_sidebar(&rx, |view| {
                    view.chat
                        .as_ref()
                        .is_some_and(|chat| chat.channel_id == conversation && chat.history_ready())
                })
                .await;
                let draft = &ready.chat.as_ref().unwrap().draft;
                let expected = if !first_launch && account == "alice" {
                    if conversation == "general" {
                        GENERAL_AT_QUIT
                    } else {
                        body
                    }
                } else {
                    ""
                };
                assert_eq!(draft.body, expected);
                assert!(draft.reply.is_none());
                assert!(draft.gif.is_none());
                assert!(draft.uploads.is_empty());
                assert!(ready.chat.unwrap().outgoing.is_none());
                if first_launch {
                    tx.send(Command::UpdateChatDraft {
                        channel_id: conversation.into(),
                        body: "In progress".into(),
                    })
                    .await
                    .unwrap();
                    receive_typing(&mut peer, conversation).await;
                    tx.send(Command::UpdateChatDraft {
                        channel_id: conversation.into(),
                        body: body.into(),
                    })
                    .await
                    .unwrap();
                    if conversation == "general" {
                        tx.send(Command::SetChatReply {
                            channel_id: conversation.into(),
                            message_id: Some("first".into()),
                        })
                        .await
                        .unwrap();
                    }
                } else if account == "charlie" {
                    tx.send(Command::UpdateChatDraft {
                        channel_id: conversation.into(),
                        body: "Another account's Draft".into(),
                    })
                    .await
                    .unwrap();
                    receive_typing(&mut peer, conversation).await;
                }
            }

            if first_launch {
                tx.send(Command::OpenTextChannel("general".into()))
                    .await
                    .unwrap();
                let returned = wait_for_sidebar(&rx, |view| {
                    view.chat
                        .as_ref()
                        .is_some_and(|chat| chat.channel_id == "general" && chat.history_ready())
                })
                .await;
                let draft = returned.chat.unwrap().draft;
                assert_eq!(draft.body, GENERAL);
                assert_eq!(draft.reply.unwrap().id, "first");
                tx.send(Command::SendChatMessage {
                    channel_id: "general".into(),
                    body: GENERAL.into(),
                })
                .await
                .unwrap();
                receive_chat_send(&mut peer, "general", GENERAL).await;
                tx.send(Command::UpdateChatDraft {
                    channel_id: "general".into(),
                    body: GENERAL_AT_QUIT.into(),
                })
                .await
                .unwrap();
            }
            tx.close();
            assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
        }
    })
    .await
    .expect("draft restart timed out");
    server.abort();
}

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn text_channel_exchanges_messages_and_recovers_paginated_history() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (history_tx, history) = async_channel::unbounded();
    let storage = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            assert_eq!(request.header("cookie"), Some("session=chat"));
            match line.as_str() {
                "GET /api/unread HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels HTTP/1.1" => {
                    request.respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"quiet","type":"text","name":"quiet"}]"#).await
                }
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels/general/pins HTTP/1.1" | "GET /api/channels/quiet/pins HTTP/1.1" => request.respond(&[], "[]").await,
                _ => history_tx.send((line, request)).await.unwrap(),
            }
        }
    });

    let mut cookies = CookieStore::default();
    cookies.parse("session=chat; Path=/", &base).unwrap();
    let client = SpkClient::new(base.clone(), cookies).unwrap();
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
        let mut peer = accept_hdr_async(
            sockets.accept().await.unwrap().0,
            |request: &server::Request, response: server::Response| {
                assert_eq!(request.uri().path(), "/_ws");
                assert_eq!(request.headers()["cookie"], "session=chat");
                Ok(response)
            },
        )
        .await
        .unwrap();
        wait_for_sidebar(&rx, |view| {
            view.channels.is_some() && matches!(view.connection, Connection::Online)
        })
        .await;
        tx.send(Command::OpenTextChannel("general".into()))
            .await
            .unwrap();
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.loading())
        })
        .await;
        history_request(&history, "?unread=1")
            .await
            .respond_raw("503 Unavailable", &[], b"")
            .await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.history_failed())
        })
        .await;
        tx.send(Command::RetryChatHistory).await.unwrap();
        let poll = |tea: &[&str], coffee: &[&str]| value!({"options":[
            {"id":"tea","label":"Tea","userIds":tea},
            {"id":"coffee","label":"Coffee","userIds":coffee}
        ]});
        let mut latest = message("m2", 2, "bob", "Hi from web <@alice>");
        latest["poll"] = poll(&["bob"], &[]);
        let initial = poll(&["bob", "eve"], &[]);
        poll_update(&mut peer, "general", "m2", &initial).await;
        peer.send(Message::text(value!({
            "type":"unread:update","channelId":"quiet","unread":1,"mentions":0
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.unread("quiet").unread == 1).await;
        page(
            history_request(&history, "?unread=1").await,
            std::slice::from_ref(&latest),
            true,
        )
        .await;
        wait_for_sidebar(&rx, |view| {
            assert!(view.alerts.is_empty(), "history must never produce a message alert");
            view.chat.as_ref().is_some_and(|chat| {
                chat.messages.iter().any(|message| message.id == "m2")
            })
        }).await;
        tx.send(Command::LoadOlderMessages).await.unwrap();
        let mut older = message("m1", 1, "bob", "Earlier message");
        older["poll"] = Value::Null;
        page(
            history_request(&history, "?before=m2").await,
            std::slice::from_ref(&older),
            false,
        )
        .await;
        wait_for_sidebar(&rx, |view| {
            let chat = view.chat.as_ref().unwrap();
            if !chat.history_ready() || chat.messages.len() != 2 {
                return false;
            }
            assert!(chat.messages[0].poll.is_none());
            let poll = chat.messages[1].poll.as_ref().expect("history Poll");
            assert_eq!(chat.messages[1].body, "Hi from web <@alice>");
            assert_eq!(poll.options[0].id, "tea");
            assert_eq!(poll.options[0].label, "Tea");
            assert_eq!(poll.options[0].user_ids, ["bob", "eve"]);
            assert_eq!(poll.options[1].label, "Coffee");
            assert!(poll.options[1].user_ids.is_empty());
            true
        }).await;

        let vote = |choice_id: Option<&str>| Command::MutateChatMessage {
            channel_id: "general".into(),
            message_id: "m2".into(),
            action: MessageAction::Vote(choice_id.map(str::to_owned)),
        };
        let vote_path = "POST /api/messages/m2/vote HTTP/1.1";
        let result = |tea: &[&str], coffee: &[&str]| value!({
            "channelId":"general", "messageId":"m2", "poll":poll(tea, coffee)
        });
        tx.send(vote(Some("tea"))).await.unwrap();
        let tea = chat_action_request(&history, vote_path, value!({"choiceId":"tea"})).await;
        tx.send(vote(Some("coffee"))).await.unwrap();
        let coffee = timeout(
            Duration::from_secs(2),
            chat_action_request(&history, vote_path, value!({"choiceId":"coffee"})),
        ).await.expect("a second choice can be submitted while the first request is in flight");
        tea.respond(&[], &result(&["bob", "alice"], &[]).to_string()).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| !chat.action_pending
                && chat.messages[1].poll.as_ref().unwrap().options[0].user_ids == ["bob", "alice"])
        }).await;

        let switched = result(&["bob"], &["alice"]);
        coffee.respond(&[], &switched.to_string()).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| !chat.action_pending
                && chat.messages[1].poll.as_ref().unwrap().options[0].user_ids == ["bob"]
                && chat.messages[1].poll.as_ref().unwrap().options[1].user_ids == ["alice"])
        }).await;

        tx.send(vote(None)).await.unwrap();
        let request = chat_action_request(&history, vote_path, value!({"choiceId":null})).await;
        request.respond_raw("400 Bad Request", &[], b"").await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat|
                chat.vote_error == Some("SPK rejected the vote. Try again.")
                    && chat.messages[1].poll.as_ref().unwrap().options[1].user_ids == ["alice"])
        }).await;

        tx.send(vote(None)).await.unwrap();
        let request = chat_action_request(&history, vote_path, value!({"choiceId":null})).await;
        request.respond(&[], &result(&["bob"], &[]).to_string()).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| !chat.action_pending
                && chat.messages[1].poll.as_ref().unwrap().options[1].user_ids.is_empty())
        }).await;

        poll_update(&mut peer, "general", "m2", &poll(&[], &["bob"])).await;
        poll_update(&mut peer, "quiet", "m2", &poll(&["bob"], &[])).await;
        poll_update(&mut peer, "general", "unloaded", &poll(&["bob"], &[])).await;
        peer.send(Message::text(value!({
            "type":"unread:update","channelId":"quiet","unread":2,"mentions":0
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| {
            if view.unread("quiet").unread != 2 {
                return false;
            }
            let chat = view.chat.as_ref().unwrap();
            assert_eq!(chat.messages.len(), 2);
            let poll = chat.messages[1].poll.as_ref().unwrap();
            assert!(poll.options[0].user_ids.is_empty());
            assert_eq!(poll.options[1].user_ids, ["bob"]);
            true
        }).await;

        let options = [" Tea ", "Coffee"];
        let completed = send_poll(&tx, "general", "  Tea or coffee?  ", &options).await;
        assert_eq!(completed.await.unwrap(), Ok(()));
        let sent_poll = receive_chat_send(&mut peer, "general", "Tea or coffee?").await;
        assert_eq!(
            sent_poll,
            value!({"type":"message:send","channelId":"general","body":"Tea or coffee?",
                "poll":[{"label":"Tea"},{"label":"Coffee"}],"nonce":sent_poll["nonce"]})
        );
        peer.send(Message::text(value!({
            "type":"message:error","nonce":sent_poll["nonce"],"error":"Poll rejected"
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.message_send_error.as_deref() == Some("Poll rejected")
                && chat.outgoing.is_none() && chat.messages.len() == 2)
        }).await;

        tx.send(Command::SendChatMessage {
            channel_id: "general".into(),
            body: "Hi from desktop".into(),
        })
        .await
        .unwrap();
        let first = receive_chat_send(&mut peer, "general", "Hi from desktop").await;
        peer.send(Message::text(
            value!({"type":"message:error","nonce":first["nonce"],"error":"Slow down"}).to_string(),
        ))
        .await
        .unwrap();
        wait_for_delivery(
            &rx,
            |delivery| matches!(delivery, Delivery::Failed(error) if error == "Slow down"),
        )
        .await;
        tx.send(Command::RetryChatSend).await.unwrap();
        let retry = receive_chat_send(&mut peer, "general", "Hi from desktop").await;
        assert_ne!(first["nonce"], retry["nonce"]);
        let sent = message("m3", 3, "alice", "Hi from desktop <@alice>");
        live(&mut peer, &sent, retry["nonce"].as_str()).await;
        live(&mut peer, &sent, retry["nonce"].as_str()).await;
        wait_for_sidebar(&rx, |view| {
            assert!(view.alerts.is_empty(), "self-authored frames must stay silent");
            view.chat.as_ref().is_some_and(|chat| {
                chat.messages.iter().map(|message| message.id.as_str()).eq(["m1", "m2", "m3"])
                    && chat.messages[2].poll.is_none()
            })
        }).await;
        tx.send(Command::MutateChatMessage {
            channel_id: "general".into(),
            message_id: "m3".into(),
            action: MessageAction::Edit("Corrected from desktop https://example.test/story".into()),
        }).await.unwrap();
        let (line, edit) = history.recv().await.unwrap();
        assert_eq!(line, "PATCH /api/messages/m3 HTTP/1.1");
        assert_eq!(serde_json::from_slice::<Value>(&edit.body).unwrap(), value!({"body":"Corrected from desktop https://example.test/story"}));
        let mut edited = message("m3", 3, "alice", "Corrected from desktop https://example.test/story");
        edited["editedAt"] = value!(30);
        peer.send(Message::text(value!({"type":"message:edit","message":edited}).to_string())).await.unwrap();
        edit.respond(&[], &edited.to_string()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.messages.iter().any(|m| m.id == "m3" && m.body == "Corrected from desktop https://example.test/story" && m.edited_at == Some(30))
        })).await;

        let url = "https://example.test/story";
        let card = value!({"url":url,"title":"A story","description":"A preview","siteName":"Example","image":true});
        peer.send(Message::text(value!({"type":"message:previews","channelId":"general","messageId":"m3","previews":[card]}).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat.as_ref().unwrap().messages.last().is_some_and(|m| {
            m.previews.first().is_some_and(|preview| preview.title.as_deref() == Some("A story"))
        })).await;
        let (line, preview) = history.recv().await.unwrap();
        assert_eq!(line, "GET /api/messages/m3/preview-image?url=https%3A%2F%2Fexample.test%2Fstory HTTP/1.1");
        let location = format!("Location: http://{}/thumbnail.webp", storage.local_addr().unwrap());
        preview.respond_raw("302 Found", &[&location], b"").await;
        let thumbnail = Request::read(&storage, "GET /thumbnail.webp HTTP/1.1").await;
        assert_eq!(thumbnail.header("cookie"), None);
        const WEBP: &[u8] = b"RIFF\x1e\0\0\0WEBPVP8L\x11\0\0\0/\x01\0\0\0\x07\xd0\x99\x9a\x95\xb9\xff\x81\x88\xe8\x7f\0\0";
        thumbnail.respond_raw("200 OK", &["Content-Type: image/webp"], WEBP).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().unwrap().preview_images.get("m3", url).is_some()).await;
        tx.send(Command::LoadLatestMessages).await.unwrap();
        let stale_history = history_request(&history, "?unread=1").await;
        tx.send(Command::MutateChatMessage {
            channel_id: "general".into(), message_id: "m3".into(), action: MessageAction::RemovePreview(url.into()),
        }).await.unwrap();
        let remove = chat_action_request(&history, "DELETE /api/messages/m3/previews HTTP/1.1", value!({"url":url})).await;
        peer.send(Message::text(value!({"type":"message:previews","channelId":"general","messageId":"m3","previews":[]}).to_string())).await.unwrap();
        remove.respond(&[], r#"{"ok":true}"#).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.loading() && chat.messages.last().is_some_and(|message| message.previews.is_empty())
        })).await;
        let mut stale = edited.clone();
        stale["previews"] = value!([card]);
        page(stale_history, &[older.clone(), latest.clone(), stale], false).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            !chat.loading() && chat.messages.last().is_some_and(|message| message.previews.is_empty())
        })).await;

        tx.send(Command::RetryChatSend).await.unwrap();
        tx.send(Command::SendChatMessage {
            channel_id: "general".into(),
            body: "Closed before confirmation".into(),
        })
        .await
        .unwrap();
        receive_chat_send(&mut peer, "general", "Closed before confirmation").await;
        peer.close(None).await.unwrap();
        wait_for_delivery(&rx, |delivery| matches!(delivery, Delivery::Unconfirmed)).await;
        let completed = send_poll(&tx, "general", "Offline poll?", &["Yes", "No"]).await;
        assert!(completed.await.unwrap().is_err());
        let mut peer = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        let latest_request = history_request(&history, "?unread=1").await;
        let committed_without_ack = message("m4", 4, "alice", "Closed before confirmation");
        let while_loading = message("m5", 5, "bob", "While catching up");
        live(&mut peer, &while_loading, None).await;
        page(
            latest_request,
            &[committed_without_ack, while_loading],
            true,
        )
        .await;
        let overlap_request = history_request(&history, "?before=m4").await;
        poll_update(&mut peer, "general", "m2", &poll(&[], &["alice", "bob"])).await;
        let mut later = message("m6", 6, "bob", "");
        later["replyTo"] = value!({"id":"m3","userId":"alice","preview":"Hi","deleted":false});
        later["attachments"] = value!([{"id":"image","filename":"private.png","mime":"image/png","size":10,"spoiler":true}]);
        live(&mut peer, &later, None).await;
        wait_for_sidebar(&rx, |view| {
            view.alerts.iter().any(|alert| {
                alert.target.channel_id() == "general" && alert.target.message_id() == "m6"
                    && alert.has_attachment && alert.body.is_empty()
            })
        }).await;
        live(&mut peer, &later, None).await;
        peer.send(Message::text(value!({
            "type": "unread:update", "channelId": "general", "unread": 2, "mentions": 1
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| {
            assert!(view.alerts.is_empty(), "a duplicate live frame must stay silent");
            let state = view.unread("general");
            state.unread == 2 && state.mentions == 1
        }).await;
        page(overlap_request, &[older, latest.clone(), sent], false).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            if !chat.latest_ready() || chat.messages.len() != 6 {
                return false;
            }
            assert!(chat.messages.iter().map(|message| message.id.as_str()).eq(["m1", "m2", "m3", "m4", "m5", "m6"]));
            let poll = chat.messages[1].poll.as_ref().unwrap();
            assert!(poll.options[0].user_ids.is_empty());
            assert_eq!(poll.options[1].user_ids, ["alice", "bob"]);
            true
        })).await;

        tx.send(Command::JumpToChatMessage {
            channel_id: "general".into(),
            message_id: "m0".into(),
        }).await.unwrap();
        let request = history_request(&history, "?around=m0").await;
        request.respond(&[], &value!({"messages":[message("m0", 0, "bob", "Original reply target"), message("m1", 1, "bob", "Earlier message")],"hasMoreBefore":false,"hasMoreAfter":true}).to_string()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.jump.as_ref().is_some_and(|jump| jump.message_id == "m0") && chat.has_more_after && chat.messages[0].id == "m0"
        })).await;
        peer.close(None).await.unwrap();
        let mut peer = accept_async(sockets.accept().await.unwrap().0).await.unwrap();
        let request = history_request(&history, "?around=m0").await;
        request.respond(&[], &value!({
            "messages":[message("older", -1, "bob", "Outside the old start"), message("m0", 0, "bob", "Edited reply target"), message("m1", 1, "bob", "Earlier message"), message("m2", 2, "bob", "Outside the window")],
            "hasMoreBefore":false,"hasMoreAfter":true
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.history_ready() && chat.has_more_before && chat.has_more_after && chat.messages.len() == 2
                && chat.messages[0].body == "Edited reply target"
        })).await;
        tx.send(Command::LoadNewerMessages).await.unwrap();
        let request = history_request(&history, "?after=m1").await;
        let mut fresh_poll = latest.clone();
        fresh_poll["poll"] = poll(&["alice"], &["bob"]);
        request.respond(&[], &value!({
            "messages":[fresh_poll],
            "hasMoreBefore":true,"hasMoreAfter":true
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            if !chat.history_ready() || chat.messages.len() != 3 {
                return false;
            }
            assert!(chat.messages.iter().map(|message| message.id.as_str()).eq(["m0", "m1", "m2"]));
            let poll = chat.messages[2].poll.as_ref().unwrap();
            assert_eq!(poll.options[0].user_ids, ["alice"]);
            assert_eq!(poll.options[1].user_ids, ["bob"]);
            true
        })).await;
        tx.send(Command::LoadNewerMessages).await.unwrap();
        let interrupted = history_request(&history, "?after=m2").await;
        peer.close(None).await.unwrap();
        let mut peer = accept_async(sockets.accept().await.unwrap().0).await.unwrap();
        let request = history_request(&history, "?around=m0").await;
        drop(interrupted);
        request.respond(&[], &value!({
            "messages":[message("m0", 0, "bob", "Edited reply target"), message("m1", 1, "bob", "Earlier message"), message("m2", 2, "bob", "Outside the window")],
            "hasMoreBefore":false,"hasMoreAfter":true
        }).to_string()).await;
        let request = history_request(&history, "?after=m2").await;
        live(&mut peer, &message("m7", 7, "bob", "Arrived during pagination"), None).await;
        request.respond(&[], &value!({
            "messages":[message("m6", 6, "bob", "After history was read")],
            "hasMoreBefore":true,"hasMoreAfter":false
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.latest_ready() && !chat.has_more_before && chat.messages.len() == 5
                && chat.messages.last().is_some_and(|message| message.id == "m7")
        })).await;
        tx.send(Command::JumpToChatMessage {
            channel_id: "general".into(),
            message_id: "removed".into(),
        }).await.unwrap();
        history_request(&history, "?around=removed").await.respond_raw("404 Not Found", &[], b"").await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| chat.jump_failed)).await;
        tx.send(Command::OpenMessageTarget(MessageTarget::new("general", "m0").unwrap()))
            .await.unwrap();
        history_request(&history, "?around=m0").await.respond(&[], &value!({
            "messages":[message("m0", 0, "bob", "Edited reply target")],
            "hasMoreBefore":false,"hasMoreAfter":true
        }).to_string()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| chat.history_ready() && chat.has_more_after)).await;
        tx.send(Command::OpenTextChannel("general".into())).await.unwrap();
        page(history_request(&history, "?unread=1").await, &[message("m6", 6, "bob", "After history was read")], true).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.latest_ready() && chat.jump.is_none() && chat.messages.len() == 1 && chat.messages[0].id == "m6"
        })).await;

        tx.send(Command::JumpToChatMessage {
            channel_id: "general".into(),
            message_id: "m6".into(),
        }).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.latest_ready() && chat.jump.as_ref().is_some_and(|jump| jump.message_id == "m6")
        })).await;
        tx.send(Command::OpenTextChannel("general".into())).await.unwrap();
        let mut poll_to_vote = message("m6", 6, "bob", "After history was read");
        poll_to_vote["poll"] = poll(&[], &[]);
        page(history_request(&history, "?unread=1").await, &[poll_to_vote], true).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.latest_ready() && chat.jump.is_none()
        })).await;

        tx.send(Command::RetryChatSend).await.unwrap();
        tx.send(Command::MarkChatSendDelivered("general".into())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| chat.outgoing.is_none() && chat.draft.body.is_empty())).await;
        tx.send(Command::SendChatMessage {
            channel_id: "general".into(),
            body: "Still here".into(),
        })
        .await
        .unwrap();
        receive_chat_send(&mut peer, "general", "Still here").await;

        let completed = send_poll(&tx, "general", "Which day?", &["Monday", "Friday"]).await;
        assert_eq!(completed.await.unwrap(), Ok(()));
        let old_server = receive_chat_send(&mut peer, "general", "Which day?").await;
        live(&mut peer, &message("legacy-poll", 8, "alice", "Which day?"), old_server["nonce"].as_str()).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.messages.iter().any(|message| message.id == "legacy-poll" && message.poll.is_none())
                && chat.outgoing.is_some() && chat.draft.body == "Still here"
        })).await;

        let completed = send_poll(&tx, "general", "Rejected after switching?", &["Yes", "No"]).await;
        assert_eq!(completed.await.unwrap(), Ok(()));
        let rejected = receive_chat_send(&mut peer, "general", "Rejected after switching?").await;
        tx.send(Command::MutateChatMessage {
            channel_id: "general".into(),
            message_id: "m6".into(),
            action: MessageAction::Vote(Some("tea".into())),
        }).await.unwrap();
        let rejected_vote = chat_action_request(
            &history, "POST /api/messages/m6/vote HTTP/1.1", value!({"choiceId":"tea"}),
        ).await;

        tx.send(Command::OpenTextChannel("quiet".into()))
            .await
            .unwrap();
        let (line, request) = history.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/quiet/messages?unread=1 HTTP/1.1");
        page(request, &[], false).await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| {
                chat.channel_id == "quiet"
                    && !chat.loading()
                    && !chat.history_failed()
                    && chat.messages.is_empty()
            })
        })
        .await;

        rejected_vote.respond_raw("429 Too Many Requests", &[], b"").await;
        timeout(Duration::from_secs(2), wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.channel_id == "quiet"
                && chat.vote_error == Some("Too many changes. Try again shortly.")
                && chat.messages.is_empty()
        }))).await.expect("vote rejection should reach the selected conversation");

        peer.send(Message::text(value!({
            "type":"message:error","nonce":rejected["nonce"],"error":"Poll rejected after switching"
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.channel_id == "quiet" && chat.message_send_error.as_deref() == Some("Poll rejected after switching")
        })).await;

        peer.send(Message::text(value!({
            "type":"message:error","nonce":"expired-poll-nonce","error":"Late rejection"
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.channel_id == "quiet" && chat.message_send_error.as_deref() == Some("Late rejection")
        })).await;

        live(&mut peer, &latest, None).await;
        peer.send(Message::text(value!({
            "type": "unread:update", "channelId": "general", "unread": 3, "mentions": 2
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |view| {
            assert!(view.alerts.is_empty(), "the first live replay of a history message must stay silent after switching conversations");
            let state = view.unread("general");
            state.unread == 3 && state.mentions == 2
        }).await;

        tx.send(Command::UpdateChatDraft { channel_id: "quiet".into(), body: "Quiet draft".into() }).await.unwrap();
        receive_typing(&mut peer, "quiet").await;
        tx.send(Command::OpenTextChannel("general".into())).await.unwrap();
        page(history_request(&history, "?unread=1").await, &[], false).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.channel_id == "general" && !chat.loading() && chat.draft.body == "Still here"
        })).await;
        tx.send(Command::OpenTextChannel("quiet".into())).await.unwrap();
        let (line, request) = history.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/quiet/messages?unread=1 HTTP/1.1");
        page(request, &[], false).await;
        wait_for_sidebar(&rx, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.channel_id == "quiet" && !chat.loading() && chat.draft.body == "Quiet draft"
        })).await;

        tx.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("loopback text channel timed out");
    http.abort();
}

fn message(id: &str, created_at: i64, user_id: &str, body: &str) -> Value {
    value!({"id":id,"channelId":"general","userId":user_id,"body":body,"createdAt":created_at})
}

async fn live(peer: &mut WebSocketStream<TcpStream>, message: &Value, nonce: Option<&str>) {
    peer.send(Message::text(
        value!({"type":"message:new","message":message,"nonce":nonce}).to_string(),
    ))
    .await
    .unwrap();
}

async fn poll_update(peer: &mut WebSocketStream<TcpStream>, channel: &str, id: &str, poll: &Value) {
    peer.send(Message::text(
        value!({
            "type":"poll:update","channelId":channel,"messageId":id,"poll":poll
        })
        .to_string(),
    ))
    .await
    .unwrap();
}

async fn history_request(history: &Receiver<(String, Request)>, query: &str) -> Request {
    let (line, request) = history.recv().await.unwrap();
    assert_eq!(
        line,
        format!("GET /api/channels/general/messages{query} HTTP/1.1")
    );
    request
}

async fn page(request: Request, messages: &[Value], more_before: bool) {
    request
        .respond(
            &[],
            &value!({"messages":messages,"hasMoreBefore":more_before,"hasMoreAfter":false})
                .to_string(),
        )
        .await;
}

async fn wait_for_messages(views: &Receiver<View>, expected: &[&str]) {
    wait_for_sidebar(views, |view| {
        view.chat.as_ref().is_some_and(|chat| {
            !chat.loading()
                && chat
                    .messages
                    .iter()
                    .map(|message| message.id.as_str())
                    .eq(expected.iter().copied())
        })
    })
    .await;
}

async fn wait_for_delivery(views: &Receiver<View>, predicate: impl Fn(&Delivery) -> bool) {
    wait_for_sidebar(views, |view| {
        view.chat
            .as_ref()
            .and_then(|chat| chat.outgoing.as_ref())
            .is_some_and(|outgoing| predicate(&outgoing.status))
    })
    .await;
}

async fn receive_chat_send(
    peer: &mut WebSocketStream<TcpStream>,
    conversation: &str,
    body: &str,
) -> Value {
    let frame: Value = serde_json::from_str(
        receive_non_presence_message(peer)
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(frame["type"], "message:send");
    assert_eq!(frame["channelId"], conversation);
    assert_eq!(frame["body"], body);
    assert!(frame["nonce"].is_string());
    frame
}

async fn receive_typing(peer: &mut WebSocketStream<TcpStream>, channel_id: &str) {
    let frame = receive_non_presence_message(peer).await.unwrap().unwrap();
    let typing: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert_eq!(typing, value!({"type":"typing","channelId":channel_id}));
}

#[tokio::test]
async fn direct_message_history_is_visible_only_to_a_participant_even_for_admins() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (history_tx, history) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            let account = request.header("cookie").unwrap();
            match (line.as_str(), account) {
                ("GET /api/channels/private/pins HTTP/1.1", "session=alice") => request.respond(&[], "[]").await,
                ("GET /api/unread HTTP/1.1", _) => request.respond(&[], "{}").await,
                ("GET /api/channels HTTP/1.1", "session=alice") => {
                    request.respond(&[], r#"[{"id":"private","type":"dm","name":null,"otherUserId":"bob","lastActivityAt":2}]"#).await
                }
                ("GET /api/users HTTP/1.1", "session=charlie") => {
                    request.respond(&[], r#"[{"id":"charlie","role":"admin"}]"#).await
                }
                ("GET /api/channels HTTP/1.1", "session=charlie")
                | ("GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1", _) => {
                    request.respond(&[], "[]").await
                }
                ("GET /api/users/presence HTTP/1.1", _) => request.respond(&[], "{}").await,
                _ => history_tx.send((line, request)).await.unwrap(),
            }
        }
    });

    timeout(Duration::from_secs(10), async {
        let (commands, views, sockets, mut peer, alice) =
            direct_message_session(base.clone(), "alice").await;
        commands
            .send(Command::OpenDirectMessage("private".into()))
            .await
            .unwrap();
        let (line, request) = history.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/private/messages?unread=1 HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=alice"));
        let message = value!({"id":"private-2","channelId":"private","userId":"bob","body":"Private content","createdAt":2});
        page(request, std::slice::from_ref(&message), true).await;
        wait_for_messages(&views, &["private-2"]).await;
        commands.send(Command::LoadOlderMessages).await.unwrap();
        let (line, request) = history.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/private/messages?before=private-2 HTTP/1.1");
        let older = value!({"id":"private-1","channelId":"private","userId":"bob","body":"Earlier private content","createdAt":1});
        page(request, &[older], false).await;
        wait_for_messages(&views, &["private-1", "private-2"]).await;

        let (outsider, outsider_views, _, _outsider_peer, charlie) =
            direct_message_session(base.clone(), "charlie").await;
        wait_for_sidebar(&outsider_views, Sidebar::is_admin).await;
        outsider
            .send(Command::OpenDirectMessage("private".into()))
            .await
            .unwrap();
        let denied = wait_for_sidebar(&outsider_views, |view| view.chat_notice.is_some()).await;
        assert!(denied.chat.is_none());
        assert!(denied.channels.unwrap().is_empty());
        assert!(history.is_empty());

        // If an authoritative history request denies access, discard the earlier page and
        // ignore any in-flight socket delivery for the unavailable conversation.
        peer.close(None).await.unwrap();
        let mut peer = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        let (line, request) = history.recv().await.unwrap();
        let pending = wait_for_sidebar(&views, |view| {
            matches!(view.connection, Connection::Online)
                && view.chat.as_ref().is_some_and(ChatView::loading)
        })
        .await;
        assert!(pending.chat.unwrap().messages.is_empty());
        live(&mut peer, &message, None).await;
        assert_eq!(line, "GET /api/channels/private/messages?unread=1 HTTP/1.1");
        request.respond_raw("403 Forbidden", &[], b"").await;
        let denied = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(ChatView::unavailable)
        })
        .await;
        assert!(denied.chat.unwrap().messages.is_empty());
        commands.close();
        outsider.close();
        assert!(matches!(alice.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(charlie.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("loopback private history timed out");
    http.abort();
}

pub(super) async fn direct_message_session(
    base: Url,
    account: &str,
) -> (
    async_channel::Sender<Command>,
    Receiver<View>,
    TcpListener,
    WebSocketStream<TcpStream>,
    tokio::task::JoinHandle<SessionEnd>,
) {
    signed_in_chat_session(base, account, "user").await
}

#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
pub(super) async fn signed_in_chat_session(
    base: Url,
    account: &str,
    role: &str,
) -> (
    async_channel::Sender<Command>,
    Receiver<View>,
    TcpListener,
    WebSocketStream<TcpStream>,
    tokio::task::JoinHandle<SessionEnd>,
) {
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let cookie = format!("session={account}");
    let mut cookies = CookieStore::default();
    cookies.parse(&format!("{cookie}; Path=/"), &base).unwrap();
    let client = SpkClient::new(base, cookies).unwrap();
    let profile = serde_json::from_value(
        value!({"id":account,"email":format!("{account}@example.test"),"role":role}),
    )
    .unwrap();
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
    let peer = accept_hdr_async(
        sockets.accept().await.unwrap().0,
        |request: &server::Request, response: server::Response| {
            assert_eq!(request.uri().path(), "/_ws");
            assert_eq!(request.headers()["cookie"], cookie);
            Ok(response)
        },
    )
    .await
    .unwrap();
    wait_for_sidebar(&rx, |view| {
        view.channels.is_some() && matches!(view.connection, Connection::Online)
    })
    .await;
    (tx, rx, sockets, peer, session)
}

struct DirectMessageHttp {
    base: Url,
    requests: Receiver<(String, Request)>,
    pinned_messages: Arc<std::sync::Mutex<Vec<Value>>>,
    http: tokio::task::JoinHandle<()>,
}

async fn direct_message_http(existing: bool) -> DirectMessageHttp {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (requests_tx, requests) = async_channel::unbounded();
    let pinned_messages = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let served_pins = pinned_messages.clone();
    let http = tokio::spawn(async move {
        let mut created = existing;
        loop {
            let (line, request) = Request::receive(&http).await;
            let account = request.header("cookie").unwrap();
            match line.as_str() {
                "GET /api/unread HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels HTTP/1.1" => {
                    let other = if account == "session=alice" {
                        "bob"
                    } else {
                        "alice"
                    };
                    let list = if created {
                        value!([{"id":"private","type":"dm","name":null,"otherUserId":other,"lastActivityAt":null}])
                    } else {
                        value!([])
                    };
                    request.respond(&[], &list.to_string()).await;
                }
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => {
                    request.respond(&[], "[]").await;
                }
                "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels/private/pins HTTP/1.1" => {
                    let response = value!(&*served_pins.lock().unwrap()).to_string();
                    request.respond(&[], &response).await;
                }
                "POST /api/dms HTTP/1.1" => {
                    assert!(!created, "an existing DM should open without a second POST");
                    assert_eq!(account, "session=alice");
                    assert_eq!(
                        serde_json::from_slice::<Value>(&request.body).unwrap(),
                        value!({"userId":"bob"})
                    );
                    created = true;
                    request.respond(&[], r#"{"id":"private","type":"dm","name":null,"otherUserId":"bob","lastActivityAt":null}"#).await;
                }
                _ => requests_tx.send((line, request)).await.unwrap(),
            }
        }
    });
    DirectMessageHttp {
        base,
        requests,
        pinned_messages,
        http,
    }
}

#[tokio::test]
async fn direct_message_starts_reuses_and_recovers_between_participants() {
    let DirectMessageHttp {
        base,
        requests,
        pinned_messages,
        http,
    } = direct_message_http(false).await;

    timeout(Duration::from_secs(10), async {
        let (alice, alice_views, sockets, mut alice_peer, alice_session) =
            direct_message_session(base.clone(), "alice").await;
        let (bob, bob_views, _, mut bob_peer, bob_session) =
            direct_message_session(base.clone(), "bob").await;
        alice.send(Command::StartDirectMessage("bob".into())).await.unwrap();
        let request = private_history(&requests, "session=alice").await;
        page(request, &[], false).await;
        wait_for_messages(&alice_views, &[]).await;
        alice.send(Command::SendChatMessage { channel_id: "private".into(), body: "Hello Bob".into() }).await.unwrap();
        let sent = receive_chat_send(&mut alice_peer, "private", "Hello Bob").await;
        let first = value!({"id":"private-1","channelId":"private","userId":"alice","body":"Hello Bob","createdAt":1});
        live(&mut alice_peer, &first, sent["nonce"].as_str()).await;
        live(&mut bob_peer, &first, sent["nonce"].as_str()).await;
        wait_for_messages(&alice_views, &["private-1"]).await;
        wait_for_sidebar(&bob_views, |view| view.channels.iter().flatten().any(|channel| channel.id == "private")).await;
        bob.send(Command::OpenDirectMessage("private".into())).await.unwrap();
        page(private_history(&requests, "session=bob").await, std::slice::from_ref(&first), false).await;
        wait_for_messages(&bob_views, &["private-1"]).await;
        timeout(Duration::from_secs(2), wait_for_sidebar(&bob_views, |view| {
            matches!(&view.chat.as_ref().unwrap().pins, Some(Pins::Ready(pins)) if pins.is_empty())
        })).await.expect("opening a DM should load its Pins");

        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-1".into(),
            action: MessageAction::Edit("Changed by another member".into()),
        }).await.unwrap();
        let denied = wait_for_sidebar(&bob_views, |view| {
            view.chat.as_ref().unwrap().action_outcome.as_ref().is_some_and(|outcome| outcome.error.is_some())
        }).await;
        assert_eq!(denied.chat.unwrap().messages[0].body, "Hello Bob");
        assert!(requests.is_empty());

        // A standard member can pin another participant's message in a DM.
        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-1".into(), action: MessageAction::Pin(true),
        }).await.unwrap();
        let pin = chat_action_request(&requests, "POST /api/messages/private-1/pin HTTP/1.1", value!({"on":true})).await;
        assert_eq!(pin.header("cookie"), Some("session=bob"));
        let mut pinned_message = first.clone();
        pinned_message["pinned"] = value!(true);
        pinned_messages.lock().unwrap().push(pinned_message);
        let pinned = value!({"channelId":"private","messageId":"private-1","pinned":true});
        let event = value!({"type":"pin:update","channelId":"private","messageId":"private-1","pinned":true});
        for peer in [&mut alice_peer, &mut bob_peer] {
            peer.send(Message::text(event.to_string())).await.unwrap();
        }
        wait_for_sidebar(&alice_views, |view| view.chat.as_ref().unwrap().messages[0].pinned).await;
        wait_for_sidebar(&bob_views, |view| view.chat.as_ref().unwrap().messages[0].pinned).await;
        pin.respond(&[], &pinned.to_string()).await;
        wait_for_sidebar(&bob_views, |view| {
            let chat = view.chat.as_ref().unwrap();
            !chat.action_pending && matches!(&chat.pins, Some(Pins::Ready(pins)) if pins.iter().any(|m| m.id == "private-1"))
        }).await;

        // An HTTP refusal leaves both participants' current pin state unchanged.
        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-1".into(), action: MessageAction::Pin(false),
        }).await.unwrap();
        let denied = chat_action_request(&requests, "POST /api/messages/private-1/pin HTTP/1.1", value!({"on":false})).await;
        denied.respond_raw("403 Forbidden", &[], b"").await;
        let denied = wait_for_sidebar(&bob_views, |view| view.chat.as_ref().unwrap().action_outcome.as_ref().is_some_and(|outcome| outcome.error.is_some())).await;
        assert!(denied.chat.unwrap().messages[0].pinned);
        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-1".into(), action: MessageAction::Pin(false),
        }).await.unwrap();
        let unpin = chat_action_request(&requests, "POST /api/messages/private-1/pin HTTP/1.1", value!({"on":false})).await;
        pinned_messages.lock().unwrap().clear();
        let update = value!({"type":"pin:update","channelId":"private","messageId":"private-1","pinned":false});
        for peer in [&mut alice_peer, &mut bob_peer] {
            peer.send(Message::text(update.to_string())).await.unwrap();
        }
        unpin.respond(&[], &update.to_string()).await;
        for views in [&alice_views, &bob_views] {
            wait_for_sidebar(views, |view| {
                let chat = view.chat.as_ref().unwrap();
                !chat.action_pending && !chat.messages[0].pinned
            }).await;
        }

        for on in [true, false] {
            bob.send(Command::MutateChatMessage {
                channel_id: "private".into(), message_id: "private-1".into(),
                action: MessageAction::React { emoji: "👍".into(), on },
            }).await.unwrap();
            let reaction = chat_action_request(&requests, "POST /api/messages/private-1/reactions HTTP/1.1", value!({"emoji":"👍","on":on})).await;
            let reactions = if on { value!([{"emoji":"👍","userIds":["bob"]}]) } else { value!([]) };
            let update = value!({"type":"reaction:update","channelId":"private","messageId":"private-1","reactions":reactions});
            for peer in [&mut alice_peer, &mut bob_peer] {
                peer.send(Message::text(update.to_string())).await.unwrap();
            }
            reaction.respond(&[], &update.to_string()).await;
            for views in [&alice_views, &bob_views] {
                wait_for_sidebar(views, |view| {
                    let chat = view.chat.as_ref().unwrap();
                    !chat.action_pending && if on {
                        chat.messages[0].reactions.first().is_some_and(|r| r.emoji == "👍" && r.user_ids == ["bob"])
                    } else {
                        chat.messages[0].reactions.is_empty()
                    }
                }).await;
            }
        }

        let body = "> Hello Bob\n\n**Hi** <@alice> 👋 ||a secret||";
        bob.send(Command::UpdateChatDraft { channel_id: "private".into(), body: body.into() }).await.unwrap();
        receive_typing(&mut bob_peer, "private").await;
        alice_peer.send(Message::text(value!({"type":"typing","channelId":"private","userId":"bob"}).to_string())).await.unwrap();
        wait_for_sidebar(&alice_views, |view| view.chat.as_ref().is_some_and(|chat| chat.typing == ["bob"])).await;
        bob.send(Command::SetChatReply { channel_id: "private".into(), message_id: Some("private-1".into()) }).await.unwrap();
        bob.send(Command::CloseChat).await.unwrap();
        wait_for_sidebar(&bob_views, |view| view.chat.is_none()).await;
        bob.send(Command::OpenDirectMessage("private".into())).await.unwrap();
        page(private_history(&requests, "session=bob").await, std::slice::from_ref(&first), false).await;
        wait_for_sidebar(&bob_views, |view| view.chat.as_ref().is_some_and(|chat| {
            !chat.loading() && chat.draft.body == body && chat.draft.reply.as_ref().is_some_and(|reply| reply.id == "private-1")
        })).await;

        bob.send(Command::SendChatMessage { channel_id: "private".into(), body: body.into() }).await.unwrap();
        let reply = receive_chat_send(&mut bob_peer, "private", body).await;
        assert_eq!(reply["replyToId"], "private-1");
        wait_for_sidebar(&bob_views, |view| view.chat.as_ref().is_some_and(|chat| chat.outgoing.is_some() && chat.draft.body == body)).await;
        let second = value!({"id":"private-2","channelId":"private","userId":"bob","body":body,"createdAt":2,
            "replyTo":{"id":"private-1","userId":"alice","preview":"Hello Bob","deleted":false}});
        live(&mut bob_peer, &second, reply["nonce"].as_str()).await;
        live(&mut alice_peer, &second, reply["nonce"].as_str()).await;
        wait_for_sidebar(&alice_views, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.messages.last().is_some_and(|message| {
                message.body == body && message.reply_to.as_ref().is_some_and(|reply| reply.id == "private-1" && reply.user_id == "alice")
            })
        })).await;
        wait_for_sidebar(&bob_views, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.messages.len() == 2 && chat.draft.body.is_empty() && chat.draft.reply.is_none() && chat.outgoing.is_none()
        })).await;

        alice_peer.close(None).await.unwrap();
        let mut alice_peer = accept_async(sockets.accept().await.unwrap().0).await.unwrap();
        let request = private_history(&requests, "session=alice").await;
        let third = value!({"id":"private-3","channelId":"private","userId":"bob","body":"While offline","createdAt":3});
        live(&mut alice_peer, &third, None).await;
        let latest = [first, second, third];
        page(request, &latest, false).await;
        wait_for_messages(&alice_views, &["private-1", "private-2", "private-3"]).await;

        alice.send(Command::CloseChat).await.unwrap();
        wait_for_sidebar(&alice_views, |view| view.chat.is_none()).await;
        alice.send(Command::StartDirectMessage("bob".into())).await.unwrap();
        page(private_history(&requests, "session=alice").await, &latest, false).await;
        wait_for_messages(&alice_views, &["private-1", "private-2", "private-3"]).await;
        alice.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-1".into(), action: MessageAction::Delete,
        }).await.unwrap();
        let (line, request) = requests.recv().await.unwrap();
        assert_eq!(line, "DELETE /api/messages/private-1 HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=alice"));
        for peer in [&mut alice_peer, &mut bob_peer] {
            peer.send(Message::text(value!({"type":"message:delete","channelId":"private","id":"private-1"}).to_string())).await.unwrap();
        }
        request.respond(&[], r#"{"ok":true}"#).await;
        wait_for_messages(&bob_views, &["private-2"]).await;
        wait_for_sidebar(&alice_views, |view| view.chat.as_ref().is_some_and(|chat| {
            chat.messages.len() == 2 && chat.messages.first().is_some_and(|message| {
                message.id == "private-2" && message.reply_to.as_ref().is_some_and(|reply| reply.deleted && reply.preview.is_empty())
            })
        })).await;

        alice.close();
        bob.close();
        assert!(matches!(alice_session.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(bob_session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("loopback private conversation timed out");
    http.abort();
}

#[tokio::test]
async fn direct_message_participants_vote_edit_create_and_reopen_polls() {
    let DirectMessageHttp {
        base,
        requests,
        pinned_messages,
        http,
    } = direct_message_http(true).await;

    timeout(Duration::from_secs(10), async {
        let (alice, alice_views, _, mut alice_peer, alice_session) =
            direct_message_session(base.clone(), "alice").await;
        let (bob, bob_views, _, mut bob_peer, bob_session) =
            direct_message_session(base, "bob").await;
        let body = "Planning our meetup";
        let previous = value!({"id":"private-2","channelId":"private","userId":"bob","body":body,"createdAt":2});
        let original_poll = value!({
            "id":"private-4","channelId":"private","userId":"bob","body":"When should we meet?","createdAt":4,
            "poll":{"options":[
                {"id":"now","label":"Now","userIds":[]},
                {"id":"later","label":"Later","userIds":[]}
            ]}
        });
        for (commands, views, cookie) in [
            (&alice, &alice_views, "session=alice"),
            (&bob, &bob_views, "session=bob"),
        ] {
            commands.send(Command::OpenDirectMessage("private".into())).await.unwrap();
            page(private_history(&requests, cookie).await, &[previous.clone(), original_poll.clone()], false).await;
            wait_for_messages(views, &["private-2", "private-4"]).await;
        }
        wait_for_sidebar(&bob_views, |view| {
            view.chat.as_ref().unwrap().messages.last()
                .and_then(|message| message.poll.as_ref())
                .is_some_and(|poll| poll.total_votes() == 0)
        }).await;
        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-4".into(),
            action: MessageAction::Vote(Some("now".into())),
        }).await.unwrap();
        let vote = chat_action_request(&requests, "POST /api/messages/private-4/vote HTTP/1.1", value!({"choiceId":"now"})).await;
        assert_eq!(vote.header("cookie"), Some("session=bob"));
        let mut poll = original_poll["poll"].clone();
        poll["options"][0]["userIds"] = value!(["bob"]);
        poll["options"][1]["userIds"] = value!(["alice"]);
        poll_update(&mut alice_peer, "private", "private-4", &poll).await;
        poll_update(&mut bob_peer, "private", "private-4", &poll).await;
        vote.respond(&[], &value!({"channelId":"private","messageId":"private-4","poll":poll}).to_string()).await;
        for views in [&alice_views, &bob_views] {
            wait_for_sidebar(views, |view| {
                let chat = view.chat.as_ref().unwrap();
                !chat.action_pending && chat.messages.last()
                    .and_then(|message| message.poll.as_ref())
                    .is_some_and(|poll| poll.options[0].user_ids == ["bob"])
            }).await;
        }

        // The latest loaded votes require confirmation even if the edit was prepared earlier.
        let options = || vec![
            crate::api::PollEditOption { id: Some("now".into()), label: "  Right now ".into() },
            crate::api::PollEditOption { id: None, label: "Tomorrow".into() },
        ];
        let edit = |reset_votes| {
            let (completed, result) = tokio::sync::oneshot::channel();
            (Command::EditChatPoll {
                channel_id: "private".into(),
                message_id: "private-4".into(),
                question: "  When suits you? ".into(),
                options: options(),
                reset_votes,
                completed,
            }, result)
        };
        let (command, result) = edit(None);
        alice.send(command).await.unwrap();
        assert!(result.await.unwrap().is_err());
        let (command, result) = edit(None);
        bob.send(command).await.unwrap();
        assert_eq!(result.await.unwrap(), Ok(PollEditOutcome::ConfirmVoteChange));
        assert!(requests.is_empty());

        let (command, result) = edit(Some(false));
        bob.send(command).await.unwrap();
        let edited_options = value!([{"id":"now","label":"Right now"},{"label":"Tomorrow"}]);
        let patch = chat_action_request(&requests, "PATCH /api/messages/private-4 HTTP/1.1", value!({
            "body":"When suits you?","poll":edited_options,"resetVotes":false
        })).await;
        assert_eq!(patch.header("cookie"), Some("session=bob"));
        patch.respond_raw("500 Server error", &[], b"").await;
        assert!(result.await.unwrap().is_err());
        let refused = wait_for_sidebar(&bob_views, |view| {
            view.chat.as_ref().unwrap().action_outcome.as_ref().is_some_and(|outcome| outcome.error.is_some())
        }).await;
        assert_eq!(refused.chat.unwrap().messages[1].body, "When should we meet?");

        let (command, result) = edit(Some(false));
        bob.send(command).await.unwrap();
        let patch = chat_action_request(&requests, "PATCH /api/messages/private-4 HTTP/1.1", value!({
            "body":"When suits you?","poll":edited_options,"resetVotes":false
        })).await;
        let mut edited = original_poll.clone();
        edited["body"] = value!("When suits you?");
        edited["editedAt"] = value!(5);
        edited["poll"]["options"] = value!([
            {"id":"now","label":"Right now","userIds":["bob"]},
            {"id":"tomorrow","label":"Tomorrow","userIds":[]}
        ]);
        let event = value!({"type":"message:edit","message":edited});
        alice_peer.send(Message::text(event.to_string())).await.unwrap();
        patch.respond(&[], &edited.to_string()).await;
        assert_eq!(result.await.unwrap(), Ok(PollEditOutcome::Saved));
        for views in [&alice_views, &bob_views] {
            wait_for_sidebar(views, |view| {
                view.chat.as_ref().unwrap().messages.last().is_some_and(|message| {
                    message.body == "When suits you?" && message.edited_at.is_some()
                        && message.poll.as_ref().is_some_and(|poll| {
                            poll.options[0].label == "Right now" && poll.options[0].user_ids == ["bob"]
                                && poll.options[1].id == "tomorrow" && poll.options[1].user_ids.is_empty()
                        })
                })
            }).await;
        }

        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-4".into(),
            action: MessageAction::Edit("When should we go?".into()),
        }).await.unwrap();
        let patch = chat_action_request(&requests, "PATCH /api/messages/private-4 HTTP/1.1",
            value!({"body":"When should we go?"})).await;
        edited["body"] = value!("When should we go?");
        patch.respond(&[], &edited.to_string()).await;
        wait_for_sidebar(&bob_views, |view| {
            let message = view.chat.as_ref().unwrap().messages.last().unwrap();
            message.body == "When should we go?"
                && message.poll.as_ref().is_some_and(|poll| poll.options[0].user_ids == ["bob"])
        }).await;

        let (command, result) = edit(Some(true));
        bob.send(command).await.unwrap();
        let patch = chat_action_request(&requests, "PATCH /api/messages/private-4 HTTP/1.1", value!({
            "body":"When suits you?","poll":edited_options,"resetVotes":true
        })).await;
        edited["poll"]["options"][0]["userIds"] = value!([]);
        patch.respond(&[], &edited.to_string()).await;
        assert_eq!(result.await.unwrap(), Ok(PollEditOutcome::Saved));
        wait_for_sidebar(&bob_views, |view| {
            view.chat.as_ref().unwrap().messages.last()
                .and_then(|message| message.poll.as_ref())
                .is_some_and(|poll| poll.total_votes() == 0)
        }).await;
        bob.send(Command::MutateChatMessage {
            channel_id: "private".into(), message_id: "private-4".into(), action: MessageAction::Delete,
        }).await.unwrap();
        let (line, request) = requests.recv().await.unwrap();
        assert_eq!(line, "DELETE /api/messages/private-4 HTTP/1.1");
        request.respond(&[], r#"{"ok":true}"#).await;
        alice_peer.send(Message::text(value!({"type":"message:delete","channelId":"private","id":"private-4"}).to_string())).await.unwrap();
        wait_for_messages(&alice_views, &["private-2"]).await;
        wait_for_messages(&bob_views, &["private-2"]).await;

        bob.send(Command::UpdateChatDraft { channel_id: "private".into(), body: "Unsent draft".into() }).await.unwrap();
        receive_typing(&mut bob_peer, "private").await;
        bob.send(Command::SetChatReply { channel_id: "private".into(), message_id: Some("private-2".into()) }).await.unwrap();

        let long = "😀".repeat(51);
        let maximum_question = "😀".repeat(150);
        let too_long = format!("{maximum_question}a");
        for (question, options) in [
            ("Question?", ["", "Yes"]),
            ("Question?", [" ΟΣ ", "ος"]),
            ("Question?", ["No", &long]),
            (too_long.as_str(), ["Yes", "No"]),
        ] {
            let completed = send_poll(&bob, "private", question, &options).await;
            assert!(completed.await.unwrap().is_err());
        }

        let maximum_option = "😀".repeat(50);
        let completed = send_poll(&bob, "private", &maximum_question, &[&maximum_option, "No"]).await;
        assert_eq!(completed.await.unwrap(), Ok(()));
        let sent = receive_chat_send(&mut bob_peer, "private", &maximum_question).await;
        assert_eq!(sent["poll"], value!([{"label":maximum_option},{"label":"No"}]));
        assert!(sent.get("replyToId").is_none());
        assert!(sent.get("attachments").is_none());

        let fifth = value!({
            "id":"private-5","channelId":"private","userId":"bob","body":maximum_question,"createdAt":5,
            "poll":{"options":[
                {"id":"emoji","label":maximum_option,"userIds":[]},
                {"id":"no","label":"No","userIds":[]}
            ]}
        });
        live(&mut bob_peer, &fifth, sent["nonce"].as_str()).await;
        live(&mut alice_peer, &fifth, sent["nonce"].as_str()).await;
        for views in [&alice_views, &bob_views] {
            let received = wait_for_sidebar(views, |view| view.chat.as_ref().is_some_and(|chat| {
                chat.messages.last().is_some_and(|message| {
                    message.id == "private-5" && message.poll.as_ref().is_some_and(|poll| {
                        poll.options[0].id == "emoji" && poll.options[0].label == maximum_option
                    })
                })
            })).await;
            if received.profile.id.as_deref() == Some("bob") {
                let chat = received.chat.unwrap();
                assert!(chat.outgoing.is_none());
                assert_eq!(chat.draft.body, "Unsent draft");
                assert_eq!(chat.draft.reply.unwrap().id, "private-2");
            }
        }

        // The server orders Pins by pin time, even when the most recently pinned Message is oldest.
        let older = value!({
            "id":"older-pin","channelId":"private","userId":"bob","body":"Tea or coffee?",
            "createdAt":0,"pinned":true,"pinnedAt":5,
            "poll":{"options":[
                {"id":"tea","label":"Tea","userIds":[]},
                {"id":"coffee","label":"Coffee","userIds":[]}
            ]}
        });
        let recent = value!({"id":"private-2","channelId":"private","userId":"bob","body":body,"createdAt":2,"pinned":true,"pinnedAt":4});
        *pinned_messages.lock().unwrap() = vec![older.clone(), recent];
        bob_peer.send(Message::text(value!({
            "type":"pin:update","channelId":"private","messageId":"older-pin","pinned":true
        }).to_string())).await.unwrap();
        let view = wait_for_sidebar(&bob_views, |view| {
            matches!(&view.chat.as_ref().unwrap().pins, Some(Pins::Ready(pins))
                if pins.iter().map(|pin| pin.id.as_str()).eq(["older-pin", "private-2"]))
        }).await;
        let chat = view.chat.as_ref().unwrap();
        assert!(chat.messages.iter().all(|message| message.id != "older-pin"));
        let Some(Pins::Ready(pins)) = &chat.pins else { unreachable!() };
        assert_eq!(pins[0].body, "Tea or coffee?");
        bob.send(Command::JumpToChatMessage {
            channel_id: chat.channel_id.clone(), message_id: "older-pin".into()
        }).await.unwrap();
        let (line, request) = requests.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/private/messages?around=older-pin HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=bob"));
        page(request, &[older], false).await;
        wait_for_sidebar(&bob_views, |view| {
            let chat = view.chat.as_ref().unwrap();
            chat.jump.as_ref().is_some_and(|jump| jump.message_id == "older-pin")
                && chat.messages.iter().any(|message| {
                    message.id == "older-pin" && message.poll.as_ref().is_some_and(|poll| {
                        poll.options.iter().map(|option| option.id.as_str()).eq(["tea", "coffee"])
                    })
                })
        }).await;
        alice.close();
        bob.close();
        assert!(matches!(alice_session.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(bob_session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("loopback direct message polls timed out");
    http.abort();
}

#[tokio::test]
async fn admin_message_moderation_is_role_gated_and_updates_a_peer() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (requests_tx, requests) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    request
                        .respond(&[], r#"[{"id":"general","type":"text"}]"#)
                        .await;
                }
                "GET /api/users HTTP/1.1" => {
                    request
                        .respond(
                            &[],
                            r#"[{"id":"alice","role":"admin"},{"id":"bob","role":"user"}]"#,
                        )
                        .await;
                }
                "GET /api/voice HTTP/1.1" | "GET /api/channels/general/pins HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => {
                    request.respond(&[], "{}").await;
                }
                _ => requests_tx.send((line, request)).await.unwrap(),
            }
        }
    });

    timeout(Duration::from_secs(10), async {
        let (alice, alice_views, _, mut alice_peer, alice_session) =
            direct_message_session(base.clone(), "alice").await;
        let (bob, bob_views, _, mut bob_peer, bob_session) =
            direct_message_session(base.clone(), "bob").await;
        let url = "https://example.test/moderation";
        let mut target = message("target", 1, "charlie", url);
        target["previews"] = value!([{"url":url,"title":"Moderated card","image":false}]);
        target["poll"] = value!({"options":[
            {"id":"yes","label":"Yes","userIds":["charlie"]},
            {"id":"no","label":"No","userIds":[]}
        ]});
        for (commands, views, cookie) in [
            (&alice, &alice_views, "session=alice"),
            (&bob, &bob_views, "session=bob"),
        ] {
            commands.send(Command::OpenTextChannel("general".into())).await.unwrap();
            let history = history_request(&requests, "?unread=1").await;
            assert_eq!(history.header("cookie"), Some(cookie));
            page(history, std::slice::from_ref(&target), false).await;
            let view = wait_for_sidebar(views, |view| {
                view.is_admin() == (cookie == "session=alice")
                    && view.chat.as_ref().is_some_and(|chat| {
                        chat.messages.first().is_some_and(|message| message.id == "target")
                    })
            }).await;
            let message = &view.chat.as_ref().unwrap().messages[0];
            assert_eq!(view.can_remove_message_content(message), cookie == "session=alice");
            assert_eq!(message.previews.len(), 1);
            assert_eq!(view.chat.as_ref().unwrap().can_pin(view.is_admin()), cookie == "session=alice");
        }

        let mutate = |action| Command::MutateChatMessage {
            channel_id: "general".into(), message_id: "target".into(), action,
        };
        bob.send(mutate(MessageAction::Delete)).await.unwrap();
        let denied = wait_for_sidebar(&bob_views, |view| {
            view.chat.as_ref().unwrap().action_outcome.as_ref().is_some_and(|outcome| outcome.error.is_some())
        }).await;
        assert!(!denied.can_remove_message_content(&denied.chat.as_ref().unwrap().messages[0]));
        assert!(requests.is_empty());

        alice.send(mutate(MessageAction::RemovePreview(url.into()))).await.unwrap();
        let remove = chat_action_request(&requests, "DELETE /api/messages/target/previews HTTP/1.1", value!({"url":url})).await;
        assert_eq!(remove.header("cookie"), Some("session=alice"));
        remove.respond(&[], r#"{"ok":true}"#).await;
        let previews = value!({"type":"message:previews","channelId":"general","messageId":"target","previews":[]});
        for peer in [&mut alice_peer, &mut bob_peer] {
            peer.send(Message::text(previews.to_string())).await.unwrap();
        }
        for views in [&alice_views, &bob_views] {
            wait_for_sidebar(views, |view| {
                let chat = view.chat.as_ref().unwrap();
                !chat.action_pending && chat.messages[0].previews.is_empty()
            }).await;
        }

        alice.send(mutate(MessageAction::Delete)).await.unwrap();
        let (line, delete) = requests.recv().await.unwrap();
        assert_eq!(line, "DELETE /api/messages/target HTTP/1.1");
        assert_eq!(delete.header("cookie"), Some("session=alice"));
        delete.respond(&[], r#"{"ok":true}"#).await;
        let deleted = value!({"type":"message:delete","channelId":"general","id":"target"});
        for peer in [&mut alice_peer, &mut bob_peer] {
            peer.send(Message::text(deleted.to_string())).await.unwrap();
        }
        for views in [&alice_views, &bob_views] {
            wait_for_messages(views, &[]).await;
        }
        alice.close();
        bob.close();
        assert!(matches!(alice_session.await.unwrap(), SessionEnd::Closed));
        assert!(matches!(bob_session.await.unwrap(), SessionEnd::Closed));
    }).await.expect("loopback moderation timed out");
    http.abort();
}

async fn private_history(requests: &Receiver<(String, Request)>, cookie: &str) -> Request {
    let (line, request) = requests.recv().await.unwrap();
    assert_eq!(line, "GET /api/channels/private/messages?unread=1 HTTP/1.1");
    assert_eq!(request.header("cookie"), Some(cookie));
    request
}

async fn chat_action_request(
    requests: &Receiver<(String, Request)>,
    path: &str,
    body: Value,
) -> Request {
    let (line, request) = requests.recv().await.unwrap();
    assert_eq!(line, path);
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        body
    );
    request
}

async fn send_poll(
    commands: &async_channel::Sender<Command>,
    channel: &str,
    question: &str,
    options: &[&str],
) -> tokio::sync::oneshot::Receiver<Result<(), &'static str>> {
    let (completed, result) = tokio::sync::oneshot::channel();
    commands
        .send(Command::SendChatPoll {
            channel_id: channel.to_owned(),
            question: question.to_owned(),
            options: options.iter().map(|option| (*option).to_owned()).collect(),
            completed,
        })
        .await
        .unwrap();
    result
}
