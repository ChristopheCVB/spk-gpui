use super::*;

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
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            assert_eq!(request.header("cookie"), Some("session=chat"));
            match line.as_str() {
                "GET /api/channels HTTP/1.1" => {
                    request.respond(&[], r#"[{"id":"general","type":"text","name":"general"},{"id":"quiet","type":"text","name":"quiet"}]"#).await
                }
                "GET /api/voice HTTP/1.1" | "GET /api/users HTTP/1.1" => {
                    request.respond(&[], "[]").await
                }
                "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
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
        history_request(&history, "")
            .await
            .respond_raw("503 Unavailable", &[], b"")
            .await;
        wait_for_sidebar(&rx, |view| {
            view.chat.as_ref().is_some_and(|chat| chat.history_failed())
        })
        .await;
        tx.send(Command::RetryChatHistory).await.unwrap();
        let latest = message("m2", 2, "bob", "Hi from web");
        page(
            history_request(&history, "").await,
            std::slice::from_ref(&latest),
            true,
        )
        .await;
        wait_for_messages(&rx, &["m2"]).await;
        tx.send(Command::LoadOlderMessages).await.unwrap();
        let older = message("m1", 1, "bob", "Earlier message");
        page(
            history_request(&history, "?before=m2").await,
            std::slice::from_ref(&older),
            false,
        )
        .await;
        wait_for_messages(&rx, &["m1", "m2"]).await;

        tx.send(Command::SendChatMessage {
            channel_id: "general".into(),
            body: "Hi from desktop".into(),
        })
        .await
        .unwrap();
        let first = receive_send(&mut peer, "Hi from desktop").await;
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
        let retry = receive_send(&mut peer, "Hi from desktop").await;
        assert_ne!(first["nonce"], retry["nonce"]);
        let sent = message("m3", 3, "alice", "Hi from desktop");
        live(&mut peer, &sent, retry["nonce"].as_str()).await;
        live(&mut peer, &sent, retry["nonce"].as_str()).await;
        wait_for_messages(&rx, &["m1", "m2", "m3"]).await;
        tx.send(Command::RetryChatSend).await.unwrap();
        tx.send(Command::SendChatMessage {
            channel_id: "general".into(),
            body: "Closed before confirmation".into(),
        })
        .await
        .unwrap();
        receive_send(&mut peer, "Closed before confirmation").await;
        peer.close(None).await.unwrap();
        wait_for_delivery(&rx, |delivery| matches!(delivery, Delivery::Unconfirmed)).await;
        let mut peer = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        let latest_request = history_request(&history, "").await;
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
        let later = message("m6", 6, "bob", "After history was read");
        live(&mut peer, &later, None).await;
        page(overlap_request, &[older, latest, sent], false).await;
        wait_for_messages(&rx, &["m1", "m2", "m3", "m4", "m5", "m6"]).await;

        tx.send(Command::RetryChatSend).await.unwrap();
        tx.send(Command::DismissChatSend).await.unwrap();
        tx.send(Command::SendChatMessage {
            channel_id: "general".into(),
            body: "Still here".into(),
        })
        .await
        .unwrap();
        receive_send(&mut peer, "Still here").await;

        tx.send(Command::OpenTextChannel("quiet".into()))
            .await
            .unwrap();
        let (line, request) = history.recv().await.unwrap();
        assert_eq!(line, "GET /api/channels/quiet/messages HTTP/1.1");
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

async fn receive_send(peer: &mut WebSocketStream<TcpStream>, body: &str) -> Value {
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
    assert_eq!(frame["channelId"], "general");
    assert_eq!(frame["body"], body);
    assert!(frame["nonce"].is_string());
    frame
}
