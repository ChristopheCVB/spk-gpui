use super::*;

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn channel_changes_reach_another_session_and_close_removed_channels() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let socket_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_url = format!("ws://{}/_ws", socket_listener.local_addr().unwrap());
    let sockets = Arc::new(tokio::sync::Mutex::new(HashMap::<
        String,
        WebSocketStream<TcpStream>,
    >::new()));
    let http = tokio::spawn({
        let sockets = Arc::clone(&sockets);
        async move {
            let mut channels = vec![
                value!({"id":"general","type":"text","name":"General"}),
                value!({"id":"lobby","type":"voice","name":"Lobby"}),
            ];
            let mut reject_reorder = true;
            let mut reject_rename = true;
            let mut reject_alice_channels = false;
            loop {
                let (line, request) = Request::receive(&listener).await;
                let cookie = request.header("cookie");
                assert!(matches!(cookie, Some("session=alice" | "session=bob")));
                match line.as_str() {
                    "GET /api/channels HTTP/1.1" => {
                        if cookie == Some("session=alice")
                            && std::mem::take(&mut reject_alice_channels)
                        {
                            request
                                .respond_raw("503 Service Unavailable", &[], b"")
                                .await;
                        } else {
                            request.respond(&[], &value!(channels).to_string()).await;
                        }
                    }
                    "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => {
                        request.respond(&[], "[]").await
                    }
                    "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => {
                        request.respond(&[], "{}").await
                    }
                    "GET /api/channels/new-text/pins HTTP/1.1" => request.respond(&[], "[]").await,
                    "GET /api/channels/new-text/messages?unread=1 HTTP/1.1" => {
                        request
                            .respond(&[], r#"{"messages":[],"hasMoreBefore":false}"#)
                            .await;
                    }
                    "POST /api/voice/token HTTP/1.1" => {
                        assert_eq!(cookie, Some("session=bob"));
                        assert_eq!(
                            serde_json::from_slice::<Value>(&request.body).unwrap(),
                            value!({"channelId":"new-voice"})
                        );
                        request
                            .respond(
                                &[],
                                &value!({"token":"fixture-token","url":env!("LIVEKIT_URL")})
                                    .to_string(),
                            )
                            .await;
                    }
                    route => {
                        assert_eq!(cookie, Some("session=alice"), "standard User sent {route}");
                        let body = || serde_json::from_slice::<Value>(&request.body).unwrap();
                        let response = match route {
                            "POST /api/admin/channels HTTP/1.1" => {
                                let body = body();
                                let (id, kind, name) = if body["type"] == "text" {
                                    ("new-text", "text", "Project")
                                } else {
                                    ("new-voice", "voice", "Huddle")
                                };
                                assert_eq!(body, value!({"type":kind,"name":name}));
                                let channel = value!({"id":id,"type":kind,"name":name});
                                channels.push(channel.clone());
                                channel
                            }
                            "PATCH /api/admin/channels/new-text HTTP/1.1"
                            | "PATCH /api/admin/channels/new-voice HTTP/1.1" => {
                                let (id, name) = if route.contains("new-text") {
                                    ("new-text", "Renamed text")
                                } else {
                                    ("new-voice", "Renamed voice")
                                };
                                assert_eq!(body(), value!({"name":name}));
                                if std::mem::take(&mut reject_rename) {
                                    request.respond_raw("403 Forbidden", &[], b"").await;
                                    continue;
                                }
                                let channel = channels
                                    .iter_mut()
                                    .find(|channel| channel["id"] == id)
                                    .unwrap();
                                channel["name"] = value!(name);
                                channel.clone()
                            }
                            "POST /api/admin/channels/order HTTP/1.1" => {
                                let body = body();
                                let (kind, new, first) = if body["type"] == "text" {
                                    ("text", "new-text", "general")
                                } else {
                                    ("voice", "new-voice", "lobby")
                                };
                                assert_eq!(body, value!({"type":kind,"ids":[new,first]}));
                                if std::mem::take(&mut reject_reorder) {
                                    request.respond_raw("409 Conflict", &[], b"").await;
                                    continue;
                                }
                                let before = channels
                                    .iter()
                                    .position(|channel| channel["id"] == first)
                                    .unwrap();
                                let after = channels
                                    .iter()
                                    .position(|channel| channel["id"] == new)
                                    .unwrap();
                                channels.swap(before, after);
                                reject_alice_channels = kind == "voice";
                                value!({"ok":true})
                            }
                            "DELETE /api/admin/channels/new-text HTTP/1.1"
                            | "DELETE /api/admin/channels/new-voice HTTP/1.1" => {
                                let id = if route.contains("new-text") {
                                    "new-text"
                                } else {
                                    "new-voice"
                                };
                                channels.retain(|channel| channel["id"] != id);
                                value!({"ok":true})
                            }
                            _ => panic!("unexpected request {route}"),
                        };
                        for (cookie, socket) in sockets.lock().await.iter_mut() {
                            if reject_alice_channels && cookie == "session=alice" {
                                continue;
                            }
                            socket
                                .send(Message::text(r#"{"type":"channels:changed"}"#))
                                .await
                                .unwrap();
                        }
                        request.respond(&[], &response.to_string()).await;
                    }
                }
            }
        }
    });
    let (alice, alice_views, alice_session) = start_peer(&base, &socket_url, "alice", "admin");
    let (bob, bob_views, bob_session) = start_peer(&base, &socket_url, "bob", "user");

    timeout(Duration::from_secs(10), async {
        for _ in 0..2 {
            let mut peer_cookie = None;
            let peer = accept_hdr_async(
                socket_listener.accept().await.unwrap().0,
                |request: &server::Request, response: server::Response| {
                    peer_cookie = Some(request.headers()["cookie"].to_str().unwrap().to_owned());
                    Ok(response)
                },
            )
            .await
            .unwrap();
            sockets.lock().await.insert(peer_cookie.unwrap(), peer);
        }
        for views in [&alice_views, &bob_views] {
            wait_for_sidebar(views, |view| {
                view.channels.is_some() && matches!(view.connection, Connection::Online)
            })
            .await;
        }
        bob.send(Command::ManageChannel(ChannelAction::Create(
            ChannelKind::Text,
            "Forbidden".into(),
        )))
        .await
        .unwrap();
        wait_for_sidebar(&bob_views, |view| {
            view.action_results
                == [ActionResult::error(
                    "You don’t have permission to manage channels.",
                )]
        })
        .await;
        alice
            .send(Command::ManageChannel(ChannelAction::Create(
                ChannelKind::Text,
                "  ".into(),
            )))
            .await
            .unwrap();
        wait_for_sidebar(&alice_views, |view| {
            view.channel_admin.validation == Some("Channel names must be 1–32 characters.")
                && view.action_results.is_empty()
        })
        .await;

        for (kind, id, existing, name, renamed) in [
            (
                ChannelKind::Text,
                "new-text",
                "general",
                "Project",
                "Renamed text",
            ),
            (
                ChannelKind::Voice,
                "new-voice",
                "lobby",
                "Huddle",
                "Renamed voice",
            ),
        ] {
            alice
                .send(Command::ManageChannel(ChannelAction::Create(
                    kind,
                    format!(" {name} "),
                )))
                .await
                .unwrap();
            let created =
                wait_for_channels(&alice_views, kind, &[(existing, None), (id, Some(name))]).await;
            assert_eq!(
                created.action_results,
                [ActionResult::success("Channel created.")]
            );
            wait_for_channels(&bob_views, kind, &[(existing, None), (id, Some(name))]).await;
            if kind == ChannelKind::Text {
                bob.send(Command::OpenTextChannel(id.into())).await.unwrap();
                wait_for_sidebar(&bob_views, |view| {
                    view.chat.as_ref().is_some_and(|chat| chat.latest_ready())
                })
                .await;
            }
            if kind == ChannelKind::Text {
                alice
                    .send(Command::ManageChannel(ChannelAction::Rename(
                        id.into(),
                        renamed.into(),
                    )))
                    .await
                    .unwrap();
                let denied = wait_for_sidebar(&alice_views, |view| {
                    view.action_results
                        == [ActionResult::error(
                            "You don’t have permission to manage channels.",
                        )]
                })
                .await;
                assert_eq!(
                    denied
                        .channels_of_kind(kind)
                        .find(|channel| channel.id == id)
                        .unwrap()
                        .name
                        .as_deref(),
                    Some(name)
                );
            }
            alice
                .send(Command::ManageChannel(ChannelAction::Rename(
                    id.into(),
                    renamed.into(),
                )))
                .await
                .unwrap();
            let renamed_here =
                wait_for_channels(&alice_views, kind, &[(existing, None), (id, Some(renamed))])
                    .await;
            assert_eq!(
                renamed_here.action_results,
                [ActionResult::success("Channel renamed.")]
            );
            let renamed_peer =
                wait_for_channels(&bob_views, kind, &[(existing, None), (id, Some(renamed))]).await;
            if kind == ChannelKind::Text {
                assert_eq!(renamed_peer.chat.unwrap().channel_id, id);
            }
            if kind == ChannelKind::Text {
                alice
                    .send(Command::ManageChannel(ChannelAction::Move {
                        id: id.into(),
                        down: false,
                    }))
                    .await
                    .unwrap();
                wait_for_sidebar(&alice_views, |view| {
                    !view.channel_admin.pending
                        && view.action_results
                            == [ActionResult::error("The channel list changed. Try again.")]
                })
                .await;
            }
            alice
                .send(Command::ManageChannel(ChannelAction::Move {
                    id: id.into(),
                    down: false,
                }))
                .await
                .unwrap();
            if kind == ChannelKind::Voice {
                let warning =
                    ActionResult::warning("Channel updated. Refresh to reload the channel list.");
                let stale = wait_for_sidebar(&alice_views, |view| {
                    view.action_results == [warning.clone()]
                })
                .await;
                assert_eq!(stale.channels_of_kind(kind).next().unwrap().id, existing);
                alice.send(Command::Refresh).await.unwrap();
            }
            let reordered =
                wait_for_channels(&alice_views, kind, &[(id, Some(renamed)), (existing, None)])
                    .await;
            if kind == ChannelKind::Text {
                assert_eq!(
                    reordered.action_results,
                    [ActionResult::success("Channel reordered.")]
                );
            } else {
                assert!(reordered.action_results.is_empty());
            }
            wait_for_channels(&bob_views, kind, &[(id, Some(renamed)), (existing, None)]).await;
            let joined_sid = if kind == ChannelKind::Voice {
                bob.send(Command::JoinVoice(id.into())).await.unwrap();
                let mut sockets = sockets.lock().await;
                let bob_socket = sockets.get_mut("session=bob").unwrap();
                let join = receive_voice_frame(bob_socket).await;
                assert_eq!(join["channelId"], id);
                confirm_voice_join(bob_socket, &join).await;
                wait_for_sidebar(&bob_views, |view| {
                    view.voice
                        .as_ref()
                        .is_some_and(|voice| voice.is_participating())
                })
                .await;
                Some(join["sid"].clone())
            } else {
                None
            };
            alice
                .send(Command::ManageChannel(ChannelAction::Delete(id.into())))
                .await
                .unwrap();
            let deleted_here = wait_for_channels(&alice_views, kind, &[(existing, None)]).await;
            assert_eq!(
                deleted_here.action_results,
                [ActionResult::success("Channel deleted.")]
            );
            let deleted = wait_for_channels(&bob_views, kind, &[(existing, None)]).await;
            if kind == ChannelKind::Text {
                assert!(deleted.chat.is_none());
                assert_eq!(deleted.chat_notice, Some("That channel no longer exists."));
            }
            if let Some(sid) = joined_sid {
                let sidebar = if deleted.voice.is_none() {
                    deleted
                } else {
                    wait_for_sidebar(&bob_views, |view| view.voice.is_none()).await
                };
                assert_eq!(
                    sidebar.voice_notice,
                    Some("This voice channel is no longer available.")
                );
                let mut sockets = sockets.lock().await;
                assert_eq!(
                    receive_voice_frame(sockets.get_mut("session=bob").unwrap()).await,
                    value!({"type":"voice:leave","sid":sid})
                );
            }
        }
    })
    .await
    .expect("channel administration workflow timed out");
    alice.close();
    bob.close();
    assert!(matches!(alice_session.await.unwrap(), SessionEnd::Closed));
    assert!(matches!(bob_session.await.unwrap(), SessionEnd::Closed));
    http.abort();
}

pub(super) fn start_peer(
    base: &Url,
    socket_url: &str,
    id: &str,
    role: &str,
) -> (
    Sender<Command>,
    Receiver<View>,
    tokio::task::JoinHandle<SessionEnd>,
) {
    let mut cookies = CookieStore::default();
    cookies
        .parse(&format!("session={id}; Path=/"), base)
        .unwrap();
    let client = SpkClient::new(base.clone(), cookies).unwrap();
    let profile =
        serde_json::from_value(value!({"id":id,"email":format!("{id}@example.test"),"role":role}))
            .unwrap();
    let (tx, commands) = async_channel::unbounded();
    let (views, rx) = async_channel::unbounded();
    let socket_url = socket_url.to_owned();
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
    (tx, rx, session)
}

async fn wait_for_channels(
    views: &Receiver<View>,
    kind: ChannelKind,
    expected: &[(&str, Option<&str>)],
) -> Box<Sidebar> {
    wait_for_sidebar(views, |view| {
        let channels = view
            .channels
            .iter()
            .flatten()
            .filter(|channel| channel.kind == kind.as_str());
        let actual = channels
            .map(|channel| (channel.id.as_str(), channel.name.as_deref()))
            .collect::<Vec<_>>();
        !view.channel_admin.pending
            && actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|((id, name), (expected_id, expected_name))| {
                    id == expected_id
                        && expected_name.is_none_or(|expected| *name == Some(expected))
                })
    })
    .await
}
