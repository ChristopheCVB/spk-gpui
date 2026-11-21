use livekit::prelude::ConnectionQuality;

use super::*;
use crate::voice::fixture::{Peer, start_call};

#[tokio::test]
async fn desktop_voice_routes_samples_and_synchronizes_presence() {
    let (peers, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let (allow_ack, wait_to_ack) = oneshot::channel();
    let end = voice_session(
        start_call(peers),
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let first = accept_voice_join(&requests, &mut socket, "first").await;
            wait_to_ack.await.unwrap();
            acknowledge_voice(&mut socket, &first).await;
            mirror_presence_until_leave(&mut socket, &first, "first", &snapshots).await;

            let retry = accept_voice_join(&requests, &mut socket, "first").await;
            assert_ne!(retry["sid"], first["sid"]);
            assert_ne!(retry["nonce"], first["nonce"]);
            acknowledge_voice(&mut socket, &retry).await;
            mirror_presence_until_leave(&mut socket, &retry, "first", &snapshots).await;
        },
        async |commands, views| {
            request_first_voice(&commands, &views).await;
            let pending = wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .voice
                    .as_ref()
                    .is_some_and(|voice| voice.status == VoiceStatus::Joining)
            })
            .await;
            assert!(pending.has_voice_channel("second"));
            assert!(matches!(
                arrivals.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
            assert!(pending.participants("first").is_empty());
            allow_ack.send(()).unwrap();

            let peer = arrivals.recv().await.unwrap();
            let first =
                wait_for_route(&views, VoiceStatus::ListenOnly, None, "default-speakers").await;
            assert_eq!(
                first.voice.unwrap().media.unwrap().diagnostics,
                Default::default()
            );
            wait_for_flags(&views, true, false, false).await;
            assert_eq!(peer.exchange(71, -28), (None, Some(-28)));

            peer.allow_microphone();
            commands.send(Command::RetryMicrophone).await.unwrap();
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("default-mic"),
                "default-speakers",
            )
            .await;
            assert_eq!(peer.exchange(71, -28), (Some(71), Some(-28)));

            peer.speaking(&["alice", "bob"]);
            wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .voice
                    .as_ref()
                    .and_then(|voice| voice.media.as_ref())
                    .is_some_and(|media| {
                        media.speaking == HashSet::from(["alice".into(), "bob".into()])
                    })
            })
            .await;
            wait_for_flags(&views, false, false, true).await;

            commands
                .send(Command::ChooseDevice(
                    DeviceKind::Speakers,
                    Some(Peer::preferred_speakers()),
                ))
                .await
                .unwrap();
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("default-mic"),
                "preferred-speakers",
            )
            .await;
            assert_eq!(peer.exchange(71, -28), (Some(71), Some(-28)));
            commands
                .send(Command::ChooseDevice(
                    DeviceKind::Microphone,
                    Some(Peer::preferred_mic()),
                ))
                .await
                .unwrap();
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;
            assert_eq!(peer.exchange(72, -29), (Some(72), Some(-29)));
            commands
                .send(Command::ChooseDevice(DeviceKind::Microphone, None))
                .await
                .unwrap();
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("default-mic"),
                "preferred-speakers",
            )
            .await;
            assert_eq!(peer.exchange(73, -30), (Some(73), Some(-30)));
            commands
                .send(Command::ChooseDevice(
                    DeviceKind::Microphone,
                    Some(Peer::preferred_mic()),
                ))
                .await
                .unwrap();
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;

            peer.set_hardware_format(Some((24_000, 1)));
            assert_eq!(peer.exchange(74, -31), (Some(74), None));
            peer.wait_for_playout((24_000, 1)).await;
            assert_eq!(peer.exchange(74, -31), (Some(74), Some(-31)));

            peer.set_hardware_format(None);
            peer.plug_preferred_devices(false);
            let fallback = wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("default-mic"),
                "default-speakers",
            )
            .await;
            assert_eq!(
                fallback.voice_intent.preferred.microphone,
                Some(Peer::preferred_mic())
            );
            assert_eq!(
                fallback.voice_intent.preferred.speakers,
                Some(Peer::preferred_speakers())
            );
            assert_eq!(peer.exchange(74, -31), (Some(74), Some(-31)));

            peer.plug_preferred_devices(true);
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;
            assert_eq!(peer.exchange(74, -31), (Some(74), Some(-31)));

            commands.send(Command::ToggleMute).await.unwrap();
            wait_for_flags(&views, true, false, false).await;
            assert_eq!(peer.exchange(74, -31), (None, Some(-31)));
            for was_muted in [true, false] {
                commands.send(Command::ToggleDeafen).await.unwrap();
                wait_for_flags(&views, true, true, false).await;
                assert_eq!(peer.exchange(74, -31), (None, None));
                commands.send(Command::ToggleDeafen).await.unwrap();
                wait_for_flags(&views, was_muted, false, false).await;
                assert_eq!(
                    peer.exchange(74, -31),
                    ((!was_muted).then_some(74), Some(-31))
                );
                if was_muted {
                    commands.send(Command::ToggleMute).await.unwrap();
                    wait_for_flags(&views, false, false, false).await;
                }
            }

            peer.quality(ConnectionQuality::Good);
            assert_call_quality(&views, Some(ConnectionQuality::Good)).await;
            peer.reconnecting();
            let reconnecting = wait_for_route(
                &views,
                VoiceStatus::Reconnecting,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;
            wait_for_flags(&views, true, false, false).await;
            assert_eq!(
                reconnecting.voice.unwrap().media.unwrap().diagnostics,
                Default::default()
            );
            peer.quality(ConnectionQuality::Lost);
            peer.set_hardware_format(Some((24_000, 1)));
            assert_eq!(peer.exchange(75, -32), (None, None));
            peer.reconnected();
            let reconnected = wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;
            wait_for_flags(&views, false, false, false).await;
            assert_eq!(peer.exchange(75, -32), (Some(75), Some(-32)));
            assert_eq!(
                reconnected.voice.unwrap().media.unwrap().diagnostics,
                Default::default()
            );
            peer.quality(ConnectionQuality::Excellent);
            assert_call_quality(&views, Some(ConnectionQuality::Excellent)).await;

            peer.disconnect();
            let terminal = wait_for_sidebar(&views, |sidebar| {
                sidebar.rejoin_channel().is_some() && sidebar.participants("first").is_empty()
            })
            .await;
            peer.assert_released();
            commands
                .send(Command::JoinVoice(
                    terminal.rejoin_channel().unwrap().into(),
                ))
                .await
                .unwrap();
            let rejoined = arrivals.recv().await.unwrap();
            wait_for_route(&views, VoiceStatus::ListenOnly, None, "preferred-speakers").await;
            wait_for_flags(&views, true, false, false).await;
            assert_eq!(rejoined.exchange(71, -28), (None, Some(-28)));
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| {
                sidebar.voice.is_none() && sidebar.participants("first").is_empty()
            })
            .await;
            rejoined.assert_released();
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

async fn wait_for_route(
    views: &Receiver<View>,
    status: VoiceStatus,
    microphone: Option<&str>,
    speakers: &str,
) -> Box<Sidebar> {
    wait_for_sidebar(views, |sidebar| {
        sidebar.voice.as_ref().is_some_and(|voice| {
            voice.status == status
                && voice.media.as_ref().is_some_and(|media| {
                    media.microphone.as_ref().map(|device| device.id.as_str()) == microphone
                        && media
                            .speakers
                            .as_ref()
                            .is_some_and(|device| device.id == speakers)
                })
        })
    })
    .await
}

async fn wait_for_flags(views: &Receiver<View>, muted: bool, deafened: bool, speaking: bool) {
    wait_for_sidebar(views, |sidebar| {
        sidebar.participants("first").iter().any(|participant| {
            participant.id == "alice"
                && participant.flags
                    == VoiceFlags {
                        muted,
                        deafened,
                        speaking,
                    }
        })
    })
    .await;
}

async fn mirror_presence_until_leave(
    socket: &mut WebSocketStream<TcpStream>,
    join: &Value,
    channel: &'static str,
    snapshots: &watch::Sender<SidebarSnapshot>,
) {
    let mut ready = false;
    loop {
        let frame = receive_voice_frame(socket).await;
        assert_eq!(frame["sid"], join["sid"]);
        let flags = match frame["type"].as_str().unwrap() {
            "voice:ready" => {
                if ready {
                    continue;
                }
                ready = true;
                VoiceFlags {
                    muted: true,
                    deafened: join["deafened"].as_bool().unwrap(),
                    speaking: false,
                }
            }
            "voice:state" => {
                assert!(ready);
                serde_json::from_value(frame).unwrap()
            }
            "voice:leave" => {
                snapshots.send_replace(SidebarSnapshot::Initial);
                broadcast_presence(socket, Value::Null, VoiceFlags::default()).await;
                return;
            }
            kind => panic!("unexpected voice frame: {kind}"),
        };
        publish_media_presence(socket, snapshots, channel, flags).await;
    }
}

async fn publish_media_presence(
    socket: &mut WebSocketStream<TcpStream>,
    snapshots: &watch::Sender<SidebarSnapshot>,
    channel: &'static str,
    flags: VoiceFlags,
) {
    snapshots.send_replace(SidebarSnapshot::MediaPresence { channel, flags });
    broadcast_presence(socket, value!(channel), flags).await;
}

async fn broadcast_presence(
    socket: &mut WebSocketStream<TcpStream>,
    channel: Value,
    flags: VoiceFlags,
) {
    socket
        .send(Message::text(
            value!({
                "type":"voice:presence","userId":"alice","channelId":channel,
                "muted":flags.muted,"deafened":flags.deafened,"speaking":flags.speaking,
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallEnd {
    Cancel,
    Switch,
    Move,
    Disconnect,
    Replaced,
    SignOut,
    Exit,
}

#[tokio::test]
async fn desktop_voice_teardown_releases_media_and_owned_presence() {
    for ending in [
        CallEnd::Cancel,
        CallEnd::Switch,
        CallEnd::Move,
        CallEnd::Disconnect,
        CallEnd::Replaced,
        CallEnd::SignOut,
        CallEnd::Exit,
    ] {
        let (peers, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (join_received, wait_for_join) = oneshot::channel();
        let (end_call, wait_for_end) = oneshot::channel();
        let end = voice_session(
            start_call(peers),
            async |listener, requests, snapshots| {
                let mut socket = accept_async(listener.accept().await.unwrap().0)
                    .await
                    .unwrap();
                let first = accept_voice_join(&requests, &mut socket, "first").await;
                join_received.send(()).unwrap();
                let sid = &first["sid"];
                if ending != CallEnd::Cancel {
                    acknowledge_voice(&mut socket, &first).await;
                    serve_active_media_presence(&mut socket, sid, &snapshots).await;
                }
                wait_for_end.await.unwrap();
                match ending {
                    CallEnd::Move => send_voice_move(&mut socket, sid, "second").await,
                    CallEnd::Disconnect => {
                        let frame = value!({"type":"voice:disconnect", "sid":sid});
                        socket.send(Message::text(frame.to_string())).await.unwrap();
                    }
                    CallEnd::Replaced => {
                        snapshots.send_replace(SidebarSnapshot::VoiceReplaced);
                        broadcast_presence(&mut socket, value!("second"), VoiceFlags::default())
                            .await;
                        let frame = value!({"type":"voice:kicked", "sid":"replacement"});
                        socket.send(Message::text(frame.to_string())).await.unwrap();
                    }
                    _ => {}
                }
                assert_eq!(
                    receive_voice_frame(&mut socket).await,
                    value!({"type":"voice:leave","sid":sid})
                );
                match ending {
                    CallEnd::Cancel => {
                        acknowledge_voice(&mut socket, &first).await;
                        socket
                            .send(Message::text(
                                r#"{"type":"voice:presence","userId":"bob","channelId":"first"}"#,
                            ))
                            .await
                            .unwrap();
                    }
                    CallEnd::Replaced | CallEnd::SignOut | CallEnd::Exit => {}
                    _ => {
                        snapshots.send_replace(SidebarSnapshot::Initial);
                        broadcast_presence(&mut socket, Value::Null, VoiceFlags::default()).await;
                    }
                }
                if matches!(ending, CallEnd::Move | CallEnd::Switch) {
                    let second = accept_voice_join(&requests, &mut socket, "second").await;
                    assert_ne!(second["sid"], first["sid"]);
                    acknowledge_voice(&mut socket, &second).await;
                    mirror_presence_until_leave(&mut socket, &second, "second", &snapshots).await;
                }
            },
            async |commands, views| {
                request_first_voice(&commands, &views).await;
                wait_for_join.await.unwrap();
                if ending == CallEnd::Cancel {
                    end_call.send(()).unwrap();
                    commands.send(Command::LeaveVoice).await.unwrap();
                    wait_for_sidebar(&views, |sidebar| {
                        sidebar.voice.is_none()
                            && sidebar
                                .participants("first")
                                .iter()
                                .any(|participant| participant.id == "bob")
                    })
                    .await;
                    assert!(arrivals.try_recv().is_err());
                    return;
                }
                let peer = arrivals.recv().await.unwrap();
                wait_for_flags(&views, true, false, false).await;
                peer.allow_microphone();
                commands.send(Command::RetryMicrophone).await.unwrap();
                wait_for_flags(&views, false, false, false).await;
                assert_eq!(peer.exchange(71, -28), (Some(71), Some(-28)));
                end_call.send(()).unwrap();

                match ending {
                    CallEnd::Exit => {
                        commands.close();
                        while views.recv().await.is_ok() {}
                    }
                    CallEnd::SignOut => {
                        commands.send(Command::SignOut).await.unwrap();
                        while views.recv().await.is_ok() {}
                    }
                    CallEnd::Switch | CallEnd::Move => {
                        if ending == CallEnd::Switch {
                            commands
                                .send(Command::JoinVoice("second".into()))
                                .await
                                .unwrap();
                        }
                        let second = arrivals.recv().await.unwrap();
                        peer.assert_released();
                        let sidebar = wait_for_sidebar(&views, |sidebar| {
                            sidebar.voice.as_ref().is_some_and(|voice| {
                                voice.channel_id == "second"
                                    && voice.status == VoiceStatus::ListenOnly
                            }) && sidebar
                                .participants("second")
                                .iter()
                                .any(|participant| participant.id == "alice")
                        })
                        .await;
                        assert!(sidebar.participants("first").is_empty());
                        assert_eq!(second.exchange(71, -28), (None, Some(-28)));
                        commands.send(Command::LeaveVoice).await.unwrap();
                        wait_for_sidebar(&views, |sidebar| {
                            sidebar.voice.is_none() && sidebar.participants("second").is_empty()
                        })
                        .await;
                        second.assert_released();
                    }
                    CallEnd::Disconnect | CallEnd::Replaced => {
                        let sidebar = wait_for_sidebar(&views, |sidebar| {
                            sidebar.voice.is_none() && sidebar.participants("first").is_empty()
                        })
                        .await;
                        assert!(sidebar.voice_notice.is_some());
                        assert_eq!(
                            sidebar
                                .participants("second")
                                .iter()
                                .any(|participant| participant.id == "alice"),
                            ending == CallEnd::Replaced,
                        );
                    }
                    CallEnd::Cancel => unreachable!(),
                }
                peer.assert_released();
            },
        )
        .await;
        assert!(match ending {
            CallEnd::SignOut => matches!(end, SessionEnd::SignOut),
            _ => matches!(end, SessionEnd::Closed),
        });
    }
}

async fn serve_active_media_presence(
    socket: &mut WebSocketStream<TcpStream>,
    sid: &Value,
    snapshots: &watch::Sender<SidebarSnapshot>,
) {
    assert_eq!(
        receive_voice_frame(socket).await,
        value!({"type":"voice:ready","sid":sid})
    );
    publish_media_presence(
        socket,
        snapshots,
        "first",
        VoiceFlags {
            muted: true,
            ..VoiceFlags::default()
        },
    )
    .await;
    assert_eq!(
        receive_voice_frame(socket).await,
        value!({"type":"voice:state","sid":sid,"muted":false,"deafened":false,"speaking":false}),
    );
    publish_media_presence(socket, snapshots, "first", VoiceFlags::default()).await;
}

async fn assert_call_quality(views: &Receiver<View>, quality: Option<ConnectionQuality>) {
    let snapshot = wait_for_sidebar(views, |sidebar| {
        sidebar
            .voice
            .as_ref()
            .and_then(|voice| voice.media.as_ref())
            .is_some_and(|media| media.diagnostics.quality == quality)
    })
    .await;
    let diagnostics = snapshot.voice.unwrap().media.unwrap().diagnostics;
    assert_eq!(diagnostics.jitter_ms, None);
    assert_eq!(diagnostics.loss_percent, None);
}
