mod media;

use std::{collections::HashSet, time::Duration};

use cookie_store::{CookieStore, serde::json};
use futures::{SinkExt, StreamExt};
use reqwest::cookie::CookieStore as _;
use serde_json::{Value, json as value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, accept_hdr_async,
    tungstenite::{Message, handshake::server},
};

use super::{presence::Data, voice_session::VOICE_REPLACED, *};
use crate::{
    api::{Member, VideoPresence, VoiceFlags, VoiceSocket},
    voice::{self, DevicePreferences, Report as VoiceReport},
};

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn restored_http_session_authenticates_websocket_and_sign_out() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let base = Url::parse(&origin).unwrap();
    let socket_url = format!("ws://{address}/_ws");
    let expected_origin = origin.clone();

    let server = async move {
        let request = Request::read(&listener, "POST /api/auth/sign-in/email HTTP/1.1").await;
        assert_eq!(request.header("origin"), Some(expected_origin.as_str()));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            value!({"email":"alice@example.test","password":"test password","rememberMe":true}),
        );
        request
            .respond(
                &[
                    "session=initial; Path=/; HttpOnly; Max-Age=3600",
                    "transient=temporary; Path=/",
                ],
                "{}",
            )
            .await;

        let request = Request::read(&listener, "GET /api/profile/me HTTP/1.1").await;
        assert!(
            request
                .header("cookie")
                .unwrap()
                .contains("session=initial")
        );
        request
            .respond(
                &["session=refreshed; Path=/; HttpOnly; Max-Age=3600"],
                r#"{"email":"alice@example.test","displayName":"Alice"}"#,
            )
            .await;

        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(
            tcp,
            |request: &server::Request, response: server::Response| {
                assert_eq!(request.uri().path(), "/_ws");
                assert_eq!(request.headers()["origin"], expected_origin);
                assert_eq!(request.headers()["cookie"], "session=refreshed");
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket
            .send(Message::text(r#"{"type":"ping"}"#))
            .await
            .unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap(), Message::text("ping"));
        socket
            .send(Message::text(
                r#"{"type":"voice:presence","userId":"alice","channelId":null}"#,
            ))
            .await
            .unwrap();

        let request = Request::read(&listener, "POST /api/auth/sign-out HTTP/1.1").await;
        assert_eq!(request.header("origin"), Some(expected_origin.as_str()));
        assert_eq!(request.header("cookie"), Some("session=refreshed"));
        request
            .respond(&["session=; Path=/; HttpOnly; Max-Age=0"], "{}")
            .await;
    };

    let desktop = async {
        let (client, profile) = sign_in(
            &base,
            "alice@example.test".to_owned(),
            "test password".to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(profile.display_name.as_deref(), Some("Alice"));
        let mut bytes = Vec::new();
        json::save(&client.cookies().lock().unwrap(), &mut bytes).unwrap();
        let restored = SpkClient::new(base.clone(), json::load(bytes.as_slice()).unwrap()).unwrap();

        let (events, updates) = async_channel::bounded(4);
        let mut sockets = JoinSet::new();
        let socket = restored.clone();
        sockets.spawn(async move { socket.socket(&socket_url, events).await });
        assert!(matches!(
            updates.recv().await.unwrap(),
            SocketEvent::Online(_)
        ));
        assert!(matches!(
            updates.recv().await.unwrap(),
            SocketEvent::Frame(Frame::Presence {
                channel_id: None,
                ..
            })
        ));
        restored.sign_out().await.unwrap();
        assert!(restored.cookies().cookies(&base).is_none());
    };

    timeout(Duration::from_secs(10), async {
        tokio::join!(server, desktop)
    })
    .await
    .expect("loopback SPK protocol timed out");
}

#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "Tungstenite defines the handshake callback type"
)]
async fn saved_session_restores_signs_out_and_clears_expired_credentials() {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let mut cookies = CookieStore::default();
    cookies
        .parse("session=saved; Path=/; HttpOnly; Max-Age=3600", &base)
        .unwrap();
    let mut bytes = Vec::new();
    json::save(&cookies, &mut bytes).unwrap();

    for expired in [false, true] {
        let (tx, commands) = async_channel::unbounded();
        let (views, rx) = async_channel::unbounded();
        let (saved, storage) = SavedSession::with_test_store(bytes.clone());
        let settings = fixture_settings(endpoint.clone(), Call::start_for_test);
        let server = async {
            let request = Request::read(&http, "GET /api/profile/me HTTP/1.1").await;
            assert_eq!(request.header("cookie"), Some("session=saved"));
            if expired {
                request.respond_raw("401 Unauthorized", &[], b"").await;
                let sign_in = Request::read(&http, "POST /api/auth/sign-in/email HTTP/1.1").await;
                assert_eq!(sign_in.header("cookie"), None);
                assert_eq!(
                    serde_json::from_slice::<Value>(&sign_in.body).unwrap(),
                    value!({"email":"alice@example.test","password":"test password","rememberMe":true}),
                );
                sign_in
                    .respond(&["session=new; Path=/; HttpOnly; Max-Age=3600"], "{}")
                    .await;
                let profile = Request::read(&http, "GET /api/profile/me HTTP/1.1").await;
                assert_eq!(profile.header("cookie"), Some("session=new"));
                profile
                    .respond(&[], r#"{"id":"alice","email":"alice@example.test"}"#)
                    .await;
                return;
            }
            request
                .respond(&[], r#"{"id":"alice","email":"alice@example.test"}"#)
                .await;
            let mut ws = accept_hdr_async(
                sockets.accept().await.unwrap().0,
                |request: &server::Request, response: server::Response| {
                    assert_eq!(request.headers()["cookie"], "session=saved");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            loop {
                let (line, request) = Request::receive(&http).await;
                assert_eq!(request.header("cookie"), Some("session=saved"));
                match line.as_str() {
                    "GET /api/channels HTTP/1.1"
                    | "GET /api/voice HTTP/1.1"
                    | "GET /api/users HTTP/1.1" => request.respond(&[], "[]").await,
                    "POST /api/auth/sign-out HTTP/1.1" => {
                        request
                            .respond(&["session=; Path=/; HttpOnly; Max-Age=0"], "{}")
                            .await;
                        let _ = ws.close(None).await;
                        break;
                    }
                    _ => panic!("unexpected request: {line}"),
                }
            }
        };
        let desktop = async {
            if !expired {
                let sidebar = wait_for_sidebar(&rx, |sidebar| {
                    matches!(sidebar.connection, Connection::Online)
                })
                .await;
                assert_eq!(sidebar.profile.id.as_deref(), Some("alice"));
                assert_eq!(*storage.lock().unwrap(), Some(bytes.clone()));
                tx.send(Command::SignOut).await.unwrap();
                loop {
                    if matches!(rx.recv().await.unwrap(), View::Busy("Signing out…")) {
                        break;
                    }
                }
            }
            match rx.recv().await.unwrap() {
                View::SignIn {
                    message,
                    retry_restore: false,
                } => {
                    assert_eq!(message, expired.then_some(SESSION_EXPIRED));
                }
                _ => panic!("expected the sign-in form"),
            }
            assert!(storage.lock().unwrap().is_none());
            if expired {
                tx.send(Command::SignIn {
                    email: "alice@example.test".into(),
                    password: "test password".into(),
                })
                .await
                .unwrap();
                let sidebar = wait_for_sidebar(&rx, |_| true).await;
                assert_eq!(sidebar.profile.id.as_deref(), Some("alice"));
                let persisted = storage.lock().unwrap();
                let cookies = json::load(persisted.as_deref().unwrap()).unwrap();
                assert!(
                    cookies
                        .matches(&base)
                        .iter()
                        .any(|cookie| { cookie.name() == "session" && cookie.value() == "new" })
                );
            }
            drop(tx);
        };
        timeout(Duration::from_secs(7), async {
            tokio::join!(
                server,
                desktop,
                run_session(base.clone(), saved, settings, commands, views)
            )
        })
        .await
        .expect("stored session workflow timed out");
    }
}

#[derive(Clone, Copy)]
enum SidebarSnapshot {
    Initial,
    Updated,
    VoiceReplaced,
    FailPresenceOnce,
    MediaPresence {
        channel: &'static str,
        flags: VoiceFlags,
    },
}

async fn voice_session(
    start_call: impl Fn(String, SpkClient, VoiceSocket, Intent) -> Call + Send + Sync + 'static,
    server: impl AsyncFnOnce(TcpListener, Receiver<Request>, watch::Sender<SidebarSnapshot>),
    desktop: impl AsyncFnOnce(Sender<Command>, Receiver<View>),
) -> SessionEnd {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_endpoint = format!("ws://{}/_ws", sockets.local_addr().unwrap());
    let (command_tx, commands) = async_channel::unbounded();
    let (views, view_rx) = async_channel::unbounded();
    let (token_requests, requests) = async_channel::unbounded();
    let (update_snapshots, updated) = watch::channel(SidebarSnapshot::Initial);

    let http = async move {
        let mut presence_failed = false;
        loop {
            let (line, request) = Request::receive(&http).await;
            let snapshot = *updated.borrow();
            let body = match line.as_str() {
                "POST /api/auth/sign-in/email HTTP/1.1" => {
                    request
                        .respond(&["session=fixture; Path=/; HttpOnly; Max-Age=3600"], "{}")
                        .await;
                    continue;
                }
                "GET /api/profile/me HTTP/1.1" => r#"{"id":"alice","email":"alice@example.test"}"#,
                "GET /api/channels HTTP/1.1" if matches!(snapshot, SidebarSnapshot::Updated) => {
                    r#"[{"id":"first","type":"voice"},{"id":"new","type":"voice"}]"#
                }
                "GET /api/voice HTTP/1.1" => match snapshot {
                    SidebarSnapshot::FailPresenceOnce if !presence_failed => {
                        presence_failed = true;
                        request
                            .respond_raw("503 Service Unavailable", &[], b"")
                            .await;
                        continue;
                    }
                    SidebarSnapshot::Initial | SidebarSnapshot::FailPresenceOnce => "[]",
                    SidebarSnapshot::Updated => {
                        r#"[{"userId":"bob","channelId":"first","speaking":true}]"#
                    }
                    SidebarSnapshot::VoiceReplaced => {
                        r#"[{"userId":"alice","channelId":"second"}]"#
                    }
                    SidebarSnapshot::MediaPresence { channel, flags } => {
                        let body = value!([{
                            "userId":"alice","channelId":channel,
                            "muted":flags.muted,"deafened":flags.deafened,"speaking":flags.speaking,
                        }]);
                        request.respond(&[], &body.to_string()).await;
                        continue;
                    }
                },
                "GET /api/channels HTTP/1.1" => {
                    r#"[{"id":"first","type":"voice"},{"id":"second","type":"voice"}]"#
                }
                "GET /api/users HTTP/1.1" => "[]",
                "POST /api/voice/token HTTP/1.1" => {
                    token_requests.send(request).await.unwrap();
                    continue;
                }
                _ => panic!("unexpected request: {line}"),
            };
            request.respond(&[], body).await;
        }
    };

    let session = async move {
        let (client, profile) = sign_in(&base, "alice@example.test".into(), "fixture".into())
            .await
            .unwrap();
        let mut persisted = Vec::new();
        json::save(&client.cookies().lock().unwrap(), &mut persisted).unwrap();
        let (mut saved, _) = SavedSession::with_test_store(persisted);
        let mut settings = fixture_settings(socket_endpoint, start_call);
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
    };

    timeout(Duration::from_secs(7), async {
        tokio::select! {
            () = http => unreachable!(),
            ((), (), end) = async { tokio::join!(server(sockets, requests, update_snapshots), desktop(command_tx, view_rx), session) } => end,
        }
    }).await.expect("loopback voice session timed out")
}

fn fixture_settings(
    socket_endpoint: String,
    start_call: impl Fn(String, SpkClient, VoiceSocket, Intent) -> Call + Send + Sync + 'static,
) -> VoiceSettings {
    let mut sounds = SoundSettings::default();
    for sound in Sound::ALL {
        sounds.get_mut(sound).enabled = false;
    }
    VoiceSettings {
        socket_endpoint,
        start_call: Box::new(start_call),
        enumerate_devices: Devices::default,
        devices: DeviceSettings {
            path: None,
            preferred: DevicePreferences::default(),
            notice: None,
        },
        sounds: VoiceSounds::new(tokio::sync::watch::channel(sounds).1),
    }
}

async fn wait_for_sidebar(
    views: &Receiver<View>,
    matches: impl Fn(&Sidebar) -> bool,
) -> Box<Sidebar> {
    loop {
        let View::SignedIn(sidebar) = views.recv().await.unwrap() else {
            panic!("expected the signed-in sidebar");
        };
        if matches(&sidebar) {
            return sidebar;
        }
    }
}

async fn accept_voice_join(
    requests: &Receiver<Request>,
    socket: &mut WebSocketStream<TcpStream>,
    channel: &str,
) -> Value {
    request_voice_token(requests, channel)
        .await
        .respond(
            &[],
            &value!({"token":"fixture-token","url":env!("LIVEKIT_URL")}).to_string(),
        )
        .await;
    let join = receive_voice_frame(socket).await;
    assert_eq!(
        join,
        value!({
            "type":"voice:join","sid":join["sid"],"channelId":channel,"nonce":join["nonce"],
            "muted":true,"deafened":false,"watching":[]
        }),
    );
    join
}

async fn request_voice_token(requests: &Receiver<Request>, channel: &str) -> Request {
    let request = requests.recv().await.unwrap();
    assert_eq!(request.header("cookie"), Some("session=fixture"));
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        value!({"channelId":channel})
    );
    request
}

async fn receive_voice_frame(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let frame = socket.next().await.unwrap().unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

async fn acknowledge_voice(socket: &mut WebSocketStream<TcpStream>, join: &Value) {
    socket.send(Message::text(value!({
        "type":"voice:joined","sid":join["sid"],"channelId":join["channelId"],"nonce":join["nonce"]
    }).to_string())).await.unwrap();
}

#[tokio::test]
async fn spk_recovery_reclaims_the_same_voice_session_with_current_flags() {
    let (offline_changes_applied, resume_socket) = oneshot::channel();
    let (published, mut observed) = tokio::sync::mpsc::channel(2);

    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, update_snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let join = establish_voice(&requests, &mut socket).await;
            let sid = &join["sid"];
            drop(socket);

            update_snapshots.send_replace(SidebarSnapshot::Updated);
            let (tcp, _) = listener.accept().await.unwrap();
            resume_socket.await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({
                    "type":"voice:join","sid":sid,"channelId":"first","ready":true,
                    "muted":true,"deafened":true,"watching":[]
                }),
            );
            socket
                .send(Message::text(
                    value!({
                        "type":"voice:joined","sid":sid,"channelId":"first"
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                voice_state(sid, true, true)
            );
            published.send(()).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                voice_state(sid, false, false)
            );
            published.send(()).await.unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:leave","sid":sid})
            );
            assert!(matches!(
                requests.try_recv(),
                Err(async_channel::TryRecvError::Empty)
            ));
        },
        async |commands, views| {
            join_test_voice(&commands, &views).await;
            wait_for_sidebar(&views, |sidebar| {
                matches!(sidebar.connection, Connection::Reconnecting)
                    && shows_alice_speaking(sidebar)
            })
            .await;
            commands.send(Command::ToggleDeafen).await.unwrap();
            wait_for_sidebar(&views, |sidebar| {
                matches!(sidebar.connection, Connection::Reconnecting)
                    && sidebar.voice_intent.mute.deafened
                    && sidebar.voice.as_ref().is_some_and(|voice| {
                        voice.status == VoiceStatus::Connected
                            && voice
                                .media
                                .as_ref()
                                .is_some_and(|media| media.speaking.is_empty())
                    })
            })
            .await;
            offline_changes_applied.send(()).unwrap();
            wait_for_sidebar(&views, |sidebar| {
                matches!(sidebar.connection, Connection::Online)
                    && sidebar
                        .channels
                        .iter()
                        .flatten()
                        .any(|channel| channel.id == "new")
                    && sidebar
                        .participants("first")
                        .iter()
                        .any(|member| member.id == "bob" && member.flags.speaking)
            })
            .await;
            observed.recv().await.unwrap();
            commands.send(Command::ToggleDeafen).await.unwrap();
            wait_for_sidebar(&views, shows_alice_speaking).await;
            observed.recv().await.unwrap();
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn spk_recovery_yields_to_another_client_in_a_different_voice_channel() {
    let (finished, closed) = oneshot::channel();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, update_snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            establish_voice(&requests, &mut socket).await;
            update_snapshots.send_replace(SidebarSnapshot::VoiceReplaced);
            drop(socket);

            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            tokio::select! {
                biased;
                Some(Ok(Message::Text(frame))) = socket.next() => {
                    panic!("the previous client sent a voice frame: {frame}")
                }
                result = closed => result.unwrap(),
            }
        },
        async |commands, views| {
            join_test_voice(&commands, &views).await;
            let sidebar = wait_for_sidebar(&views, |sidebar| {
                sidebar.voice.is_none() && sidebar.voice_notice == Some(VOICE_REPLACED)
            })
            .await;
            assert!(
                sidebar
                    .participants("second")
                    .iter()
                    .any(|member| member.id == "alice")
            );
            finished.send(()).unwrap();
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn spk_recovery_retries_presence_and_waits_for_livekit_before_announcing_ready() {
    let (recovering, reconnect_socket) = oneshot::channel();
    let (reclaimed, restore_media) = oneshot::channel();
    let (resynced, leave) = oneshot::channel();
    let end = voice_session(
        voice::tests::start_recovery_fixture,
        async |listener, requests, update_snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let join = accept_voice_join(&requests, &mut socket, "first").await;
            acknowledge_voice(&mut socket, &join).await;
            let sid = &join["sid"];
            let ready = value!({"type":"voice:ready","sid":sid});
            assert_eq!(receive_voice_frame(&mut socket).await, ready);
            reconnect_socket.await.unwrap();
            update_snapshots.send_replace(SidebarSnapshot::FailPresenceOnce);
            drop(socket);

            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({
                    "type":"voice:join","sid":sid,"channelId":"first","ready":false,
                    "muted":true,"deafened":false,"watching":[]
                })
            );
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                voice_state(sid, true, false)
            );
            reclaimed.send(()).unwrap();
            assert_eq!(receive_voice_frame(&mut socket).await, ready);
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                voice_state(sid, true, false)
            );
            resynced.send(()).unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:leave","sid":sid})
            );
        },
        async |commands, views| {
            wait_for_sidebar(&views, |sidebar| sidebar.can_join_voice("first")).await;
            commands
                .send(Command::JoinVoice("first".into()))
                .await
                .unwrap();
            let status = |expected| {
                move |sidebar: &Sidebar| {
                    sidebar
                        .voice
                        .as_ref()
                        .is_some_and(|voice| voice.status == expected)
                }
            };
            wait_for_sidebar(&views, status(VoiceStatus::ListenOnly)).await;
            commands.send(Command::RefreshDevices).await.unwrap();
            wait_for_sidebar(&views, status(VoiceStatus::Reconnecting)).await;
            recovering.send(()).unwrap();
            wait_for_sidebar(&views, |sidebar| {
                matches!(sidebar.connection, Connection::Online) && sidebar.load_failed
            })
            .await;
            restore_media.await.unwrap();

            commands.send(Command::RefreshDevices).await.unwrap();
            wait_for_sidebar(&views, status(VoiceStatus::ListenOnly)).await;
            leave.await.unwrap();
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[derive(Clone, Copy)]
enum VoiceEnding {
    LeaveOffline,
    LeaveWindow,
    SignOut,
    Replaced,
    Disconnected,
    Unauthorized,
}

#[tokio::test]
async fn spk_recovery_yields_to_voice_teardown_and_invalidation() {
    for ending in [
        VoiceEnding::LeaveOffline,
        VoiceEnding::LeaveWindow,
        VoiceEnding::SignOut,
        VoiceEnding::Replaced,
        VoiceEnding::Disconnected,
        VoiceEnding::Unauthorized,
    ] {
        let (established, voice_started) = oneshot::channel();
        let (left_offline, reconnect_after_leaving) = oneshot::channel();
        let end = voice_session(
            Call::start_for_test,
            async |listener, requests, _| {
                let mut socket = accept_async(listener.accept().await.unwrap().0)
                    .await
                    .unwrap();
                let join = establish_voice(&requests, &mut socket).await;
                established.send(()).unwrap();
                let sid = &join["sid"];
                match ending {
                    VoiceEnding::LeaveOffline => {
                        drop(socket);
                        reconnect_after_leaving.await.unwrap();
                        let mut socket = accept_async(listener.accept().await.unwrap().0)
                            .await
                            .unwrap();
                        acknowledge_voice(&mut socket, &join).await;
                        socket
                            .send(Message::text(r#"{"type":"ping"}"#))
                            .await
                            .unwrap();
                        assert_eq!(socket.next().await.unwrap().unwrap(), Message::text("ping"));
                        socket
                            .send(Message::text(
                                r#"{"type":"voice:presence","userId":"bob","channelId":"first"}"#,
                            ))
                            .await
                            .unwrap();
                    }
                    VoiceEnding::Unauthorized => {
                        drop(socket);
                        Request::read(&listener, "GET /_ws HTTP/1.1")
                            .await
                            .respond_raw("401 Unauthorized", &[], b"")
                            .await;
                    }
                    ending => {
                        let frame = match ending {
                            VoiceEnding::Replaced => {
                                Some(value!({"type":"voice:kicked","sid":"other-device"}))
                            }
                            VoiceEnding::Disconnected => {
                                Some(value!({"type":"voice:disconnect","sid":sid}))
                            }
                            _ => None,
                        };
                        if let Some(frame) = frame {
                            socket.send(Message::text(frame.to_string())).await.unwrap();
                        }
                        assert_eq!(
                            receive_voice_frame(&mut socket).await,
                            value!({"type":"voice:leave","sid":sid})
                        );
                    }
                }
            },
            async |commands, views| {
                join_test_voice(&commands, &views).await;
                voice_started.await.unwrap();
                if matches!(ending, VoiceEnding::LeaveWindow) {
                    return;
                }
                if matches!(ending, VoiceEnding::SignOut) {
                    commands.send(Command::SignOut).await.unwrap();
                    while views.recv().await.is_ok() {}
                    return;
                }
                if matches!(ending, VoiceEnding::LeaveOffline) {
                    wait_for_sidebar(&views, |sidebar| {
                        matches!(sidebar.connection, Connection::Reconnecting)
                    })
                    .await;
                    commands.send(Command::LeaveVoice).await.unwrap();
                    wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
                    left_offline.send(()).unwrap();
                    let sidebar = wait_for_sidebar(&views, |sidebar| {
                        !sidebar.participants("first").is_empty()
                    })
                    .await;
                    assert!(sidebar.voice.is_none());
                } else if matches!(ending, VoiceEnding::Unauthorized) {
                    while views.recv().await.is_ok() {}
                } else {
                    let sidebar = wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
                    assert!(sidebar.voice_notice.is_some());
                }
            },
        )
        .await;
        match ending {
            VoiceEnding::SignOut => assert!(matches!(end, SessionEnd::SignOut)),
            VoiceEnding::Unauthorized => {
                assert!(matches!(end, SessionEnd::Invalidated(SESSION_EXPIRED)))
            }
            _ => assert!(matches!(end, SessionEnd::Closed)),
        }
    }
}

async fn establish_voice(
    requests: &Receiver<Request>,
    socket: &mut WebSocketStream<TcpStream>,
) -> Value {
    let join = accept_voice_join(requests, socket, "first").await;
    confirm_voice_join(socket, &join).await;
    join
}

async fn confirm_voice_join(socket: &mut WebSocketStream<TcpStream>, join: &Value) {
    acknowledge_voice(socket, join).await;
    let sid = &join["sid"];
    assert_eq!(
        receive_voice_frame(socket).await,
        value!({"type":"voice:ready","sid":sid})
    );
    assert_eq!(
        receive_voice_frame(socket).await,
        voice_state(sid, false, false)
    );
}

async fn join_test_voice(commands: &Sender<Command>, views: &Receiver<View>) {
    request_first_voice(commands, views).await;
    wait_for_sidebar(views, shows_alice_speaking).await;
}

async fn request_first_voice(commands: &Sender<Command>, views: &Receiver<View>) {
    wait_for_sidebar(views, |sidebar| {
        matches!(sidebar.connection, Connection::Online) && sidebar.channels.is_some()
    })
    .await;
    commands
        .send(Command::JoinVoice("first".into()))
        .await
        .unwrap();
}

fn shows_alice_speaking(sidebar: &Sidebar) -> bool {
    sidebar.voice.as_ref().is_some_and(|voice| {
        voice.status == VoiceStatus::Connected
            && voice
                .media
                .as_ref()
                .is_some_and(|media| media.speaking.contains("alice"))
    })
}

fn voice_state(sid: &Value, muted: bool, deafened: bool) -> Value {
    value!({"type":"voice:state","sid":sid,"muted":muted,"deafened":deafened,"speaking":!muted,"cam":false,"screen":false,"watching":[]})
}

#[test]
fn voice_reclaim_acknowledgment_accepts_an_omitted_nonce() {
    assert!(matches!(
        serde_json::from_str::<Frame>(
            r#"{"type":"voice:joined","sid":"session","channelId":"voice"}"#
        ),
        Ok(Frame::VoiceJoined { .. })
    ));
}

#[test]
fn live_membership_and_flags_win_over_an_in_flight_snapshot() {
    let mut sidebar = Sidebar::new(
        Profile {
            id: Some("reader".into()),
            email: "reader@example.test".into(),
            display_name: None,
            avatar_key: None,
        },
        None,
    );
    let mut refresh = Refresh::default();
    refresh.presence_updates = Some(HashMap::new());
    refresh.complete(
        &mut sidebar,
        Resource::Channels,
        Ok(Data::Channels(
            [("second", "voice"), ("text", "text"), ("first", "voice")]
                .into_iter()
                .map(|(id, kind)| Channel {
                    id: id.into(),
                    kind: kind.into(),
                    name: None,
                })
                .collect(),
        )),
    );
    let listed = sidebar
        .channels
        .as_ref()
        .unwrap()
        .iter()
        .map(|channel| channel.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(listed, ["second", "text", "first"]);
    refresh.complete(
        &mut sidebar,
        Resource::Users,
        Ok(Data::Users(vec![User {
            id: "alice".into(),
            display_name: Some("Alice".into()),
            avatar_key: None,
            banned: false,
        }])),
    );
    assert_eq!(sidebar.people(), [("alice", "Alice")]);

    let Frame::Presence { user_id, channel_id, flags, video } = serde_json::from_str(
        r#"{"type":"voice:presence","userId":"alice","channelId":"second","muted":true,"deafened":true,"speaking":false}"#,
    ).unwrap() else { panic!("expected voice presence") };
    refresh.update_presence(&mut sidebar, user_id, channel_id, flags, video);
    refresh.update_presence(
        &mut sidebar,
        "bob".into(),
        None,
        VoiceFlags::default(),
        VideoPresence::default(),
    );
    refresh.update_presence(
        &mut sidebar,
        "new".into(),
        Some("first".into()),
        VoiceFlags::default(),
        VideoPresence::default(),
    );
    refresh.complete(
        &mut sidebar,
        Resource::Presence,
        Ok(Data::Presence(vec![
            Member {
                user_id: "alice".into(),
                channel_id: "first".into(),
                flags: VoiceFlags {
                    speaking: true,
                    ..VoiceFlags::default()
                },
                video: VideoPresence::default(),
            },
            Member {
                user_id: "bob".into(),
                channel_id: "first".into(),
                flags: VoiceFlags::default(),
                video: VideoPresence::default(),
            },
        ])),
    );

    let first = sidebar.participants("first");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].name, "Unknown user");
    let second = sidebar.participants("second");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].name, "Alice");
    assert_eq!(
        second[0].flags,
        VoiceFlags {
            muted: true,
            deafened: true,
            speaking: false
        }
    );
    assert!(refresh.presence_updates.is_none());
    assert!(
        serde_json::from_str::<Frame>(r#"{"type":"voice:presence","userId":"alice"}"#).is_err()
    );
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn avatar_loads_share_a_photo_and_discard_an_older_response() {
    use image_webp::{ColorType, WebPEncoder};

    async fn storage_request(spk: &TcpListener, storage: &TcpListener, version: &str) -> Request {
        let request = Request::read(
            spk,
            &format!("GET /api/avatar/alice?v=avatars%2Falice%2F{version}.webp HTTP/1.1"),
        )
        .await;
        let origin = format!("http://{}", spk.local_addr().unwrap());
        assert_eq!(request.header("origin"), Some(origin.as_str()));
        assert_eq!(request.header("cookie"), Some("session=avatar-test"));
        let location = format!(
            "Location: http://{}/{version}?signed=test",
            storage.local_addr().unwrap()
        );
        request.respond_raw("302 Found", &[&location], b"").await;
        let request = Request::read(storage, &format!("GET /{version}?signed=test HTTP/1.1")).await;
        for header in ["cookie", "origin", "authorization", "referer"] {
            assert_eq!(request.header(header), None);
        }
        request
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let spk = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", spk.local_addr().unwrap())).unwrap();
    let mut cookies = CookieStore::default();
    cookies.parse("session=avatar-test; Path=/", &base).unwrap();
    let client = SpkClient::new(base, cookies).unwrap();
    let (old_requested, wait_for_old) = tokio::sync::oneshot::channel();
    let (new_applied, wait_for_new) = tokio::sync::oneshot::channel();

    let server = async {
        let old = storage_request(&spk, &storage, "old").await;
        old_requested.send(()).unwrap();
        let new = storage_request(&spk, &storage, "new").await;
        let mut bytes = Vec::new();
        WebPEncoder::new(&mut bytes)
            .encode(&[255, 0, 0], 1, 1, ColorType::Rgb8)
            .unwrap();
        new.respond_raw(
            "200 OK",
            &["Content-Type: image/webp"],
            include_bytes!("../../tests/fixtures/animated-avatar.webp"),
        )
        .await;
        wait_for_new.await.unwrap();
        old.respond_raw("200 OK", &["Content-Type: image/webp"], &bytes)
            .await;

        storage_request(&spk, &storage, "corrupt")
            .await
            .respond_raw("200 OK", &["Content-Type: image/webp"], b"not an image")
            .await;
        let request = Request::read(
            &spk,
            "GET /api/avatar/alice?v=avatars%2Falice%2Fmissing.webp HTTP/1.1",
        )
        .await;
        request.respond_raw("404 Not Found", &[], b"").await;
    };

    let desktop = async {
        let profile = serde_json::from_value(value!({
            "id": "alice", "email": "alice@example.test", "avatarKey": "avatars/alice/old.webp"
        }))
        .unwrap();
        let mut sidebar = Sidebar::new(profile, None);
        let mut avatars = AvatarLoads::default();
        let mut refresh = Refresh::default();
        refresh.update_presence(
            &mut sidebar,
            "alice".into(),
            Some("voice".into()),
            VoiceFlags::default(),
            VideoPresence::default(),
        );
        let update = |sidebar: &mut Sidebar,
                      avatars: &mut AvatarLoads,
                      refresh: &mut Refresh,
                      key: Option<&str>| {
            let users = serde_json::from_value(value!([{
                "id": "alice", "displayName": "Alice", "avatarKey": key
            }]))
            .unwrap();
            refresh.complete(sidebar, Resource::Users, Ok(Data::Users(users)));
            avatars.directory_changed(sidebar);
            avatars.fill(&client, sidebar);
        };

        avatars.fill(&client, &sidebar);
        avatars.fill(&client, &sidebar);
        assert_eq!(avatars.tasks.len(), 1);
        assert!(sidebar.avatar(Some("alice")).is_none());
        wait_for_old.await.unwrap();
        update(
            &mut sidebar,
            &mut avatars,
            &mut refresh,
            Some("avatars/alice/new.webp"),
        );
        let loaded = avatars.tasks.join_next().await.unwrap().unwrap();
        avatars.complete(&mut sidebar, loaded);
        let photo = sidebar
            .avatar(sidebar.profile.id.as_deref())
            .unwrap()
            .clone();
        assert_eq!(photo.frame_count(), 2);
        new_applied.send(()).unwrap();
        let loaded = avatars.tasks.join_next().await.unwrap().unwrap();
        avatars.complete(&mut sidebar, loaded);
        assert!(Arc::ptr_eq(
            &photo,
            sidebar
                .avatar(Some(sidebar.participants("voice")[0].id))
                .unwrap()
        ));
        avatars.fill(&client, &sidebar);
        assert!(avatars.tasks.is_empty());

        for version in ["corrupt", "missing"] {
            let key = format!("avatars/alice/{version}.webp");
            update(&mut sidebar, &mut avatars, &mut refresh, Some(key.as_str()));
            assert!(sidebar.avatar(Some("alice")).is_none());
            let loaded = avatars.tasks.join_next().await.unwrap().unwrap();
            avatars.complete(&mut sidebar, loaded);
            assert!(sidebar.avatar(Some("alice")).is_none());
        }
        update(&mut sidebar, &mut avatars, &mut refresh, None::<&str>);
        assert!(sidebar.avatar_key("alice").is_none());
        assert!(avatars.tasks.is_empty());
    };

    timeout(Duration::from_secs(10), async {
        tokio::join!(server, desktop)
    })
    .await
    .expect("loopback avatar requests timed out");
}

pub(crate) struct Request {
    stream: BufReader<TcpStream>,
    headers: String,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) async fn read(listener: &TcpListener, request_line: &str) -> Self {
        let (line, request) = Self::receive(listener).await;
        assert_eq!(line, request_line);
        request
    }

    async fn receive(listener: &TcpListener) -> (String, Self) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut request = Self {
            stream: BufReader::new(stream),
            headers: String::new(),
            body: Vec::new(),
        };
        let mut line = String::new();
        request.stream.read_line(&mut line).await.unwrap();
        let request_line = line.trim_end().to_owned();
        loop {
            line.clear();
            assert_ne!(request.stream.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            request.headers.push_str(&line);
        }
        if let Some(length) = request
            .header("content-length")
            .map(|length| length.parse().unwrap())
        {
            request.body.resize(length, 0);
            request.stream.read_exact(&mut request.body).await.unwrap();
        }
        (request_line, request)
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    pub(crate) async fn respond(self, cookies: &[&str], body: &str) {
        let mut headers = vec!["Content-Type: application/json".to_owned()];
        headers.extend(cookies.iter().map(|cookie| format!("Set-Cookie: {cookie}")));
        let headers = headers.iter().map(String::as_str).collect::<Vec<_>>();
        self.respond_raw("200 OK", &headers, body.as_bytes()).await;
    }

    async fn respond_raw(mut self, status: &str, headers: &[&str], body: &[u8]) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )
        .into_bytes();
        for header in headers {
            response.extend_from_slice(header.as_bytes());
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"\r\n");
        response.extend_from_slice(body);
        self.stream.write_all(&response).await.unwrap();
    }
}

#[test]
fn voice_cues_follow_live_membership_of_other_users() {
    let profile = serde_json::from_str(r#"{"id":"me","email":"me@example.test"}"#).unwrap();
    let mut sidebar = Sidebar::new(profile, None);
    let mut call = VoiceView::joining("here".into());
    call.status = VoiceStatus::ListenOnly;
    sidebar.voice = Some(call);
    let mut refresh = Refresh::default();

    let present =
        |refresh: &mut Refresh, sidebar: &mut Sidebar, user: &str, channel: Option<&str>| {
            refresh.update_presence(
                sidebar,
                user.into(),
                channel.map(str::to_owned),
                VoiceFlags::default(),
                VideoPresence::default(),
            )
        };

    assert_eq!(
        present(&mut refresh, &mut sidebar, "already-here", Some("here")),
        vec![]
    );
    let members =
        serde_json::from_str(r#"[{"userId":"already-here","channelId":"here"}]"#).unwrap();
    refresh.complete(
        &mut sidebar,
        Resource::Presence,
        Ok(Data::Presence(members)),
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "already-here", Some("here")),
        vec![]
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "me", Some("here")),
        vec![]
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("other")),
        vec![]
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("here")),
        vec![(Sound::Join, true)]
    );
    assert_eq!(
        refresh.update_presence(
            &mut sidebar,
            "peer".into(),
            Some("here".into()),
            VoiceFlags {
                muted: true,
                ..VoiceFlags::default()
            },
            VideoPresence::default(),
        ),
        vec![],
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("other")),
        vec![(Sound::Leave, false)]
    );
    assert_eq!(present(&mut refresh, &mut sidebar, "peer", None), vec![]);
    assert_eq!(
        present(&mut refresh, &mut sidebar, "already-here", None),
        vec![(Sound::Leave, false)]
    );

    sidebar.voice.as_mut().unwrap().begin_leave(true);
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("here")),
        vec![]
    );
}

#[test]
fn local_cues_follow_first_ready_report_and_explicit_leave() {
    let report = |status| {
        Arc::new(VoiceReport {
            status,
            devices: Arc::default(),
            microphone: None,
            speakers: None,
            diagnostics: Default::default(),
            speaking: HashSet::new(),
        })
    };
    let mut call = VoiceView::joining("room".into());
    assert!(call.update_media(report(VoiceStatus::ListenOnly)));
    assert!(!call.update_media(report(VoiceStatus::Connected)));
    assert!(!call.update_media(report(VoiceStatus::Reconnecting)));
    assert_eq!(call.status, VoiceStatus::Reconnecting);
    assert!(!call.update_media(report(VoiceStatus::ListenOnly)));
    assert_eq!(call.status, VoiceStatus::ListenOnly);
    assert!(!call.update_media(report(VoiceStatus::Reconnecting)));
    assert!(call.begin_leave(true));
    assert!(call.play_leave);
    assert!(!call.begin_leave(true));
    assert!(!call.update_media(report(VoiceStatus::ListenOnly)));

    let mut cancelled = VoiceView::joining("room".into());
    assert!(cancelled.begin_leave(true));
    assert!(!cancelled.play_leave);
    assert!(!cancelled.update_media(report(VoiceStatus::Connected)));

    let mut disconnected = VoiceView::joining("room".into());
    assert!(disconnected.update_media(report(VoiceStatus::Connected)));
    assert!(disconnected.begin_leave(false));
    assert!(!disconnected.play_leave);
}

#[tokio::test]
async fn switching_cancels_a_pending_join() {
    switch_voice(false, None).await;
}

#[tokio::test]
async fn switching_leaves_an_established_call() {
    switch_voice(true, None).await;
}

#[tokio::test]
async fn server_move_cancels_a_pending_join() {
    switch_voice(false, Some("second")).await;
}

#[tokio::test]
async fn server_move_joins_before_the_destination_is_listed() {
    switch_voice(true, Some("new")).await;
}

async fn switch_voice(established: bool, moved_to: Option<&str>) {
    let destination = moved_to.unwrap_or("second");
    let (move_ready, move_when) = oneshot::channel();
    let (old_joined, old_attempt) = oneshot::channel();
    let (next_joined, next_attempt) = oneshot::channel();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, update_snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let old = accept_voice_join(&requests, &mut socket, "first").await;
            if established {
                confirm_voice_join(&mut socket, &old).await;
                socket
                    .send(Message::text(
                        r#"{"type":"voice:presence","userId":"alice","channelId":"first"}"#,
                    ))
                    .await
                    .unwrap();
            }
            old_joined.send(()).unwrap();
            move_when.await.unwrap();
            if moved_to.is_some() {
                let unrelated_destination = if destination == "second" {
                    "new"
                } else {
                    "second"
                };
                send_voice_move(
                    &mut socket,
                    &value!("another-session"),
                    unrelated_destination,
                )
                .await;
                send_voice_move(&mut socket, &old["sid"], destination).await;
            }
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:leave","sid":old["sid"]})
            );

            if established {
                socket
                    .send(Message::text(
                        r#"{"type":"voice:presence","userId":"alice","channelId":null}"#,
                    ))
                    .await
                    .unwrap();
            }
            let next = accept_voice_join(&requests, &mut socket, destination).await;
            assert_ne!(next["sid"], old["sid"]);
            assert_ne!(next["nonce"], old["nonce"]);
            acknowledge_voice(&mut socket, &old).await;
            if moved_to.is_some() {
                send_voice_move(&mut socket, &old["sid"], "first").await;
                socket
                    .send(Message::text(
                        value!({
                            "type":"voice:disconnect","sid":old["sid"]
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                if destination == "new" {
                    update_snapshots.send(SidebarSnapshot::Updated).unwrap();
                    socket
                        .send(Message::text(r#"{"type":"channels:changed"}"#))
                        .await
                        .unwrap();
                }
            }
            assert!(
                timeout(Duration::from_millis(60), receive_voice_frame(&mut socket))
                    .await
                    .is_err()
            );
            confirm_voice_join(&mut socket, &next).await;
            socket
                .send(Message::text(
                    value!({
                        "type":"voice:presence","userId":"alice","channelId":destination
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            next_joined.send(()).unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:leave","sid":next["sid"]})
            );
        },
        async |commands, views| {
            request_first_voice(&commands, &views).await;
            old_attempt.await.unwrap();
            if established {
                wait_for_sidebar(&views, shows_alice_speaking).await;
            }
            if moved_to.is_none() {
                commands
                    .send(Command::JoinVoice(destination.into()))
                    .await
                    .unwrap();
            }
            move_ready.send(()).unwrap();
            wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .voice
                    .as_ref()
                    .is_some_and(|voice| voice.channel_id == destination)
                    && shows_alice_speaking(sidebar)
                    && sidebar
                        .participants("first")
                        .iter()
                        .all(|person| person.id != "alice")
                    && sidebar
                        .participants(destination)
                        .iter()
                        .any(|person| person.id == "alice")
            })
            .await;
            next_attempt.await.unwrap();
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

async fn send_voice_move(socket: &mut WebSocketStream<TcpStream>, sid: &Value, channel_id: &str) {
    socket
        .send(Message::text(
            value!({
                "type":"voice:move","sid":sid,"channelId":channel_id
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn server_move_releases_the_original_call_when_the_destination_fails() {
    let (ready, connected) = oneshot::channel();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, _| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let old = establish_voice(&requests, &mut socket).await;
            connected.await.unwrap();
            send_voice_move(&mut socket, &old["sid"], "deleted").await;
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:leave","sid":old["sid"]})
            );
            request_voice_token(&requests, "deleted")
                .await
                .respond_raw("404 Not Found", &[], b"")
                .await;
            assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        },
        async |commands, views| {
            join_test_voice(&commands, &views).await;
            ready.send(()).unwrap();
            let sidebar = wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
            assert_eq!(sidebar.voice_notice, Some(VoiceIssue::Token.message()));
            assert!(sidebar.switching_to().is_none());
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn leave_cancels_a_voice_channel_switch() {
    let (claimed, old_attempt) = oneshot::channel();
    let (acknowledged, late_ack) = oneshot::channel();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, _| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let old = accept_voice_join(&requests, &mut socket, "first").await;
            claimed.send(()).unwrap();
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:leave","sid":old["sid"]})
            );
            acknowledge_voice(&mut socket, &old).await;
            acknowledged.send(()).unwrap();
            assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        },
        async |commands, views| {
            request_first_voice(&commands, &views).await;
            old_attempt.await.unwrap();
            commands
                .send(Command::JoinVoice("second".into()))
                .await
                .unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.switching_to() == Some("second")).await;
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| {
                sidebar.voice.is_none() && sidebar.switching_to().is_none()
            })
            .await;
            late_ack.await.unwrap();
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[test]
fn terminal_media_failure_offers_the_affected_voice_channel_for_rejoin() {
    let profile = serde_json::from_str(r#"{"id":"me","email":"me@example.test"}"#).unwrap();
    let mut sidebar = Sidebar::new(profile, None);
    sidebar.channels = Some(vec![Channel {
        id: "room".into(),
        kind: "voice".into(),
        name: None,
    }]);
    sidebar.voice = Some(VoiceView::joining("room".into()));
    sidebar.voice.as_mut().unwrap().status = VoiceStatus::Reconnecting;
    sidebar.finish_voice(Err(VoiceIssue::LiveKitDisconnected));
    assert!(sidebar.voice.is_none());
    assert_eq!(sidebar.rejoin_channel(), None);
    sidebar.connection = Connection::Online;
    assert_eq!(sidebar.rejoin_channel(), Some("room"));
    sidebar.channels = Some(Vec::new());
    assert_eq!(sidebar.rejoin_channel(), None);

    sidebar.voice = Some(VoiceView::joining("room".into()));
    sidebar.voice.as_mut().unwrap().begin_leave(true);
    sidebar.finish_voice(Err(VoiceIssue::LiveKitDisconnected));
    assert!(sidebar.voice_rejoin.is_none());
}

#[tokio::test]
async fn livekit_recovery_and_rejoin_use_the_signed_in_voice_flow() {
    let (resynced, confirmed) = tokio::sync::oneshot::channel();
    let end = voice_session(
        voice::tests::start_recovery_fixture,
        async |listener, requests, _| {
            let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let first = accept_voice_join(&requests, &mut socket, "first").await;
            acknowledge_voice(&mut socket, &first).await;
            let sid = &first["sid"];
            let ready = value!({"type":"voice:ready","sid":sid});
            assert_eq!(receive_voice_frame(&mut socket).await, ready);
            assert_eq!(receive_voice_frame(&mut socket).await, ready);
            assert_eq!(
                receive_voice_frame(&mut socket).await,
                value!({"type":"voice:state","sid":sid,"muted":true,"deafened":false,"speaking":false,"cam":false,"screen":false,"watching":[]})
            );
            resynced.send(()).unwrap();
            assert_eq!(receive_voice_frame(&mut socket).await, value!({"type":"voice:leave","sid":sid}));

            let retry = accept_voice_join(&requests, &mut socket, "first").await;
            assert_ne!(retry["sid"], *sid);
            assert_ne!(retry["nonce"], first["nonce"]);
            acknowledge_voice(&mut socket, &retry).await;
            let sid = &retry["sid"];
            assert_eq!(receive_voice_frame(&mut socket).await, value!({"type":"voice:ready","sid":sid}));
            assert_eq!(receive_voice_frame(&mut socket).await, value!({"type":"voice:leave","sid":sid}));
        },
        async |commands, views| {
            wait_for_sidebar(&views, |sidebar| {
                matches!(sidebar.connection, Connection::Online) && sidebar.has_voice_channel("first")
            }).await;
            commands.send(Command::JoinVoice("first".into())).await.unwrap();
            let status = |expected| move |sidebar: &Sidebar| {
                sidebar.voice.as_ref().is_some_and(|voice| voice.status == expected)
            };
            wait_for_sidebar(&views, status(VoiceStatus::ListenOnly)).await;
            commands.send(Command::RefreshDevices).await.unwrap();
            wait_for_sidebar(&views, status(VoiceStatus::Reconnecting)).await;
            commands.send(Command::RefreshDevices).await.unwrap();
            wait_for_sidebar(&views, status(VoiceStatus::ListenOnly)).await;
            confirmed.await.unwrap();
            commands.send(Command::RefreshDevices).await.unwrap();
            let failed = wait_for_sidebar(&views, |sidebar| sidebar.rejoin_channel().is_some()).await;
            assert!(failed.voice.is_none());
            assert_eq!(failed.voice_notice, Some(VoiceIssue::LiveKitDisconnected.message()));
            commands.send(Command::JoinVoice(failed.rejoin_channel().unwrap().into())).await.unwrap();
            let joined = wait_for_sidebar(&views, status(VoiceStatus::ListenOnly)).await;
            assert!(joined.voice_notice.is_none());
            assert!(joined.rejoin_channel().is_none());
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
    ).await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn camera_and_watch_choices_are_scoped_to_the_current_voice_session() {
    let (observed, observations) = async_channel::bounded(1);
    let end =
        voice_session(
            Call::start_for_test,
            async |listener, requests, _| {
                let mut socket = accept_async(listener.accept().await.unwrap().0)
                    .await
                    .unwrap();
                let join = establish_voice(&requests, &mut socket).await;
                let sid = &join["sid"];
                socket.send(Message::text(value!({
                "type":"voice:presence", "userId":"bob", "channelId":"first", "cam":true,
                "watching":["alice"],
            }).to_string())).await.unwrap();

                let watch = receive_voice_frame(&mut socket).await;
                assert_eq!(watch["sid"], *sid);
                assert_eq!(watch["watching"], value!(["bob"]));
                observed.send(()).await.unwrap();
                let publish = receive_voice_frame(&mut socket).await;
                assert_eq!(publish["cam"], true);
                assert_eq!(publish["watching"], value!(["bob"]));

                socket
                    .send(Message::text(
                        value!({
                            "type":"voice:presence", "userId":"bob", "channelId":null,
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                let departed = receive_voice_frame(&mut socket).await;
                assert_eq!(departed["cam"], true);
                assert_eq!(departed["watching"], value!([]));
                observed.send(()).await.unwrap();

                let stop = receive_voice_frame(&mut socket).await;
                assert_eq!(stop["cam"], false);
                assert_eq!(stop["watching"], value!([]));
                observed.send(()).await.unwrap();
                assert_eq!(
                    receive_voice_frame(&mut socket).await,
                    value!({"type":"voice:leave","sid":sid})
                );
            },
            async |commands, views| {
                join_test_voice(&commands, &views).await;
                let view = wait_for_sidebar(&views, |sidebar| {
                    sidebar.viewers("first", "alice").len() == 1
                })
                .await;
                assert!(view.participants("first").iter().any(|participant| {
                    participant.id == "bob" && participant.video.is_some_and(|video| video.cam)
                }));
                commands
                    .send(Command::SetWatching("alice".into(), true))
                    .await
                    .unwrap();
                commands
                    .send(Command::SetWatching("bob".into(), true))
                    .await
                    .unwrap();
                let view = wait_for_sidebar(&views, |sidebar| {
                    sidebar.voice_intent.watching.contains("bob")
                })
                .await;
                assert!(!view.voice_intent.watching.contains("alice"));
                observations.recv().await.unwrap();
                commands
                    .send(Command::ChooseCameraResolution(CameraResolution::P720))
                    .await
                    .unwrap();
                commands
                    .send(Command::SetCameraEnabled(true))
                    .await
                    .unwrap();
                let view = wait_for_sidebar(&views, |sidebar| {
                    sidebar.voice_intent.camera.enabled && sidebar.voice_intent.watching.is_empty()
                })
                .await;
                assert!(view.viewers("first", "alice").is_empty());
                observations.recv().await.unwrap();
                commands
                    .send(Command::SetCameraEnabled(false))
                    .await
                    .unwrap();
                wait_for_sidebar(&views, |sidebar| !sidebar.voice_intent.camera.enabled).await;
                observations.recv().await.unwrap();
                commands.send(Command::LeaveVoice).await.unwrap();
                let view = wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
                assert!(!view.voice_intent.camera.enabled);
                assert!(view.voice_intent.watching.is_empty());
                assert_eq!(view.voice_intent.camera.resolution, CameraResolution::P720);
            },
        )
        .await;
    assert!(matches!(end, SessionEnd::Closed));
}
