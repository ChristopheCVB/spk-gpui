use super::*;

const STATION: &str = "23456789-1111-4ccc-8aaa-123456789abc";

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
                wait_for_sidebar(&views, |s| {
                    s.channel_radio
                        .get("second")
                        .is_some_and(|name| name == "Peer station")
                })
                .await;
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
                wait_for_sidebar(&views, |s| s.radio.playback == RadioPlayback::Unavailable).await;
                commands.send(Command::PauseRadio).await.unwrap();
                commands.send(Command::SetRadioVolume(25)).await.unwrap();
                commands.send(Command::ToggleDeafen).await.unwrap();
                let paused = wait_for_sidebar(&views, |s| {
                    s.radio.volume == 25 && s.voice_intent.mute.deafened
                })
                .await;
                assert_eq!(paused.radio.station.as_ref().unwrap().name, "Shared jazz");
                assert!(!paused.radio.enabled);
                commands.send(Command::ListenToRadio).await.unwrap();
                wait_for_sidebar(&views, |s| {
                    s.radio.enabled && s.radio.playback == RadioPlayback::Unavailable
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
                assert_eq!(left.radio.playback, RadioPlayback::Idle);
            },
        )
        .await;
    assert!(matches!(end, SessionEnd::Closed));
}
