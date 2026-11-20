use std::time::Duration;

use futures::{SinkExt, StreamExt};
use reqwest::cookie::CookieStore as _;
use serde_json::{Value, json as value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{Message, handshake::server},
};

use super::*;

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
    let mut refresh = Refresh {
        presence_updates: Some(HashMap::new()),
        ..Default::default()
    };
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

    let Frame::Presence { user_id, channel_id, flags } = serde_json::from_str(
        r#"{"type":"voice:presence","userId":"alice","channelId":"second","muted":true,"deafened":true,"speaking":false}"#,
    ).unwrap() else { panic!("expected voice presence") };
    refresh.update_presence(&mut sidebar, user_id, channel_id, flags);
    refresh.update_presence(&mut sidebar, "bob".into(), None, VoiceFlags::default());
    refresh.update_presence(
        &mut sidebar,
        "new".into(),
        Some("first".into()),
        VoiceFlags::default(),
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
            },
            Member {
                user_id: "bob".into(),
                channel_id: "first".into(),
                flags: VoiceFlags::default(),
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
        let (stream, _) = listener.accept().await.unwrap();
        let mut request = Self {
            stream: BufReader::new(stream),
            headers: String::new(),
            body: Vec::new(),
        };
        let mut line = String::new();
        request.stream.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim_end(), request_line);
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
        request
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
fn room_cues_follow_live_membership_of_other_users() {
    let profile = serde_json::from_str(r#"{"id":"me","email":"me@example.test"}"#).unwrap();
    let mut sidebar = Sidebar::new(profile, None);
    let mut call = VoiceView::joining("here".into());
    call.status = VoiceStatus::ListenOnly;
    sidebar.voice = Some(call);
    let mut refresh = Refresh::default();

    let present = |refresh: &mut Refresh, sidebar: &mut Sidebar, user: &str, room: Option<&str>| {
        refresh.update_presence(
            sidebar,
            user.into(),
            room.map(str::to_owned),
            VoiceFlags::default(),
        )
    };

    assert_eq!(
        present(&mut refresh, &mut sidebar, "already-here", Some("here")),
        None
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
        None
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "me", Some("here")),
        None
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("other")),
        None
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("here")),
        Some(Sound::Join)
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
        ),
        None,
    );
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("other")),
        Some(Sound::Leave)
    );
    assert_eq!(present(&mut refresh, &mut sidebar, "peer", None), None);
    assert_eq!(
        present(&mut refresh, &mut sidebar, "already-here", None),
        Some(Sound::Leave)
    );

    sidebar.voice.as_mut().unwrap().begin_leave(true);
    assert_eq!(
        present(&mut refresh, &mut sidebar, "peer", Some("here")),
        None
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
            speaking: HashSet::new(),
        })
    };
    let mut call = VoiceView::joining("room".into());
    assert!(call.update_media(report(VoiceStatus::ListenOnly)));
    assert!(!call.update_media(report(VoiceStatus::Connected)));
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
