use super::*;

const PREVIEW_GIF: &[u8] =
    b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";

const PREVIEW_PNG: &[u8] = b"\x89PNG\x0d\x0a\x1a\x0a\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde\x00\x00\x00\x0cIDATx\x9cc\xf8\xcf\xc0\x00\x00\x03\x01\x01\x00\xc9\xfe\x92\xef\x00\x00\x00\x00IEND\xaeB`\x82";

#[tokio::test]
async fn gif_picker_searches_saves_and_shares_across_conversations() {
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
                "GET /api/channels HTTP/1.1" => request.respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"private","type":"dm","otherUserId":"bob","name":null}]"#).await,
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => request.respond(&[], "[]").await,
                "GET /api/unread HTTP/1.1" | "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
                "GET /api/channels/general/pins HTTP/1.1" | "GET /api/channels/private/pins HTTP/1.1" => request.respond(&[], "[]").await,
                "GET /api/channels/general/messages?unread=1 HTTP/1.1"
                | "GET /api/channels/private/messages?unread=1 HTTP/1.1" => {
                    request.respond(&[], r#"{"messages":[],"hasMoreBefore":false}"#).await;
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
        let open_chat = async |id: &str, dm| {
            tx.send(if dm { Command::OpenDirectMessage(id.into()) } else { Command::OpenTextChannel(id.into()) }).await.unwrap();
            wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.channel_id == id && chat.latest_ready())).await;
        };
        let request = async |method: &str| {
            let (line, request) = requests.recv().await.unwrap();
            assert_eq!(line, format!("{method} HTTP/1.1"));
            assert_eq!(request.header("cookie"), Some("session=alice"));
            request
        };
        let send = async |command| tx.send(command).await.unwrap();
        let cat_url = "https://media.klipy.com/cat.gif";
        let cat = value!({"id":"cat","title":"Cat","url":cat_url,"preview":base.join("/cat.webp").unwrap().as_str(),"width":240,"height":160});
        let empty = r#"{"items":[],"hasNext":false}"#;

        wait_for_sidebar(&rx, |s| s.channels.is_some() && matches!(s.connection, Connection::Online)).await;
        open_chat("general", false).await;
        send(Command::ShowGifs(true)).await;
        let mut initial = std::collections::HashSet::new();
        for _ in 0..2 {
            let (line, request) = requests.recv().await.unwrap();
            assert_eq!(request.header("cookie"), Some("session=alice"));
            match line.as_str() {
                "GET /api/gifs?q=&page=1 HTTP/1.1" => request.respond_raw("503 Unavailable", &[], b"").await,
                "GET /api/gifs/favorites HTTP/1.1" => request.respond(&[], "[]").await,
                _ => panic!("unexpected request {line}"),
            }
            assert!(initial.insert(line));
        }
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.browse_error == Some("GIFs are not set up") && gifs.favorites_ready)).await;

        send(Command::SearchGifs("no match".into())).await;
        request("GET /api/gifs?q=no+match&page=1").await.respond(&[], empty).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| !gifs.browse_loading && gifs.browse_error.is_none() && gifs.items.is_empty())).await;
        send(Command::SearchGifs(" cat ".into())).await;
        request("GET /api/gifs?q=cat&page=1").await.respond(&[], &value!({"items":[cat.clone()],"hasNext":true}).to_string()).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.items.len() == 1 && gifs.has_next)).await;
        let (line, thumbnail) = requests.recv().await.unwrap();
        assert_eq!(line, "GET /cat.webp HTTP/1.1");
        assert_eq!(thumbnail.header("cookie"), None);
        assert_eq!(thumbnail.header("origin"), None);
        thumbnail.respond_raw("200 OK", &[], PREVIEW_GIF).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.image(cat["preview"].as_str().unwrap()).is_some())).await;
        send(Command::MoreGifs).await;
        request("GET /api/gifs?q=cat&page=2").await.respond_raw("502 Bad Gateway", &[], b"").await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.items.len() == 1 && gifs.browse_error == Some("GIF service unavailable"))).await;

        send(Command::ToggleGifFavorite("cat".into())).await;
        let save = request("PUT /api/gifs/favorites/cat").await;
        assert_eq!(serde_json::from_slice::<Value>(&save.body).unwrap(), cat);
        save.respond(&[], &cat.to_string()).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.is_favorite("cat"))).await;
        send(Command::PickGif("cat".into())).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.gifs.is_none() && chat.draft.gif.is_some())).await;

        open_chat("private", true).await;
        send(Command::ShowGifs(true)).await;
        for _ in 0..2 {
            let (line, request) = requests.recv().await.unwrap();
            assert_eq!(request.header("cookie"), Some("session=alice"));
            match line.as_str() {
                "GET /api/gifs?q=&page=1 HTTP/1.1" => request.respond(&[], empty).await,
                "GET /api/gifs/favorites HTTP/1.1" => request.respond(&[], &value!([cat.clone()]).to_string()).await,
                _ => panic!("unexpected request {line}"),
            }
        }
        send(Command::ShowGifFavorites(true)).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.favorites_ready && gifs.is_favorite("cat"))).await;
        let (_, thumbnail) = requests.recv().await.unwrap();
        thumbnail.respond_raw("404 Not Found", &[], b"").await;
        send(Command::PickGif("cat".into())).await;
        send(Command::ShowGifs(true)).await;
        for _ in 0..2 {
            let (line, request) = requests.recv().await.unwrap();
            match line.as_str() {
                "GET /api/gifs?q=&page=1 HTTP/1.1" => request.respond(&[], empty).await,
                "GET /api/gifs/favorites HTTP/1.1" => request.respond(&[], &value!([cat.clone()]).to_string()).await,
                _ => panic!("unexpected request {line}"),
            }
        }
        send(Command::ShowGifFavorites(true)).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| gifs.favorites_ready && gifs.is_favorite("cat"))).await;
        let (_, thumbnail) = requests.recv().await.unwrap();
        thumbnail.respond_raw("404 Not Found", &[], b"").await;
        send(Command::ToggleGifFavorite("cat".into())).await;
        request("DELETE /api/gifs/favorites/cat").await.respond(&[], r#"{"ok":true}"#).await;
        wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.gifs.as_ref()).is_some_and(|gifs| !gifs.is_favorite("cat"))).await;
        send(Command::ShowGifs(false)).await;

        for (channel_id, body, expected) in [
            ("private", " hi ", format!("hi\n{cat_url}")),
            ("general", "", cat_url.to_owned())
        ] {
            if channel_id == "general" {
                open_chat(channel_id, false).await;
            }
            send(Command::SendChatMessage { channel_id: channel_id.into(), body: body.into() }).await;
            let frame = loop {
                let raw = peer.next().await.unwrap().unwrap();
                let frame: Value = serde_json::from_str(raw.to_text().unwrap()).unwrap();
                if frame["type"] == "message:send" {
                    break frame;
                }
            };
            assert_eq!(frame["channelId"], channel_id);
            assert_eq!(frame["body"], expected);
            peer.send(Message::text(value!({"type":"message:new","nonce":frame["nonce"],"message":{
                "id":"sent","channelId":channel_id,"userId":"alice","body":expected,"createdAt":1
            }}).to_string())).await.unwrap();
            wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.outgoing.is_none()
                && chat.draft.gif.is_none() && chat.messages.iter().any(|m| m.body == expected))).await;
        }

        for (path, preview_bytes) in [("/from-web.gif", PREVIEW_GIF), ("/from-web.png", PREVIEW_PNG)] {
            let incoming_url = base.join(path).unwrap().to_string();
            peer.send(Message::text(value!({"type":"message:new","message":{
                "id":path,"channelId":"general","userId":"bob","body":format!("Shared {incoming_url} inline"),"createdAt":2
            }}).to_string())).await.unwrap();
            wait_for_sidebar(&rx, |s| s.chat.as_ref().is_some_and(|chat| chat.messages.iter().any(|message| message.id == path))).await;
            assert!(requests.is_empty(), "incoming image links must wait for a click");
            let preview = || Command::PreviewMessageImage { message_id: path.into(), url: incoming_url.clone() };
            for (status, bytes) in [("404 Not Found", &b""[..]), ("200 OK", preview_bytes)] {
                send(preview()).await;
                let (line, request) = timeout(Duration::from_secs(2), requests.recv()).await.expect("clicked image must fetch").unwrap();
                assert_eq!(line, format!("GET {path} HTTP/1.1"));
                assert_eq!(request.header("cookie"), None);
                assert_eq!(request.header("origin"), None);
                request.respond_raw(status, &[], bytes).await;
                let view = wait_for_sidebar(&rx, |s| s.chat.as_ref().and_then(|chat| chat.message_image.as_ref()).is_some_and(|gif| !gif.loading && gif.message_id == path)).await;
                assert_eq!(view.chat.unwrap().message_image.unwrap().image.is_some(), status == "200 OK");
            }
        }
    }).await.expect("GIF picker workflow timed out");

    session.abort();
    server.abort();
}
