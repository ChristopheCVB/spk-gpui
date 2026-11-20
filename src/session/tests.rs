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
            banned: false,
        }])),
    );
    assert_eq!(sidebar.people(), ["Alice"]);

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

    pub(crate) async fn respond(mut self, cookies: &[&str], body: &str) {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len(),
        );
        for cookie in cookies {
            response.push_str(&format!("Set-Cookie: {cookie}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(body);
        self.stream.write_all(response.as_bytes()).await.unwrap();
    }
}
