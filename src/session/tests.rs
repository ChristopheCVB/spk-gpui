mod account_admin;
mod accounts;
#[cfg(debug_assertions)]
mod attachments;
mod changelog;
mod channel_admin;
mod chat;
mod gifs;
mod media;
mod moderation;
mod profile;
mod radio;
mod search;
mod soundboard;
#[cfg(debug_assertions)]
mod soundboard_admin;
mod unread;
mod user_presence;
mod webhooks;

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
    sounds::SoundSettings,
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
    for expired in [false, true] {
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

        if !expired {
            let mut abandoned = TcpStream::connect(http.local_addr().unwrap())
                .await
                .unwrap();
            abandoned.shutdown().await.unwrap();
        }
        let (tx, commands) = async_channel::unbounded();
        let (views, rx) = async_channel::unbounded();
        let (saved, storage) = SavedSession::with_test_store(Some(bytes.clone()));
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
                    .respond(
                        &[],
                        r#"{"id":"alice","email":"alice@example.test","displayName":"Alice"}"#,
                    )
                    .await;
                return;
            }
            request
                .respond(
                    &[],
                    r#"{"id":"alice","email":"alice@example.test","displayName":"Alice"}"#,
                )
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
                    "GET /api/unread HTTP/1.1" => request.respond(&[], "{}").await,
                    "GET /api/channels HTTP/1.1"
                    | "GET /api/voice HTTP/1.1"
                    | "GET /api/users HTTP/1.1" => request.respond(&[], "[]").await,
                    "GET /api/users/presence HTTP/1.1" => request.respond(&[], "{}").await,
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
                tx.send(Command::OpenAccountLink).await.unwrap();
                assert!(matches!(rx.recv().await.unwrap(), View::AccountLink { .. }));
                tx.send(Command::BackToSignIn).await.unwrap();
                assert!(matches!(
                    rx.recv().await.unwrap(),
                    View::SignIn {
                        message: Some(SESSION_EXPIRED),
                        retry_restore: false
                    }
                ));
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
    AccountAdmin {
        role: &'static str,
        target_role: &'static str,
        banned: bool,
    },
    Updated,
    VoiceReplaced,
    FailPresenceOnce,
    DelayUserPresence,
    VoiceModeration {
        role: &'static str,
        channel: Option<&'static str>,
    },
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
    voice_session_with_activity(watch::channel(true).1, start_call, server, desktop).await
}

async fn voice_session_with_activity(
    activity: watch::Receiver<bool>,
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
        let mut radio_favorite = false;
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
                    SidebarSnapshot::Initial
                    | SidebarSnapshot::AccountAdmin { .. }
                    | SidebarSnapshot::FailPresenceOnce
                    | SidebarSnapshot::DelayUserPresence => "[]",
                    SidebarSnapshot::VoiceModeration {
                        channel: Some(channel),
                        ..
                    } => {
                        let body = value!([{"userId":"bob","channelId":channel}]);
                        request.respond(&[], &body.to_string()).await;
                        continue;
                    }
                    SidebarSnapshot::VoiceModeration { channel: None, .. } => "[]",
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
                "GET /api/users HTTP/1.1"
                    if let SidebarSnapshot::VoiceModeration { role, .. } = snapshot =>
                {
                    request
                        .respond(&[], &value!([{"id":"alice","role":role}]).to_string())
                        .await;
                    continue;
                }
                "GET /api/users HTTP/1.1"
                    if let SidebarSnapshot::AccountAdmin {
                        role,
                        target_role,
                        banned,
                    } = snapshot =>
                {
                    request
                        .respond(
                            &[],
                            &value!([
                                {"id":"alice","role":role},
                                {"id":"bob","displayName":"Bob","role":target_role,"banned":banned},
                            ])
                            .to_string(),
                        )
                        .await;
                    continue;
                }
                "GET /api/users HTTP/1.1" => "[]",
                "GET /api/admin/users HTTP/1.1"
                    if let SidebarSnapshot::AccountAdmin {
                        target_role,
                        banned,
                        ..
                    } = snapshot =>
                {
                    assert_eq!(request.header("cookie"), Some("session=fixture"));
                    request
                        .respond(
                            &[],
                            &value!({
                                "users":[{"id":"bob","email":"bob@example.test","displayName":"Bob",
                                    "role":target_role,"banned":banned,"bytes":512}],
                                "totalBytes":4096,
                            })
                            .to_string(),
                        )
                        .await;
                    continue;
                }
                line if line.contains(" /api/invites") || line.contains(" /api/admin/users") => {
                    assert_eq!(request.header("cookie"), Some("session=fixture"));
                    token_requests.send(request).await.unwrap();
                    continue;
                }

                "GET /api/unread HTTP/1.1" => "{}",
                "GET /api/users/presence HTTP/1.1"
                    if matches!(snapshot, SidebarSnapshot::DelayUserPresence) =>
                {
                    token_requests.send(request).await.unwrap();
                    continue;
                }
                "GET /api/users/presence HTTP/1.1" => "{}",
                line if line.contains(" /api/radio/") => {
                    radio::respond(line, request, &mut radio_favorite).await;
                    continue;
                }
                line if line.starts_with("GET /api/sounds") => {
                    assert_eq!(request.header("cookie"), Some("session=fixture"));
                    token_requests.send(request).await.unwrap();
                    continue;
                }
                "POST /api/voice/token HTTP/1.1" => {
                    token_requests.send(request).await.unwrap();
                    continue;
                }
                "POST /api/admin/voice/move HTTP/1.1"
                | "POST /api/admin/voice/disconnect HTTP/1.1" => {
                    let payload: Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(
                        payload.get("channelId").is_some(),
                        line == "POST /api/admin/voice/move HTTP/1.1",
                    );
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
        let (mut saved, _) = SavedSession::with_test_store(Some(persisted));
        let mut settings = fixture_settings(socket_endpoint, start_call);
        settings.presence.activity = activity;
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
) -> SessionSettings {
    let mut sounds = SoundSettings::default();
    for sound in Sound::ALL {
        sounds.get_mut(sound).enabled = false;
    }
    SessionSettings {
        socket_endpoint,
        drafts_path: None,
        start_call: Box::new(start_call),
        enumerate_devices: Devices::default,
        devices: DeviceSettings {
            path: None,
            preferred: DevicePreferences::default(),
            notice: None,
        },
        sounds: VoiceSounds::new(watch::channel(sounds).1),
        window_active: watch::channel(false).1,
        alert_activation: watch::channel(None).1,
        presence: PresenceSettings {
            path: None,
            status: UserStatus::default(),
            activity: watch::channel(true).1,
        },
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

async fn receive_non_presence_message(
    socket: &mut WebSocketStream<TcpStream>,
) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
    loop {
        let message = socket.next().await;
        if let Some(Ok(Message::Text(text))) = &message
            && let Ok(frame) = serde_json::from_str::<Value>(text.as_str())
            && frame["type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("presence:"))
        {
            continue;
        }
        return message;
    }
}

async fn receive_voice_frame(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let frame = receive_non_presence_message(socket).await.unwrap().unwrap();
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
                Some(Ok(Message::Text(frame))) = receive_non_presence_message(&mut socket) => {
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
        let (reconnected, presence_seen) = oneshot::channel();
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
                        assert_eq!(
                            receive_non_presence_message(&mut socket)
                                .await
                                .unwrap()
                                .unwrap(),
                            Message::text("ping")
                        );
                        socket
                            .send(Message::text(
                                r#"{"type":"voice:presence","userId":"bob","channelId":"first"}"#,
                            ))
                            .await
                            .unwrap();
                        presence_seen.await.unwrap();
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
                    reconnected.send(()).unwrap();
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
            role: "user".into(),
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
                    other_user_id: None,
                    last_activity_at: None,
                    icon: None,
                    owner_id: None,
                    private: false,
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
            role: "user".into(),
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
            speaking: false,
            radio_listening: None,
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
        assert_eq!(photo.image.frame_count(), 2);
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
    line: String,
}

impl Request {
    pub(crate) async fn read(listener: &TcpListener, request_line: &str) -> Self {
        let (line, request) = Self::receive(listener).await;
        assert_eq!(line, request_line);
        request
    }

    async fn receive(listener: &TcpListener) -> (String, Self) {
        let mut line = String::new();
        let stream = loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            // Cancelled requests can close the connection before sending any bytes.
            if stream.read_line(&mut line).await.unwrap() != 0 {
                break stream;
            }
        };
        let request_line = line.trim_end().to_owned();
        let mut request = Self {
            stream,
            headers: String::new(),
            body: Vec::new(),
            line: request_line.clone(),
        };
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
    let watching = |enabled: bool| VideoPresence {
        watching: enabled.then(|| "me".to_owned()).into_iter().collect(),
        ..Default::default()
    };
    let update_watch = |refresh: &mut Refresh, sidebar: &mut Sidebar, enabled: bool| {
        refresh.update_presence(
            sidebar,
            "peer".into(),
            Some("here".into()),
            VoiceFlags::default(),
            watching(enabled),
        )
    };
    sidebar.voice.as_mut().unwrap().video = Some(Arc::new(crate::voice::video::VideoReport {
        camera: crate::voice::video::CameraStatus::Live(640, 360),
        ..Default::default()
    }));
    assert!(update_watch(&mut refresh, &mut sidebar, true).is_empty());
    sidebar.voice.as_mut().unwrap().screen = Some(screen::Report {
        attempt: 1,
        state: screen::State::Live {
            width: 1280,
            height: 720,
            fps: 30,
            quality: screen::Quality {
                height: 720,
                fps: 30,
            },
            quality_failed: None,
            audio: None,
            audio_unavailable: false,
            frames: Default::default(),
        },
    });
    assert_eq!(
        update_watch(&mut refresh, &mut sidebar, false),
        [(Sound::Watch, false)]
    );
    assert_eq!(
        update_watch(&mut refresh, &mut sidebar, true),
        [(Sound::Watch, true)]
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
            echo_reference_unavailable: false,
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
            assert!(!matches!(
                receive_non_presence_message(&mut socket).await,
                Some(Ok(Message::Text(_)))
            ));
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
            assert!(!matches!(
                receive_non_presence_message(&mut socket).await,
                Some(Ok(Message::Text(_)))
            ));
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
        other_user_id: None,
        last_activity_at: None,
        icon: None,
        owner_id: None,
        private: false,
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
async fn native_screen_selection_waits_for_confirmation_in_the_current_attempt() {
    let mut sidebar = Sidebar::new(
        serde_json::from_str(r#"{"id":"alice","email":"alice@example.test"}"#).unwrap(),
        None,
    );
    let mut voice = VoiceView::joining("first".into());
    voice.status = VoiceStatus::Connected;
    sidebar.voice = Some(voice);
    let session = VoiceSession::default();
    let mut settings = fixture_settings(String::new(), Call::start_for_test);
    session
        .apply_controls(&mut sidebar, &mut settings, Command::OpenScreenShare)
        .await;
    let first = sidebar.voice_intent.screen.attempt;
    session
        .apply_controls(&mut sidebar, &mut settings, Command::StopScreenShare(first))
        .await;
    session
        .apply_controls(&mut sidebar, &mut settings, Command::OpenScreenShare)
        .await;
    let attempt = sidebar.voice_intent.screen.attempt;

    let confirmation = |attempt, can_choose_display| screen::Report {
        attempt,
        state: screen::State::Confirm {
            label: "Window".into(),
            audio: None,
            can_choose_display,
        },
    };
    session.screen_report(&mut sidebar, &settings, confirmation(first, false));
    session
        .apply_controls(
            &mut sidebar,
            &mut settings,
            Command::StartScreenShare(attempt, true),
        )
        .await;
    assert_eq!(
        sidebar.voice_intent.screen.action,
        screen::Action::Open(None)
    );

    session.screen_report(&mut sidebar, &settings, confirmation(attempt, true));
    session
        .apply_controls(
            &mut sidebar,
            &mut settings,
            Command::ChooseScreenShareDisplay(first),
        )
        .await;
    assert_eq!(sidebar.voice_intent.screen.attempt, attempt);
    session
        .apply_controls(
            &mut sidebar,
            &mut settings,
            Command::ChooseScreenShareDisplay(attempt),
        )
        .await;
    let retry = sidebar.voice_intent.screen.attempt;
    assert_eq!(retry, attempt + 1);
    assert!(sidebar.voice_intent.screen.display_only);
    assert_eq!(
        sidebar.voice_intent.screen.action,
        screen::Action::Open(None)
    );
    session.screen_report(&mut sidebar, &settings, confirmation(retry, false));
    session
        .apply_controls(
            &mut sidebar,
            &mut settings,
            Command::StartScreenShare(retry, true),
        )
        .await;
    assert_eq!(
        sidebar.voice_intent.screen.action,
        screen::Action::Start(false)
    );
    assert_eq!(
        sidebar.voice.as_ref().unwrap().status,
        VoiceStatus::Connected
    );
}

#[tokio::test]
async fn queued_screen_start_respects_the_latest_audio_preference() {
    let mut sidebar = Sidebar::new(
        serde_json::from_str(r#"{"id":"alice","email":"alice@example.test"}"#).unwrap(),
        None,
    );
    let mut view = VoiceView::joining("first".into());
    view.status = VoiceStatus::Connected;
    let request = &mut sidebar.voice_intent.screen;
    request.open(None);
    let attempt = request.attempt;
    assert!(request.advance(attempt, screen::Action::Choose("display".into(), None)));
    view.screen = Some(screen::Report {
        attempt,
        state: screen::State::Confirm {
            label: "Display".into(),
            audio: Some(screen::AudioScope::PlaybackMix),
            can_choose_display: false,
        },
    });
    sidebar.voice = Some(view);
    let session = VoiceSession::default();
    let mut settings = fixture_settings(String::new(), Call::start_for_test);

    session
        .apply_controls(&mut sidebar, &mut settings, Command::SetScreenAudio(false))
        .await;
    session
        .apply_controls(
            &mut sidebar,
            &mut settings,
            Command::StartScreenShare(attempt, true),
        )
        .await;
    assert_eq!(
        sidebar.voice_intent.screen.action,
        screen::Action::Start(false)
    );
}

#[test]
fn failed_screen_quality_restores_the_previous_preset_for_the_current_share() {
    let mut sidebar = Sidebar::new(
        serde_json::from_str(r#"{"id":"alice","email":"alice@example.test"}"#).unwrap(),
        None,
    );
    let mut voice = VoiceView::joining("first".into());
    voice.status = VoiceStatus::Connected;
    sidebar.voice = Some(voice);
    let request = &mut sidebar.voice_intent.screen;
    request.open(None);
    let attempt = request.attempt;
    assert!(request.advance(attempt, screen::Action::Choose("display".into(), None)));
    assert!(request.advance(attempt, screen::Action::Start(false)));
    let applied = screen::Quality::default();
    let rejected = screen::Quality {
        height: 1440,
        fps: 60,
    };
    request.quality = rejected;
    let report = |attempt| screen::Report {
        attempt,
        state: screen::State::Live {
            width: 1920,
            height: 1080,
            fps: 30,
            quality: applied,
            quality_failed: Some(rejected),
            audio: None,
            audio_unavailable: false,
            frames: Default::default(),
        },
    };
    let session = VoiceSession::default();
    let settings = fixture_settings(String::new(), Call::start_for_test);
    session.screen_report(&mut sidebar, &settings, report(attempt - 1));
    assert_eq!(sidebar.voice_intent.screen.quality, rejected);
    session.screen_report(&mut sidebar, &settings, report(attempt));
    assert_eq!(sidebar.voice_intent.screen.quality, applied);
    assert!(
        sidebar
            .voice
            .as_ref()
            .unwrap()
            .screen
            .as_ref()
            .unwrap()
            .state
            .active()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_picker_failures_allow_retry_in_the_same_voice_session() {
    let mut sidebar = Sidebar::new(
        serde_json::from_str(r#"{"id":"alice","email":"alice@example.test"}"#).unwrap(),
        None,
    );
    let mut view = VoiceView::joining("first".into());
    view.status = VoiceStatus::Connected;
    sidebar.voice = Some(view);
    let session = VoiceSession::default();
    let mut settings = fixture_settings(String::new(), Call::start_for_test);
    session
        .apply_controls(&mut sidebar, &mut settings, Command::OpenScreenShare)
        .await;

    for (error, state) in [
        (screen::CaptureError::Cancelled, screen::State::Cancelled),
        (
            screen::CaptureError::Permission,
            screen::State::Ended(
                "Allow screen capture in system settings, then select a source again.",
            ),
        ),
    ] {
        let attempt = sidebar.voice_intent.screen.attempt;
        session.screen_report(
            &mut sidebar,
            &settings,
            screen::Report {
                attempt,
                state: screen::State::Choices(vec![]),
            },
        );
        session
            .apply_controls(
                &mut sidebar,
                &mut settings,
                Command::ChooseWindowsScreenSource(attempt, Err(error)),
            )
            .await;
        assert_eq!(
            sidebar.voice_intent.screen.action,
            screen::Action::ChooseSystem(Err(error))
        );
        session.screen_report(
            &mut sidebar,
            &settings,
            screen::Report {
                attempt,
                state: state.clone(),
            },
        );
        let voice = sidebar.voice.as_ref().unwrap();
        assert_eq!(voice.channel_id, "first");
        assert_eq!(voice.status, VoiceStatus::Connected);
        match state {
            screen::State::Cancelled => assert!(voice.screen.is_none()),
            screen::State::Ended(notice) => assert!(matches!(
                voice.screen.as_ref().unwrap().state,
                screen::State::Ended(message) if message == notice
            )),
            _ => unreachable!(),
        }
        session
            .apply_controls(&mut sidebar, &mut settings, Command::OpenScreenShare)
            .await;
        assert_eq!(sidebar.voice_intent.screen.attempt, attempt + 1);
    }
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
                let stop_watching = receive_voice_frame(&mut socket).await;
                assert_eq!(stop_watching["watching"], value!([]));
                observed.send(()).await.unwrap();
                let watch_again = receive_voice_frame(&mut socket).await;
                assert_eq!(watch_again["watching"], value!(["bob"]));
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
                for change in [
                    ParticipantVolumeChange::Microphone(125),
                    ParticipantVolumeChange::Screen(40),
                    ParticipantVolumeChange::Muted(true),
                ] {
                    commands
                        .send(Command::SetParticipantVolume("bob".into(), change))
                        .await
                        .unwrap();
                }
                commands
                    .send(Command::SetParticipantVolume(
                        "alice".into(),
                        ParticipantVolumeChange::Muted(true),
                    ))
                    .await
                    .unwrap();
                let view = wait_for_sidebar(&views, |sidebar| {
                    sidebar
                        .voice_intent
                        .preferred
                        .participant_volume("bob")
                        .muted
                })
                .await;
                let volume = view.voice_intent.preferred.participant_volume("bob");
                assert_eq!((volume.mic, volume.screen), (125, 40));
                commands
                    .send(Command::SetWatching("bob".into(), true))
                    .await
                    .unwrap();
                let view = wait_for_sidebar(&views, |sidebar| {
                    sidebar.voice_intent.watching.contains("bob")
                })
                .await;
                assert!(!view.voice_intent.watching.contains("alice"));
                assert!(
                    !view
                        .voice_intent
                        .preferred
                        .participant_volume("alice")
                        .muted
                );
                observations.recv().await.unwrap();
                commands
                    .send(Command::SetWatching("bob".into(), false))
                    .await
                    .unwrap();
                observations.recv().await.unwrap();
                commands
                    .send(Command::SetWatching("bob".into(), true))
                    .await
                    .unwrap();
                observations.recv().await.unwrap();
                commands.send(Command::SetScreenHeight(1440)).await.unwrap();
                commands.send(Command::SetScreenFps(60)).await.unwrap();
                commands.send(Command::SetScreenHeight(900)).await.unwrap();
                commands.send(Command::SetScreenFps(25)).await.unwrap();

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
                assert!(view.voice_intent.preferred.participant_volume("bob").muted);
                assert_eq!(
                    view.voice_intent.screen.quality,
                    screen::Quality {
                        height: 1440,
                        fps: 60
                    }
                );
                observations.recv().await.unwrap();
                let (preview, frames) = async_channel::bounded(1);
                commands
                    .send(Command::PreviewCamera(preview))
                    .await
                    .unwrap();
                assert!(frames.recv().await.is_err());
                commands
                    .send(Command::SetWatching("bob".into(), true))
                    .await
                    .unwrap();
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
