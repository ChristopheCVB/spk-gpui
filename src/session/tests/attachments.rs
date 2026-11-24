use super::*;

#[tokio::test]
async fn attachment_transfers_use_spk_authorization_without_disclosing_session_cookies() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let client = |id: &str| {
        let mut cookies = CookieStore::default();
        cookies
            .parse(&format!("session={id}; Path=/"), &base)
            .unwrap();
        SpkClient::new(base.clone(), cookies).unwrap()
    };
    let alice = client("alice");
    let bob = client("bob");
    let outsider = client("outsider");
    let serve = async {
        let (line, request) = Request::receive(&http).await;
        assert_eq!(line, "POST /api/attachments/presign HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=alice"));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            value!({"filename":"notes.txt","mime":"text/plain","size":5})
        );
        request
            .respond(
                &[],
                &value!({
                    "id":"file-id", "url":base.join("/object").unwrap().as_str(),
                    "headers":{"content-type":"text/plain"}
                })
                .to_string(),
            )
            .await;

        let (line, request) = Request::receive(&http).await;
        assert_eq!(line, "PUT /object HTTP/1.1");
        assert_eq!(request.header("cookie"), None);
        assert_eq!(request.header("origin"), None);
        assert_eq!(request.header("content-length"), Some("5"));
        assert_eq!(request.header("content-type"), Some("text/plain"));
        assert_eq!(request.body, b"hello");
        request.respond_raw("200 OK", &[], b"").await;

        let (line, request) = Request::receive(&http).await;
        assert_eq!(line, "POST /api/attachments/file-id/cancel HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=alice"));
        request.respond(&[], "{}").await;

        let (line, request) = Request::receive(&http).await;
        assert_eq!(line, "GET /api/attachments/shared?json=1 HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=bob"));
        request
            .respond(
                &[],
                &value!({"url":base.join("/private").unwrap().as_str()}).to_string(),
            )
            .await;

        let (line, request) = Request::receive(&http).await;
        assert_eq!(line, "GET /private HTTP/1.1");
        assert_eq!(request.header("cookie"), None);
        assert_eq!(request.header("origin"), None);
        request.respond_raw("200 OK", &[], b"hello").await;

        let (line, request) = Request::receive(&http).await;
        assert_eq!(line, "GET /api/attachments/shared?json=1 HTTP/1.1");
        assert_eq!(request.header("cookie"), Some("session=outsider"));
        request.respond_raw("404 Not Found", &[], b"").await;
    };
    let workflow = async {
        let upload = alice
            .presign_attachment("notes.txt", "text/plain", 5)
            .await
            .unwrap();
        assert_eq!(upload.id, "file-id");
        alice
            .put_attachment(&upload, "text/plain", b"hello".to_vec(), |_| {})
            .await
            .unwrap();
        alice.cancel_attachment(&upload.id).await.unwrap();
        assert_eq!(bob.attachment_bytes("shared").await.unwrap(), b"hello");
        assert!(matches!(
            outsider.attachment_bytes("shared").await,
            Err(Failure::Http(reqwest::StatusCode::NOT_FOUND))
        ));
    };
    timeout(Duration::from_secs(10), async {
        tokio::join!(serve, workflow);
    })
    .await
    .expect("attachment workflow timed out");
}

#[tokio::test]
async fn attachment_queue_cancels_in_flight_uploads_and_sends_only_ready_files() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (requests_tx, requests) = async_channel::unbounded();
    let server = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    request
                        .respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"private","type":"dm","otherUserId":"bob","name":null}]"#)
                        .await
                }
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => {
                    request.respond(&[], "{}").await
                }
                "GET /api/channels/general/pins HTTP/1.1" | "GET /api/channels/private/pins HTTP/1.1" => request.respond(&[], "[]").await,
                "GET /api/channels/general/messages?unread=1 HTTP/1.1" => {
                    request
                        .respond(&[], r#"{"messages":[],"hasMoreBefore":false}"#)
                        .await;
                }
                _ => requests_tx.send((line, request)).await.unwrap(),
            }
        }
    });
    let mut cookies = CookieStore::default();
    cookies.parse("session=alice; Path=/", &base).unwrap();
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
        let mut peer = accept_async(sockets.accept().await.unwrap().0).await.unwrap();
        wait_for_sidebar(&rx, |s| s.channels.is_some() && matches!(s.connection, Connection::Online)).await;
        tx.send(Command::OpenTextChannel("general".into())).await.unwrap();
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.latest_ready())).await;

        let queue = |channel: &str, name: &str| Command::AddChatAttachments {
            channel_id: channel.into(),
            files: vec![crate::session::AttachmentInput::Clipboard {
                filename: name.into(), mime: "text/plain", bytes: b"hello".to_vec()
            }],
        };
        let request = async |method: &str| {
            let (line, request) = requests.recv().await.unwrap();
            assert_eq!(line, format!("{method} HTTP/1.1"));
            request
        };
        let signed = |id: &str| value!({
            "id":id, "url":base.join("/upload").unwrap().as_str(),
            "headers":{"content-type":"text/plain"}
        }).to_string();

        tx.send(queue("general", "reserving.txt")).await.unwrap();
        let presign = request("POST /api/attachments/presign").await;
        let queued = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.draft.uploads.iter().any(|upload| upload.filename == "reserving.txt"))).await;
        let key = queued.chat.unwrap().draft.uploads[0].key;
        tx.send(Command::CancelChatAttachment { channel_id: "general".into(), key }).await.unwrap();
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.draft.uploads.is_empty())).await;
        presign.respond(&[], &signed("cancel-presign")).await;
        request("POST /api/attachments/cancel-presign/cancel").await.respond(&[], "{}").await;

        tx.send(queue("general", "cancel.txt")).await.unwrap();
        request("POST /api/attachments/presign").await.respond(&[], &signed("cancel")).await;
        let in_flight = request("PUT /upload").await;
        let view = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.draft.uploads.iter().any(|upload| matches!(upload.status, crate::session::UploadStatus::Uploading(99))))).await;
        let key = view.chat.unwrap().draft.uploads[0].key;
        tx.send(Command::CancelChatAttachment { channel_id: "general".into(), key }).await.unwrap();
        request("POST /api/attachments/cancel/cancel").await.respond(&[], "{}").await;
        drop(in_flight);

        tx.send(Command::OpenDirectMessage("private".into())).await.unwrap();
        let target = value!({
            "id":"target", "channelId":"private", "userId":"bob",
            "body":format!(" \nHello Alice ||{}|| are you there?\nSecond line", "secret".repeat(25)), "createdAt":1
        });
        request("GET /api/channels/private/messages?unread=1").await
            .respond(&[], &value!({"messages":[target],"hasMoreBefore":false}).to_string()).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.channel_id == "private" && chat.latest_ready())).await;

        tx.send(queue("private", "fail.txt")).await.unwrap();
        request("POST /api/attachments/presign").await.respond(&[], &signed("fail")).await;
        request("PUT /upload").await.respond_raw("401 Unauthorized", &[], b"").await;
        request("POST /api/attachments/fail/cancel").await.respond(&[], "{}").await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.draft.uploads.iter().any(|upload| matches!(upload.status, crate::session::UploadStatus::Failed(_))))).await;

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("ready.txt");
        std::fs::write(&file, b"hello").unwrap();
        tx.send(Command::AddChatAttachments {
            channel_id: "private".into(),
            files: vec![crate::session::AttachmentInput::Path(file)],
        }).await.unwrap();
        request("POST /api/attachments/presign").await.respond(&[], &signed("ready")).await;
        request("PUT /upload").await.respond(&[], "").await;
        let ready = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.draft.attachments_ready())).await;
        let key = ready.chat.unwrap().draft.uploads.into_iter()
            .find(|upload| matches!(upload.status, crate::session::UploadStatus::Ready(_))).unwrap().key;
        tx.send(Command::ToggleChatAttachmentSpoiler { channel_id: "private".into(), key }).await.unwrap();

        let body = "I’ll send a file instead.";
        tx.send(Command::UpdateChatDraft { channel_id: "private".into(), body: body.into() }).await.unwrap();
        let set_reply = |message_id: Option<&str>| Command::SetChatReply {
            channel_id: "private".into(), message_id: message_id.map(str::to_owned),
        };
        let replying = |s: &Sidebar| s.chat.as_ref().is_some_and(|chat| {
            chat.draft.reply.as_ref().is_some_and(|reply| {
                assert_eq!(reply.id, "target");
                assert_eq!(
                    crate::chat_markdown::reply_preview(&reply.preview, |id| s.message_author(id)),
                    "Hello Alice ▒▒▒ are you there?",
                );
                true
            })
        });
        let kept_draft = |s: &Sidebar| s.chat.as_ref().is_some_and(|chat| {
            chat.draft.reply.is_none() && chat.draft.body == body && chat.draft.attachments_ready()
        });
        tx.send(set_reply(Some("target"))).await.unwrap();
        wait_for_sidebar(&rx, replying).await;
        tx.send(set_reply(None)).await.unwrap();
        wait_for_sidebar(&rx, kept_draft).await;

        tx.send(set_reply(Some("target"))).await.unwrap();
        wait_for_sidebar(&rx, replying).await;
        peer.send(Message::text(value!({
            "type":"message:delete","channelId":"private","id":"target"
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, kept_draft).await;

        tx.send(Command::SendChatMessage { channel_id: "private".into(), body: String::new() }).await.unwrap();
        let frame = loop {
            let raw = peer.next().await.unwrap().unwrap();
            let frame: Value = serde_json::from_str(raw.to_text().unwrap()).unwrap();
            if frame["type"] == "message:send" {
                break frame;
            }
        };
        assert_eq!(frame["channelId"], "private");
        assert_eq!(frame["body"], "");
        assert!(frame["replyToId"].is_null());
        assert_eq!(frame["attachments"], value!([{"id":"ready","spoiler":true}]));
        peer.close(None).await.unwrap();
        let _peer = accept_async(sockets.accept().await.unwrap().0).await.unwrap();
        request("GET /api/channels/private/messages?unread=1").await.respond(&[], &value!({
            "hasMoreBefore":false, "messages": [{
                "id":"message","channelId":"private","userId":"alice","createdAt":1,"body":"",
                "attachments":[{"id":"ready","filename":"ready.txt","mime":"text/plain","size":5,"spoiler":true}]
            }]
        }).to_string()).await;
        let received = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.messages.iter().any(|message| message.id == "message"))).await;
        let chat = received.chat.unwrap();
        assert!(chat.draft.uploads.is_empty());
        assert!(chat.outgoing.is_none());
        assert!(chat.messages[0].attachments[0].spoiler);

        let (reply, bytes) = oneshot::channel();
        tx.send(Command::ReadChatAttachment {
            channel_id: "private".into(), message_id: "message".into(), attachment_id: "ready".into(), reply,
        }).await.unwrap();
        let access = request("GET /api/attachments/ready?json=1").await;
        assert_eq!(access.header("cookie"), Some("session=alice"));
        access.respond(&[], &value!({"url": base.join("/object").unwrap().as_str()}).to_string()).await;
        request("GET /object").await.respond_raw("200 OK", &[], b"hello").await;
        assert_eq!(bytes.await.unwrap().unwrap(), b"hello");
    }).await.expect("attachment queue workflow timed out");

    session.abort();
    server.abort();
}
