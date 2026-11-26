use super::*;

const STATION: &str = "23456789-1111-4ccc-8aaa-123456789abc";
const UNAVAILABLE_STATION: &str = "23456789-1111-4ccc-8aaa-123456789abe";

fn station(name: &str) -> Value {
    value!({
        "id": STATION, "name": name, "country": "FR", "codec": "MP3",
        "url": "https://127.0.0.1/station"
    })
}

pub(super) async fn respond(line: &str, request: Request, favorite: &mut bool) {
    assert_eq!(request.header("cookie"), Some("session=fixture"));
    let details = value!({
        "id": STATION, "name": "Jazz & Soul", "country": "FR", "codec": "MP3", "available": true,
    });
    let body = match line {
        "GET /api/radio/stations?q=Jazz+%26+Soul HTTP/1.1" => value!([station("Jazz & Soul")]),
        "GET /api/radio/favorites HTTP/1.1" => {
            if *favorite {
                value!([details])
            } else {
                value!([])
            }
        }
        line if line == format!("PUT /api/radio/favorites/{UNAVAILABLE_STATION} HTTP/1.1") => {
            request.respond_raw("404 Not Found", &[], b"{}").await;
            return;
        }
        line if line == format!("PUT /api/radio/favorites/{STATION} HTTP/1.1") => {
            *favorite = true;
            details
        }
        line if line == format!("DELETE /api/radio/favorites/{STATION} HTTP/1.1") => {
            *favorite = false;
            value!({"ok":true})
        }
        _ => panic!("unexpected request: {line}"),
    };
    request.respond(&[], &body.to_string()).await;
}

#[tokio::test]
async fn channel_radio_uses_shared_station_and_local_controls() {
    let end =
        voice_session(
            Call::start_for_test,
            async |sockets, requests, _| {
                let mut socket = accept_async(sockets.accept().await.unwrap().0)
                    .await
                    .unwrap();
                socket.send(Message::text(value!({
                "type":"radio:snapshot", "channels":[
                    {"channelId":"second","station":{"id":STATION,"name":"Peer station"}}
                ],
            }).to_string())).await.unwrap();
                let join = establish_voice(&requests, &mut socket).await;
                let sid = &join["sid"];
                assert_eq!(
                    receive_voice_frame(&mut socket).await,
                    value!({"type":"voice:radio:set","sid": sid,"stationId":STATION})
                );
                for (session, channel, revision, name) in [
                    (sid.clone(), "first", 3, "Shared jazz"),
                    (sid.clone(), "first", 2, "Old station"),
                    (sid.clone(), "second", 4, "Other channel"),
                    (value!("other-session"), "first", 5, "Other session"),
                ] {
                    socket
                        .send(Message::text(
                            value!({
                                "type":"voice:radio:state","sid":session,"channelId":channel,
                                "revision":revision,"station":station(name),
                            })
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                }
                assert_eq!(
                    receive_voice_frame(&mut socket).await,
                    value!({"type":"voice:radio:stop","sid":sid})
                );
                socket
                    .send(Message::text(
                        value!({
                            "type":"voice:radio:error","sid":sid,"channelId":"first",
                            "error":"Could not stop Channel Radio.",
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                let mut updates = [
                    receive_voice_frame(&mut socket).await,
                    receive_voice_frame(&mut socket).await,
                ];
                updates.sort_by(|a, b| a["type"].as_str().cmp(&b["type"].as_str()));
                assert_eq!(
                    updates,
                    [
                        value!({"type":"voice:radio:stop","sid":sid}),
                        voice_state(sid, true, true),
                    ]
                );
                socket
                    .send(Message::text(
                        value!({
                            "type":"voice:radio:state","sid":sid,"channelId":"first",
                            "revision":4,"station":null,
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    receive_voice_frame(&mut socket).await,
                    value!({"type":"voice:leave","sid":sid})
                );
            },
            async |commands, views| {
                let outside = wait_for_sidebar(&views, |s| {
                    s.channel_radio
                        .get("second")
                        .is_some_and(|name| name == "Peer station")
                })
                .await;
                assert!(!outside.radio.can_control_channel);
                assert_eq!(outside.radio.local_action(), None);
                join_test_voice(&commands, &views).await;
                commands.send(Command::RadioFavorites).await.unwrap();
                wait_for_sidebar(&views, |s| s.radio.favorites.is_some()).await;
                commands
                    .send(Command::SearchRadio(" Jazz & Soul ".into()))
                    .await
                    .unwrap();
                wait_for_sidebar(&views, |s| {
                    s.radio
                        .stations
                        .as_ref()
                        .is_some_and(|stations| stations[0].id == STATION)
                })
                .await;
                commands
                    .send(Command::SetRadioFavorite {
                        id: UNAVAILABLE_STATION.into(),
                        saved: true,
                    })
                    .await
                    .unwrap();
                let failed_favorite = wait_for_sidebar(&views, |s| {
                    s.action_results
                        == [ActionResult::error(
                            "Could not update radio Favorites. Try again.",
                        )]
                })
                .await;
                assert!(
                    failed_favorite
                        .radio
                        .favorites
                        .as_ref()
                        .is_some_and(Vec::is_empty)
                );
                assert!(failed_favorite.radio.directory_error.is_none());
                commands
                    .send(Command::SetRadioFavorite {
                        id: STATION.into(),
                        saved: true,
                    })
                    .await
                    .unwrap();
                wait_for_sidebar(&views, |s| {
                    s.radio
                        .favorites
                        .as_ref()
                        .is_some_and(|favorites| favorites.len() == 1)
                })
                .await;
                commands
                    .send(Command::SelectRadio(STATION.into()))
                    .await
                    .unwrap();
                let unavailable =
                    wait_for_sidebar(&views, |s| s.radio.playback == RadioPlayback::Unavailable)
                        .await;
                assert!(unavailable.radio.can_control_channel);
                assert_eq!(unavailable.radio.local_action(), Some(RadioAction::Retry));
                commands
                    .send(unavailable.radio.local_action().unwrap().command())
                    .await
                    .unwrap();
                let retrying =
                    wait_for_sidebar(&views, |s| s.radio.playback == RadioPlayback::Connecting)
                        .await;
                assert_eq!(retrying.radio.local_action(), Some(RadioAction::Pause));
                wait_for_sidebar(&views, |s| s.radio.playback == RadioPlayback::Unavailable).await;
                commands.send(Command::StopRadio).await.unwrap();
                let failed_stop = wait_for_sidebar(&views, |s| {
                    s.action_results == [ActionResult::error("Could not stop Channel Radio.")]
                })
                .await;
                assert_eq!(failed_stop.radio.playback, RadioPlayback::Unavailable);
                assert_eq!(failed_stop.radio.local_action(), Some(RadioAction::Retry));
                assert_eq!(
                    failed_stop.radio.station.as_ref().unwrap().name,
                    "Shared jazz"
                );
                commands.send(Command::PauseRadio).await.unwrap();
                commands.send(Command::SetRadioVolume(25)).await.unwrap();
                commands.send(Command::ToggleDeafen).await.unwrap();
                let paused = wait_for_sidebar(&views, |s| {
                    s.radio.volume == 25 && s.voice_intent.mute.deafened
                })
                .await;
                assert_eq!(paused.radio.station.as_ref().unwrap().name, "Shared jazz");
                assert_eq!(paused.radio.local_action(), Some(RadioAction::Resume));
                commands
                    .send(paused.radio.local_action().unwrap().command())
                    .await
                    .unwrap();
                wait_for_sidebar(&views, |s| {
                    s.radio.local_action() == Some(RadioAction::Retry)
                })
                .await;
                commands
                    .send(Command::SetRadioFavorite {
                        id: STATION.into(),
                        saved: false,
                    })
                    .await
                    .unwrap();
                wait_for_sidebar(&views, |s| {
                    s.radio.favorites.as_ref().is_some_and(Vec::is_empty)
                })
                .await;
                commands.send(Command::StopRadio).await.unwrap();
                wait_for_sidebar(&views, |s| s.radio.station.is_none()).await;
                commands.send(Command::LeaveVoice).await.unwrap();
                let left = wait_for_sidebar(&views, |s| s.voice.is_none()).await;
                assert!(left.radio.station.is_none());
                assert!(!left.radio.can_control_channel);
                assert_eq!(left.radio.local_action(), None);
                assert_eq!(left.radio.playback, RadioPlayback::Idle);
            },
        )
        .await;
    assert!(matches!(end, SessionEnd::Closed));
}
