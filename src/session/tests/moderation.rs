use super::*;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn admin_voice_actions_follow_role_and_shared_presence() {
    let (advance, step) = async_channel::unbounded();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, snapshots| {
            snapshots.send(SidebarSnapshot::VoiceModeration {
                role: "user", channel: Some("first"),
            }).unwrap();
            let mut desktop = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            voice_presence(&mut desktop, Some("first")).await;
            step.recv().await.unwrap();
            assert!(requests.is_empty());

            snapshots.send(SidebarSnapshot::VoiceModeration {
                role: "admin", channel: Some("first"),
            }).unwrap();
            users_changed(&mut desktop).await;
            step.recv().await.unwrap();
            assert!(requests.is_empty());

            let (mut web, mut web_server) = web_peer(&listener).await;
            let rejected = moderation_request(&requests, value!({"userId":"bob","channelId":"second"})).await;
            rejected.respond_raw("403 Forbidden", &[], b"").await;

            let failed = moderation_request(&requests, value!({"userId":"bob","channelId":"second"})).await;
            failed.respond_raw("409 Conflict", &[], b"").await;

            let request = moderation_request(&requests, value!({"userId":"bob","channelId":"second"})).await;
            request.respond(&[], r#"{"ok":true}"#).await;
            // SPK targets the affected client. It must rejoin before peers see the move.
            step.recv().await.unwrap();
            send_voice_move(&mut web_server, &value!("web-sid"), "second").await;
            assert_eq!(
                receive_server_frame(&mut web).await,
                value!({"type":"voice:move","sid":"web-sid","channelId":"second"}),
            );
            let join = value!({"type":"voice:join","sid":"web-sid","channelId":"second"});
            web.send(Message::text(join.to_string())).await.unwrap();
            assert_eq!(receive_voice_frame(&mut web_server).await, join);
            for peer in [&mut desktop, &mut web_server] {
                voice_presence(peer, Some("second")).await;
            }
            assert_eq!(receive_server_frame(&mut web).await, value!({
                "type":"voice:presence","userId":"bob","channelId":"second",
            }));

            let disconnect = moderation_request(&requests, value!({"userId":"bob"})).await;
            for peer in [&mut desktop, &mut web_server] {
                voice_presence(peer, None).await;
            }
            let disconnected = value!({
                "type":"voice:disconnect","sid":"web-sid","reason":"An admin disconnected you from voice",
            });
            web_server.send(Message::text(disconnected.to_string())).await.unwrap();
            assert_eq!(receive_server_frame(&mut web).await, value!({
                "type":"voice:presence","userId":"bob","channelId":null,
            }));
            assert_eq!(receive_server_frame(&mut web).await, disconnected);
            disconnect.respond(&[], r#"{"ok":true}"#).await;
            step.recv().await.unwrap();

            snapshots.send(SidebarSnapshot::VoiceModeration {
                role: "user", channel: Some("second"),
            }).unwrap();
            users_changed(&mut desktop).await;
            voice_presence(&mut desktop, Some("second")).await;
            step.recv().await.unwrap();
            assert!(requests.is_empty());
        },
        async |commands, views| {
            let participants = wait_for_sidebar(&views, |view| {
                matches!(view.connection, Connection::Online)
                    && view.channels.is_some()
                    && view.participants("first").iter().any(|person| person.id == "bob")
            }).await;
            assert!(participants.voice_moderation_channels("bob").is_none());
            let move_bob = |destination: &str| Command::ModerateVoice {
                user_id: "bob".into(),
                action: VoiceModeration::Move(destination.into()),
            };
            commands.send(move_bob("second")).await.unwrap();
            assert_voice_moderation_notice(&views, "You don’t have permission to moderate voice.").await;
            advance.send(()).await.unwrap();

            let admin = wait_for_sidebar(&views, |view| {
                view.voice_moderation_channels("bob").is_some_and(|destinations| {
                    destinations.len() == 1 && destinations[0].id == "second"
                })
            }).await;
            assert!(admin.voice.is_none());
            assert!(admin.voice_moderation_channels("missing").is_none());
            commands.send(move_bob("first")).await.unwrap();
            assert_voice_moderation_notice(&views, "This voice moderation action is unavailable.").await;
            advance.send(()).await.unwrap();

            for notice in [
                "You don’t have permission to moderate voice.",
                "That participant is no longer in the expected voice channel.",
                "Move requested.",
            ] {
                commands.send(move_bob("second")).await.unwrap();
                let current = assert_voice_moderation_notice(&views, notice).await;
                assert_eq!(current.participants("first")[0].id, "bob");
                assert!(current.participants("second").is_empty());
            }
            advance.send(()).await.unwrap();
            let moved = wait_for_sidebar(&views, |view| {
                view.participants("second").iter().any(|person| person.id == "bob")
            }).await;
            assert!(moved.participants("first").is_empty());
            commands.send(Command::ModerateVoice {
                user_id: "bob".into(),
                action: VoiceModeration::Disconnect,
            }).await.unwrap();
            let left = wait_for_sidebar(&views, |view| {
                view.voice_moderation_notice == Some("Voice participant disconnected.")
                    && view.participants("second").is_empty()
            }).await;
            assert!(left.voice_moderation_channels("bob").is_none());
            advance.send(()).await.unwrap();

            let user = wait_for_sidebar(&views, |view| {
                !view.is_admin() && view.participants("second").iter().any(|person| person.id == "bob")
            }).await;
            assert!(user.voice_moderation_channels("bob").is_none());
            commands.send(move_bob("first")).await.unwrap();
            assert_voice_moderation_notice(&views, "You don’t have permission to moderate voice.").await;
            advance.send(()).await.unwrap();
        },
    ).await;
    assert!(matches!(end, SessionEnd::Closed));
}

async fn assert_voice_moderation_notice(views: &Receiver<View>, notice: &str) -> Box<Sidebar> {
    wait_for_sidebar(views, |view| view.voice_moderation_notice == Some(notice)).await
}

async fn moderation_request(requests: &Receiver<Request>, body: Value) -> Request {
    let request = requests.recv().await.unwrap();
    assert_eq!(request.header("cookie"), Some("session=fixture"));
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        body
    );
    request
}

#[expect(
    clippy::result_large_err,
    reason = "The WebSocket handshake fixes the callback signature"
)]
async fn web_peer(
    listener: &TcpListener,
) -> (
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    WebSocketStream<TcpStream>,
) {
    let mut request = format!("ws://{}/_ws", listener.local_addr().unwrap())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("cookie", "session=web".parse().unwrap());
    let (web, socket) = tokio::join!(tokio_tungstenite::connect_async(request), async {
        accept_hdr_async(
            listener.accept().await.unwrap().0,
            |request: &server::Request, response: server::Response| {
                assert_eq!(request.headers()["cookie"], "session=web");
                Ok(response)
            },
        )
        .await
        .unwrap()
    },);
    (web.unwrap().0, socket)
}

async fn receive_server_frame(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
) -> Value {
    let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
    serde_json::from_str(&frame).unwrap()
}

async fn voice_presence(socket: &mut WebSocketStream<TcpStream>, channel: Option<&str>) {
    socket
        .send(Message::text(
            value!({
                "type":"voice:presence","userId":"bob","channelId":channel,
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

async fn users_changed(socket: &mut WebSocketStream<TcpStream>) {
    socket
        .send(Message::text(r#"{"type":"users:changed"}"#))
        .await
        .unwrap();
}
