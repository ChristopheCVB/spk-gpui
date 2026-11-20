use cookie_store::CookieStore;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use crate::{
    api::{Frame, SocketEvent},
    session::tests::Request,
};

use super::*;

#[derive(Clone, Copy)]
enum Scenario {
    Cancelled,
    MediaFailure,
    MicrophoneFailure,
    Controls,
}

#[tokio::test]
async fn voice_claim_and_cleanup_follow_spk_protocol() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for scenario in [
        Scenario::Cancelled,
        Scenario::MediaFailure,
        Scenario::MicrophoneFailure,
        Scenario::Controls,
    ] {
        timeout(Duration::from_secs(4), exercise(scenario))
            .await
            .expect("loopback voice protocol timed out");
    }
}

async fn exercise(scenario: Scenario) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let endpoint = format!("ws://{address}/_ws");
    let controls = matches!(scenario, Scenario::Controls);
    let (left, departure) = oneshot::channel();

    let server = async {
        Request::read(&listener, "POST /api/auth/sign-in/email HTTP/1.1")
            .await
            .respond(&["session=fixture; Path=/; HttpOnly"], "{}")
            .await;
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();

        let request = Request::read(&listener, "POST /api/voice/token HTTP/1.1").await;
        assert_eq!(request.header("origin"), Some(origin.as_str()));
        assert_eq!(request.header("cookie"), Some("session=fixture"));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({"channelId":"room"})
        );
        request
            .respond(
                &[],
                &json!({"token":"fixture-token","url":env!("LIVEKIT_URL")}).to_string(),
            )
            .await;

        let join = receive_frame(&mut socket).await;
        let sid = join["sid"].as_str().unwrap();
        let nonce = join["nonce"].as_str().unwrap();
        assert!(Uuid::parse_str(sid).is_ok());
        assert!(Uuid::parse_str(nonce).is_ok());
        assert_eq!(
            join,
            json!({
                "type":"voice:join", "channelId":"room", "sid":sid, "nonce":nonce,
                "muted":true, "deafened":controls, "watching":[]
            })
        );

        for (sid, channel, nonce) in [
            ("another", "room", nonce),
            (sid, "another", nonce),
            (sid, "room", "previous"),
        ] {
            acknowledge(&mut socket, sid, channel, nonce).await;
        }
        if !matches!(scenario, Scenario::Cancelled) {
            acknowledge(&mut socket, sid, "room", nonce).await;
        }
        if matches!(scenario, Scenario::MicrophoneFailure | Scenario::Controls) {
            assert_eq!(
                receive_frame(&mut socket).await,
                json!({"type":"voice:ready","sid":sid})
            );
        }
        if controls {
            assert_eq!(
                receive_frame(&mut socket).await,
                json!({"type":"voice:state","sid":sid,"muted":false,"deafened":false,"speaking":true})
            );
            assert_eq!(
                receive_frame(&mut socket).await,
                json!({"type":"voice:state","sid":sid,"muted":true,"deafened":true,"speaking":false})
            );
        }
        assert_eq!(
            receive_frame(&mut socket).await,
            json!({"type":"voice:leave","sid":sid})
        );
        if matches!(scenario, Scenario::Cancelled) {
            acknowledge(&mut socket, sid, "room", nonce).await;
        }
        left.send(()).unwrap();
    };

    let desktop = async {
        let client = SpkClient::new(Url::parse(&origin).unwrap(), CookieStore::default()).unwrap();
        client
            .sign_in("test@example.test".into(), "fixture-password".into())
            .await
            .unwrap();
        let (events, updates) = async_channel::bounded(16);
        let socket_client = client.clone();
        let socket_worker =
            tokio::spawn(async move { socket_client.socket(&endpoint, events).await });
        let SocketEvent::Online(socket) = updates.recv().await.unwrap() else {
            panic!("SPK socket did not connect");
        };
        let initial = Intent {
            mute: Mute {
                deafened: controls,
                ..Mute::default()
            },
            ..Intent::default()
        };
        let mut call = Call::spawn("room".into(), socket, initial, |worker, ack, cancelled| {
            tokio::spawn(worker.run(
                client,
                ack,
                cancelled,
                async move |worker: &Worker, _: &mut Media, token: String| {
                    assert_eq!(token, "fixture-token");
                    match scenario {
                        Scenario::Cancelled => panic!("a cancelled call started media"),
                        Scenario::MediaFailure => Err(Issue::Media),
                        Scenario::MicrophoneFailure => {
                            worker.ready().await?;
                            worker.update(Update::Media(Arc::new(Report {
                                status: Status::ListenOnly,
                                devices: Arc::default(),
                                microphone: None,
                                speakers: None,
                                speaking: HashSet::new(),
                            })));
                            pending().await
                        }
                        Scenario::Controls => {
                            worker.ready().await?;
                            worker
                                .state(VoiceFlags {
                                    speaking: true,
                                    ..VoiceFlags::default()
                                })
                                .await?;
                            worker
                                .state(VoiceFlags {
                                    muted: true,
                                    deafened: true,
                                    speaking: false,
                                })
                                .await?;
                            Err(Issue::Disconnected)
                        }
                    }
                },
            ))
        });
        for _ in 0..3 {
            apply_acknowledgment(&mut call, &updates).await;
            assert!(call.acknowledgment.is_some());
        }

        match scenario {
            Scenario::Cancelled => {
                call.cancel();
                apply_acknowledgment(&mut call, &updates).await;
            }
            Scenario::MediaFailure => apply_acknowledgment(&mut call, &updates).await,
            Scenario::Controls => apply_acknowledgment(&mut call, &updates).await,
            Scenario::MicrophoneFailure => {
                apply_acknowledgment(&mut call, &updates).await;
                assert!(matches!(
                    next_update(Some(&call)).await,
                    Update::Media(report) if report.status == Status::ListenOnly
                ));
                call.cancel();
            }
        }
        let outcome = next_update(Some(&call)).await;
        match scenario {
            Scenario::MediaFailure => {
                assert!(matches!(outcome, Update::Finished(Err(Issue::Media))))
            }
            Scenario::Controls => assert!(matches!(
                outcome,
                Update::Finished(Err(Issue::Disconnected))
            )),
            _ => assert!(matches!(outcome, Update::Finished(Ok(())))),
        }
        departure.await.unwrap();
        call.stop().await;
        socket_worker.abort();
    };

    tokio::join!(server, desktop);
}

#[test]
fn deafen_restores_mute_and_unmuting_undeafens() {
    for muted in [false, true] {
        let mut mute = Mute {
            muted,
            deafened: false,
        };
        mute.toggle_deafen();
        assert!(mute.effective_muted());
        mute.toggle_deafen();
        assert_eq!(mute.effective_muted(), muted);
        mute.toggle_deafen();
        mute.toggle_mic();
        assert_eq!(mute, Mute::default());
    }
}

#[test]
fn holds_speaking_and_sends_only_the_latest_changed_state() {
    let now = Instant::now();
    let mut speaking = Speaking::default();
    speaking.update(HashSet::from(["alice".into()]), now);
    speaking.update(HashSet::new(), now);
    speaking.expire(now + Duration::from_millis(499));
    assert!(speaking.0.contains_key("alice"));
    speaking.expire(now + Duration::from_millis(500));
    assert!(speaking.0.is_empty());

    let mut presence = Presence::new(false);
    let talking = VoiceFlags {
        speaking: true,
        ..VoiceFlags::default()
    };
    presence.sent(talking, now);
    assert_eq!(presence.due(talking), None);
    assert_eq!(
        presence.due(VoiceFlags::default()),
        Some(now + Duration::from_millis(50))
    );
    let deafened = VoiceFlags {
        muted: true,
        deafened: true,
        speaking: false,
    };
    let due = presence.due(deafened).unwrap();
    assert_eq!(due, now + Duration::from_millis(50));
    presence.sent(deafened, due);
    assert_eq!(presence.due(deafened), None);
}

async fn acknowledge(
    socket: &mut WebSocketStream<TcpStream>,
    sid: &str,
    channel: &str,
    nonce: &str,
) {
    socket
        .send(Message::text(
            json!({
                "type":"voice:joined", "sid":sid, "channelId":channel, "nonce":nonce
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

async fn apply_acknowledgment(call: &mut Call, updates: &Receiver<SocketEvent>) {
    let SocketEvent::Frame(Frame::VoiceJoined {
        sid,
        channel_id,
        nonce,
    }) = updates.recv().await.unwrap()
    else {
        panic!("expected a voice acknowledgment");
    };
    call.acknowledge(&sid, &channel_id, &nonce);
}

async fn receive_frame(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let message = socket.next().await.unwrap().unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
