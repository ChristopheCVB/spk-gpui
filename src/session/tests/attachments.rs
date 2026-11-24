use super::*;

use std::io::{Read, Seek, Write};

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
            .put_attachment(
                &upload,
                "text/plain",
                5,
                std::io::Cursor::new(b"hello"),
                |_| {},
            )
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
async fn large_attachments_round_trip_between_files() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let client = SpkClient::new(base.clone(), CookieStore::default()).unwrap();
    let file_size: u64 = 128 * 1024 * 1024 + 11;
    let block_size = 64 * 1024;
    let mut input = tempfile::tempfile().unwrap();
    let mut expected = vec![0; block_size];
    for index in 0..file_size.div_ceil(block_size as u64) {
        expected.fill(index as u8);
        let length = (file_size - index * block_size as u64).min(block_size as u64) as usize;
        input.write_all(&expected[..length]).unwrap();
    }
    input.rewind().unwrap();

    let serve =
        async {
            Request::read(&http, "POST /api/attachments/presign HTTP/1.1")
                .await
                .respond(
                    &[],
                    &value!({
                        "id":"large", "url":base.join("/object").unwrap().as_str(),
                        "headers":{"content-type":"application/octet-stream"}
                    })
                    .to_string(),
                )
                .await;

            let (line, mut upload) = Request::receive_headers(&http).await;
            assert_eq!(line, "PUT /object HTTP/1.1");
            assert_eq!(
                upload
                    .header("content-length")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
                file_size
            );
            let mut actual = vec![0; block_size];
            for index in 0..file_size.div_ceil(block_size as u64) {
                let length =
                    (file_size - index * block_size as u64).min(block_size as u64) as usize;
                upload
                    .stream
                    .read_exact(&mut actual[..length])
                    .await
                    .unwrap();
                assert!(actual[..length].iter().all(|byte| *byte == index as u8));
            }
            upload.respond_raw("200 OK", &[], b"").await;

            Request::read(&http, "GET /api/attachments/large?json=1 HTTP/1.1")
                .await
                .respond(
                    &[],
                    &value!({"url": base.join("/object").unwrap().as_str()}).to_string(),
                )
                .await;
            let mut download = Request::read(&http, "GET /object HTTP/1.1").await;
            download.stream.write_all(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {file_size}\r\nConnection: close\r\n\r\n"
        ).as_bytes()).await.unwrap();
            for index in 0..file_size.div_ceil(block_size as u64) {
                expected.fill(index as u8);
                let length =
                    (file_size - index * block_size as u64).min(block_size as u64) as usize;
                download
                    .stream
                    .write_all(&expected[..length])
                    .await
                    .unwrap();
            }
        };
    let transfer = async {
        let signed = client
            .presign_attachment("large.bin", "application/octet-stream", file_size)
            .await
            .unwrap();
        client
            .put_attachment(
                &signed,
                "application/octet-stream",
                file_size,
                tokio::fs::File::from_std(input),
                |_| {},
            )
            .await
            .unwrap();
        let destination = tempfile::NamedTempFile::new().unwrap();
        client
            .download_attachment("large", file_size, &destination)
            .await
            .unwrap();
        let mut downloaded = destination.reopen().unwrap();
        assert_eq!(downloaded.metadata().unwrap().len(), file_size);
        let mut actual = vec![0; block_size];
        for index in 0..file_size.div_ceil(block_size as u64) {
            let length = (file_size - index * block_size as u64).min(block_size as u64) as usize;
            downloaded.read_exact(&mut actual[..length]).unwrap();
            assert!(actual[..length].iter().all(|byte| *byte == index as u8));
        }
    };
    timeout(Duration::from_secs(60), async {
        tokio::join!(serve, transfer);
    })
    .await
    .expect("file transfer timed out");
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
                filename: name.into(), mime: "text/plain", bytes: bytes::Bytes::from_static(b"hello")
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

        let directory = tempfile::tempdir().unwrap();
        let maximum = directory.path().join("maximum.txt");
        let oversized = directory.path().join("oversized.txt");
        for (path, size) in [(&maximum, 1_073_741_824), (&oversized, 1_073_741_825)] {
            std::fs::File::create(path).unwrap().set_len(size).unwrap();
        }
        tx.send(Command::AddChatAttachments {
            channel_id: "general".into(),
            files: vec![crate::session::AttachmentInput::Path(maximum)],
        }).await.unwrap();
        let maximum_presign = request("POST /api/attachments/presign").await;
        assert_eq!(serde_json::from_slice::<Value>(&maximum_presign.body).unwrap()["size"], 1_073_741_824);
        tx.send(Command::AddChatAttachments {
            channel_id: "general".into(),
            files: vec![crate::session::AttachmentInput::Path(oversized)],
        }).await.unwrap();
        let rejected = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.draft.upload_notice == Some("Files can be 1 GiB at most."))).await;
        let uploads = rejected.chat.unwrap().draft.uploads;
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].filename, "maximum.txt");

        let maximum_key = uploads[0].key;

        let extra = (0..10).map(|index| crate::session::AttachmentInput::Clipboard {
            filename: format!("extra-{index}.txt"),
            mime: "text/plain",
            bytes: bytes::Bytes::from_static(b"hello"),
        }).collect();
        tx.send(Command::AddChatAttachments {
            channel_id: "general".into(), files: extra,
        }).await.unwrap();
        let full = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.draft.upload_notice == Some("At most 10 files per message."))).await;
        let uploads = full.chat.unwrap().draft.uploads;
        assert_eq!(uploads.len(), 10);
        assert_eq!(uploads[0].key, maximum_key);
        assert!(!uploads.iter().any(|upload| upload.filename == "extra-9.txt"));
        let mut presigns = vec![maximum_presign];
        for _ in 0..9 {
            presigns.push(request("POST /api/attachments/presign").await);
        }
        for upload in uploads {
            tx.send(Command::CancelChatAttachment { channel_id: "general".into(), key: upload.key }).await.unwrap();
        }
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.draft.uploads.is_empty())).await;
        for (index, presign) in presigns.into_iter().enumerate() {
            let id = format!("batch-{index}");
            presign.respond(&[], &signed(&id)).await;
            request(&format!("POST /api/attachments/{id}/cancel")).await.respond(&[], "{}").await;
        }

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
        let failed = wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.draft.uploads.iter().any(|upload| matches!(upload.status, crate::session::UploadStatus::Failed(_))))).await;
        let failed_key = failed.chat.unwrap().draft.uploads[0].key;
        tx.send(Command::RetryChatAttachment {
            channel_id: "private".into(), key: failed_key,
        }).await.unwrap();
        let retry_presign = request("POST /api/attachments/presign").await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            !chat.draft.can_send("message") && chat.draft.uploads.iter().any(|upload|
                upload.key == failed_key && matches!(upload.status, crate::session::UploadStatus::Uploading(_))))).await;
        retry_presign.respond(&[], &signed("retry")).await;
        let retry = request("PUT /upload").await;
        assert_eq!(retry.body, b"hello");
        retry.respond_raw("503 Service Unavailable", &[], b"").await;
        request("POST /api/attachments/retry/cancel").await.respond(&[], "{}").await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat|
            chat.draft.uploads.iter().any(|upload| upload.key == failed_key
                && matches!(upload.status, crate::session::UploadStatus::Failed(_))))).await;

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
        let typing = crate::session::tests::receive_non_presence_message(&mut peer).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(typing.to_text().unwrap()).unwrap()["type"], "typing");
        tx.send(Command::SendChatMessage { channel_id: "private".into(), body: body.into() }).await.unwrap();
        let rejected = super::chat::receive_chat_send(&mut peer, "private", body).await;
        assert_eq!(rejected["replyToId"], "target");
        assert_eq!(rejected["attachments"], value!([{"id":"ready","spoiler":true}]));
        peer.send(Message::text(value!({
            "type":"message:error","nonce":rejected["nonce"],"error":"Try again later"
        }).to_string())).await.unwrap();
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| {
            if chat.message_send_error.as_deref() != Some("Try again later") {
                return false;
            }
            assert!(replying(s));
            assert_eq!(chat.draft.body, body);
            assert_eq!(chat.draft.uploads.len(), 2);
            assert!(chat.draft.uploads.iter().any(|upload| upload.key == key && upload.spoiler));
            assert!(chat.outgoing.is_empty());
            true
        })).await;
        tx.send(Command::ResolveChatDraftRestoration {
            channel_id: "private".into(),
            nonce: rejected["nonce"].as_str().unwrap().into(),
            accept: true,
        }).await.unwrap();
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
        assert_eq!(chat.draft.uploads.len(), 1);
        assert_eq!(chat.draft.uploads[0].filename, "fail.txt");
        assert!(chat.outgoing.is_empty());
        assert!(chat.messages[0].attachments[0].spoiler);

        let (reply, saved_file) = oneshot::channel();
        tx.send(Command::DownloadChatAttachment {
            channel_id: "private".into(),
            message_id: "message".into(),
            attachment_id: "ready".into(),
            destination: tempfile::NamedTempFile::new().unwrap(),
            reply,
        }).await.unwrap();
        let access = request("GET /api/attachments/ready?json=1").await;
        assert_eq!(access.header("cookie"), Some("session=alice"));
        access.respond(&[], &value!({"url": base.join("/object").unwrap().as_str()}).to_string()).await;
        request("GET /object").await.respond_raw("200 OK", &[], b"hello").await;
        let file = saved_file.await.unwrap().unwrap();
        assert_eq!(std::fs::read(file.path()).unwrap(), b"hello");
    }).await.expect("attachment queue workflow timed out");

    session.abort();
    server.abort();
}
