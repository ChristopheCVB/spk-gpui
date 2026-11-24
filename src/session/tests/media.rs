use livekit::{
    prelude::ConnectionQuality,
    webrtc::stats::{RemoteInboundRtpStats, RtcStats},
};

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

            peer.set_echo_reference_unavailable(true);
            let unavailable =
                wait_for_sidebar(&views, |sidebar| sidebar.echo_reference_unavailable()).await;
            assert!(
                unavailable
                    .voice_intent
                    .preferred
                    .processing
                    .echo_cancellation
            );
            assert_eq!(peer.exchange(74, -31), (Some(74), Some(-31)));
            assert!(!peer.echo_cancellation());
            peer.set_echo_reference_unavailable(false);
            wait_for_sidebar(&views, |sidebar| !sidebar.echo_reference_unavailable()).await;
            assert!(peer.echo_cancellation());

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
            for setting in [
                VoiceProcessingSetting::NoiseSuppression(NoiseSuppression::Enhanced),
                VoiceProcessingSetting::AutoGain(false),
                VoiceProcessingSetting::Sensitivity(80),
            ] {
                commands
                    .send(Command::SetVoiceProcessing(setting))
                    .await
                    .unwrap();
            }
            let tuned = wait_for_sidebar(&views, |sidebar| {
                sidebar.voice_intent.preferred.processing.sensitivity == 80
            })
            .await;
            let processing = tuned.voice_intent.preferred.processing;
            assert!(
                processing.echo_cancellation
                    && processing.noise_suppression == NoiseSuppression::Enhanced
                    && !processing.auto_gain
            );
            assert!(tuned.voice_intent.mute.effective_muted());
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
            commands
                .send(Command::SetVoiceProcessing(
                    VoiceProcessingSetting::EchoCancellation(false),
                ))
                .await
                .unwrap();
            let tuned_live = wait_for_sidebar(&views, |sidebar| {
                !sidebar.voice_intent.preferred.processing.echo_cancellation
            })
            .await;
            assert_eq!(
                tuned_live.voice_intent.retry_mic,
                tuned.voice_intent.retry_mic
            );
            assert_eq!(peer.exchange(74, -31), (Some(74), Some(-31)));

            peer.quality(ConnectionQuality::Good);
            assert_call_quality(&views, Some(ConnectionQuality::Good)).await;
            peer.report_stats(audio_stats(1_000_000, 2_000, 1_000, 90, 10, 0.003));
            let baseline =
                wait_for_diagnostics(&views, |report| report.jitter_ms == Some(30)).await;
            assert_eq!(baseline.upload_kbps, None);
            assert_eq!(baseline.download_kbps, None);
            assert_eq!(baseline.loss_percent, None);

            let mut stats = audio_stats(2_000_000, 6_000, 3_500, 108, 12, 0.007);
            let mut remote = RemoteInboundRtpStats::default();
            remote.stream.kind = "audio".into();
            remote.remote_inbound.local_id = "outgoing".into();
            remote.received.jitter = 0.012;
            remote.remote_inbound.round_trip_time = 0.052;
            remote.remote_inbound.round_trip_time_measurements = 1;
            stats
                .publisher_stats
                .push(RtcStats::RemoteInboundRtp(remote));
            peer.report_stats(stats);
            let measured =
                wait_for_diagnostics(&views, |report| report.upload_kbps.is_some()).await;
            assert_eq!(measured.jitter_ms, Some(120));
            assert_eq!(measured.round_trip_ms, Some(520));
            assert_eq!(measured.upload_kbps, Some(320));
            assert_eq!(measured.download_kbps, Some(200));
            assert_eq!(measured.loss_percent, Some(100));
            assert_eq!(measured.quality, Some(ConnectionQuality::Good));
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
            peer.report_stats(audio_stats(3_000_000, 8_000, 5_500, 126, 13, 0.004));
            let recovered =
                wait_for_diagnostics(&views, |report| report.jitter_ms == Some(40)).await;
            assert_eq!(recovered.upload_kbps, None);
            assert_eq!(recovered.download_kbps, None);
            assert_eq!(recovered.loss_percent, None);

            peer.reconnecting();
            wait_for_route(
                &views,
                VoiceStatus::Reconnecting,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;
            let resumptions = peer.microphone_resumptions();
            let (scan_started, finish_scan) = peer.reconnect_with_paused_scan();
            scan_started.await.unwrap();
            commands.send(Command::ToggleMute).await.unwrap();
            wait_for_sidebar(&views, |sidebar| {
                sidebar.voice_intent.mute.effective_muted()
            })
            .await;
            finish_scan.send(()).unwrap();
            wait_for_route(&views, VoiceStatus::Connected, None, "preferred-speakers").await;
            assert_eq!(peer.microphone_resumptions(), resumptions);
            commands.send(Command::ToggleMute).await.unwrap();
            wait_for_route(
                &views,
                VoiceStatus::Connected,
                Some("preferred-mic"),
                "preferred-speakers",
            )
            .await;

            peer.disconnect();
            wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .voice
                    .as_ref()
                    .is_some_and(|voice| voice.status == VoiceStatus::Joining)
            })
            .await;
            let rejoined = arrivals.recv().await.unwrap();
            peer.assert_released();
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

#[tokio::test]
async fn missing_remote_microphone_connects_locally_until_the_peer_mutes() {
    let (peers, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let (mute, muted) = oneshot::channel();
    let end = voice_session(
        start_call(peers),
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let join = accept_voice_join(&requests, &mut socket, "first").await;
            acknowledge_voice(&mut socket, &join).await;
            remote_presence(&mut socket, "missing", false).await;
            remote_presence(&mut socket, "muted", true).await;
            muted.await.unwrap();
            remote_presence(&mut socket, "missing", true).await;
            mirror_presence_until_leave(&mut socket, &join, "first", &snapshots).await;
        },
        async |commands, views| {
            request_first_voice(&commands, &views).await;
            let _peer = arrivals.recv().await.unwrap();
            let waiting = wait_for_sidebar(&views, |sidebar| {
                sidebar.participants("first").len() == 2
                    && sidebar
                        .voice
                        .as_ref()
                        .is_some_and(|voice| voice.is_participating())
            })
            .await;
            assert!(
                waiting
                    .participants("first")
                    .iter()
                    .all(|peer| !peer.microphone_connecting)
            );

            let connecting = wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .participants("first")
                    .iter()
                    .any(|peer| peer.id == "missing" && peer.microphone_connecting)
            })
            .await;
            assert!(connecting.participants("first").iter().any(|peer| {
                peer.id == "muted" && peer.flags.muted && !peer.microphone_connecting
            }));
            assert!(connecting.voice.as_ref().unwrap().is_participating());

            mute.send(()).unwrap();
            wait_for_sidebar(&views, |sidebar| {
                sidebar.participants("first").iter().any(|peer| {
                    peer.id == "missing" && peer.flags.muted && !peer.microphone_connecting
                })
            })
            .await;
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn isolated_stuck_subscription_repairs_once_without_rejoining() {
    let (peers, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let end = voice_session(
        start_call(peers),
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let join = accept_voice_join(&requests, &mut socket, "first").await;
            acknowledge_voice(&mut socket, &join).await;
            for (id, muted) in [
                ("stalled", false),
                ("healthy", false),
                ("muted", true),
                ("unknown", false),
            ] {
                remote_presence(&mut socket, id, muted).await;
            }
            mirror_presence_until_leave(&mut socket, &join, "first", &snapshots).await;
            assert!(requests.is_empty());
        },
        async |commands, views| {
            request_first_voice(&commands, &views).await;
            let peer = Arc::new(arrivals.recv().await.unwrap());
            peer.detach_remote_microphone("stalled");
            peer.set_remote_microphone_packets("muted", Some(1));
            peer.set_remote_microphone_packets("unknown", None);
            let keep_packets_flowing = |peer: Arc<Peer>, id| {
                tokio::spawn(async move {
                    for packets in 1.. {
                        peer.set_remote_microphone_packets(id, Some(packets));
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
            };
            let healthy = keep_packets_flowing(peer.clone(), "healthy");

            let connecting = wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .participants("first")
                    .iter()
                    .any(|peer| peer.id == "stalled" && peer.microphone_connecting)
            })
            .await;
            assert_eq!(
                connecting
                    .participants("first")
                    .iter()
                    .filter(|peer| peer.microphone_connecting)
                    .map(|peer| peer.id)
                    .collect::<Vec<_>>(),
                ["stalled"]
            );
            assert!(peer.microphone_repairs().is_empty());

            while peer.microphone_repairs().is_empty() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::time::sleep(Duration::from_millis(1100)).await;
            assert_eq!(peer.microphone_repairs(), ["stalled"]);

            let recovering = keep_packets_flowing(peer.clone(), "stalled");
            let recovered = wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .participants("first")
                    .iter()
                    .all(|peer| !peer.microphone_connecting)
            })
            .await;
            assert!(recovered.voice.as_ref().unwrap().is_participating());
            assert_eq!(peer.exchange(0, 25).1, Some(25));
            assert_eq!(peer.microphone_repairs(), ["stalled"]);
            healthy.abort();
            recovering.abort();

            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn widespread_remote_microphone_stalls_rejoin_after_targeted_repairs_with_a_cooldown() {
    let (peers, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let end = voice_session_with_limit(
        watch::channel(true).1,
        start_call(peers),
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let first = accept_voice_join(&requests, &mut socket, "first").await;
            acknowledge_voice(&mut socket, &first).await;
            for id in ["detached", "flat", "absent"] {
                remote_presence(&mut socket, id, false).await;
            }
            mirror_presence_until_leave(&mut socket, &first, "first", &snapshots).await;

            let retry = accept_voice_join(&requests, &mut socket, "first").await;
            assert_ne!(retry["sid"], first["sid"]);
            acknowledge_voice(&mut socket, &retry).await;
            mirror_presence_until_leave(&mut socket, &retry, "first", &snapshots).await;
            assert!(requests.is_empty());
        },
        async |commands, views| {
            request_first_voice(&commands, &views).await;
            let peer = arrivals.recv().await.unwrap();
            commands
                .send(Command::ChooseDevice(
                    DeviceKind::Speakers,
                    Some(Peer::preferred_speakers()),
                ))
                .await
                .unwrap();
            wait_for_route(&views, VoiceStatus::ListenOnly, None, "preferred-speakers").await;
            let stalled_at = Instant::now();
            peer.detach_remote_microphone("detached");
            peer.set_remote_microphone_packets("flat", Some(42));
            let rejoined = arrivals.recv().await.unwrap();
            assert!(stalled_at.elapsed() >= Duration::from_secs(20));
            let mut repairs = peer.microphone_repairs();
            repairs.sort();
            assert_eq!(repairs, ["detached", "flat"]);
            peer.assert_released();

            wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .voice
                    .as_ref()
                    .is_some_and(|voice| voice.status == VoiceStatus::Joining)
            })
            .await;
            wait_for_route(&views, VoiceStatus::ListenOnly, None, "preferred-speakers").await;
            wait_for_flags(&views, true, false, false).await;
            assert_eq!(rejoined.exchange(0, 25).1, Some(25));

            rejoined.detach_remote_microphone("detached");
            rejoined.detach_remote_microphone("flat");
            assert_no_automatic_rejoin(&views, Duration::from_secs(22)).await;
            let mut repairs = rejoined.microphone_repairs();
            repairs.sort();
            assert_eq!(repairs, ["detached", "flat"]);
            commands.send(Command::LeaveVoice).await.unwrap();
            wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
        },
        Duration::from_secs(50),
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn widespread_rejoin_requires_all_eligible_microphones_to_stall_and_healthy_output() {
    #[derive(Clone, Copy)]
    enum Control {
        Healthy,
        Unknown,
        Muted,
        Absent,
        LateArrival,
        FailedOutput,
        RestoredOutput,
    }
    use Control::*;

    futures::future::join_all(
        [
            Healthy,
            Unknown,
            Muted,
            Absent,
            LateArrival,
            FailedOutput,
            RestoredOutput,
        ]
        .into_iter()
        .map(|control| async move {
            let (peers, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
            let end = voice_session_with_limit(
                watch::channel(true).1,
                start_call(peers),
                async |listener, requests, snapshots| {
                    let mut socket = accept_async(listener.accept().await.unwrap().0)
                        .await
                        .unwrap();
                    let join = accept_voice_join(&requests, &mut socket, "first").await;
                    acknowledge_voice(&mut socket, &join).await;
                    remote_presence(&mut socket, "stalled", false).await;
                    remote_presence(&mut socket, "other", matches!(control, Muted)).await;
                    if matches!(control, Healthy | Unknown) {
                        remote_presence(&mut socket, "also-stalled", false).await;
                    }
                    mirror_presence_until_leave(&mut socket, &join, "first", &snapshots).await;
                    assert!(requests.is_empty());
                },
                async |commands, views| {
                    request_first_voice(&commands, &views).await;
                    let peer = Arc::new(arrivals.recv().await.unwrap());
                    wait_for_route(&views, VoiceStatus::ListenOnly, None, "default-speakers").await;
                    peer.detach_remote_microphone("stalled");
                    peer.detach_remote_microphone("also-stalled");
                    let flowing = match control {
                        Healthy => {
                            let peer = peer.clone();
                            Some(tokio::spawn(async move {
                                for packets in 1.. {
                                    peer.set_remote_microphone_packets("other", Some(packets));
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }))
                        }
                        Unknown => {
                            peer.set_remote_microphone_packets("other", None);
                            None
                        }
                        Muted => {
                            peer.detach_remote_microphone("other");
                            None
                        }
                        FailedOutput | RestoredOutput => {
                            peer.set_voice_output_failed(true);
                            peer.detach_remote_microphone("other");
                            None
                        }
                        Absent | LateArrival => None,
                    };
                    let later = async {
                        tokio::time::sleep(Duration::from_secs(12)).await;
                        if matches!(control, LateArrival) {
                            peer.detach_remote_microphone("other");
                        }
                        if matches!(control, RestoredOutput) {
                            peer.set_voice_output_failed(false);
                        }
                    };
                    tokio::join!(
                        later,
                        assert_no_automatic_rejoin(&views, Duration::from_secs(23)),
                    );
                    if let Some(flowing) = flowing {
                        flowing.abort();
                    }
                    commands.send(Command::LeaveVoice).await.unwrap();
                    wait_for_sidebar(&views, |sidebar| sidebar.voice.is_none()).await;
                },
                Duration::from_secs(27),
            )
            .await;
            assert!(matches!(end, SessionEnd::Closed));
        }),
    )
    .await;
}

async fn assert_no_automatic_rejoin(views: &Receiver<View>, duration: Duration) {
    assert!(
        timeout(
            duration,
            wait_for_sidebar(views, |sidebar| {
                sidebar
                    .voice
                    .as_ref()
                    .is_none_or(|voice| !voice.is_participating())
            }),
        )
        .await
        .is_err(),
        "the Voice Session should stay connected"
    );
}

async fn remote_presence(socket: &mut WebSocketStream<TcpStream>, id: &str, muted: bool) {
    socket
        .send(Message::text(
            value!({"type":"voice:presence","userId":id,"channelId":"first","muted":muted})
                .to_string(),
        ))
        .await
        .unwrap();
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
                        radio_listening: None,
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
                    radio_listening: None,
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
        value!({"type":"voice:state","sid":sid,"muted":false,"deafened":false,"speaking":false,"cam":false,"screen":false,"watching":[]}),
    );
    publish_media_presence(socket, snapshots, "first", VoiceFlags::default()).await;
}

fn audio_stats(
    timestamp: i64,
    sent: u64,
    received: u64,
    packets: u64,
    lost: i64,
    jitter: f64,
) -> livekit::rtc_engine::SessionStats {
    use livekit::webrtc::stats::{InboundRtpStats, OutboundRtpStats};

    let mut inbound = InboundRtpStats::default();
    inbound.rtc.id = "incoming".into();
    inbound.rtc.timestamp = timestamp;
    inbound.stream.kind = "audio".into();
    inbound.inbound.bytes_received = received;
    inbound.received.packets_received = packets;
    inbound.received.packets_lost = lost;
    inbound.received.jitter = jitter;

    let mut outbound = OutboundRtpStats::default();
    outbound.rtc.id = "outgoing".into();
    outbound.rtc.timestamp = timestamp;
    outbound.stream.kind = "audio".into();
    outbound.sent.bytes_sent = sent;

    livekit::rtc_engine::SessionStats {
        // A single peer connection reports both directions through publisher stats.
        publisher_stats: vec![
            RtcStats::InboundRtp(inbound),
            RtcStats::OutboundRtp(outbound),
        ],
        subscriber_stats: Vec::new(),
    }
}

async fn wait_for_diagnostics(
    views: &Receiver<View>,
    matches: impl Fn(&crate::voice::Diagnostics) -> bool,
) -> crate::voice::Diagnostics {
    let sidebar = wait_for_sidebar(views, |sidebar| {
        sidebar
            .voice
            .as_ref()
            .and_then(|voice| voice.media.as_ref())
            .is_some_and(|media| matches(&media.diagnostics))
    })
    .await;
    sidebar.voice.unwrap().media.unwrap().diagnostics
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
