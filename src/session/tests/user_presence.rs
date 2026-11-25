use super::*;

#[tokio::test]
async fn user_presence_follows_the_instance_without_hiding_voice_participants() {
    let (away, away_seen) = oneshot::channel();
    let (loaded, snapshot_seen) = oneshot::channel();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, snapshots| {
            snapshots.send_replace(SidebarSnapshot::DelayUserPresence);
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let snapshot = requests.recv().await.unwrap();
            assert_eq!(snapshot.header("cookie"), Some("session=fixture"));
            for frame in [
                value!({"type":"voice:presence","userId":"bob","channelId":"first"}),
                value!({"type":"presence:update","userId":"bob","status":"idle"}),
            ] {
                socket.send(Message::text(frame.to_string())).await.unwrap();
            }
            away_seen.await.unwrap();
            snapshot
                .respond(&[], r#"{"bob":"busy","charlie":"online"}"#)
                .await;
            snapshot_seen.await.unwrap();
            socket
                .send(Message::text(
                    r#"{"type":"presence:update","userId":"bob","status":"offline"}"#,
                ))
                .await
                .unwrap();
            let _ = receive_non_presence_message(&mut socket).await;
        },
        async |_, views| {
            wait_for_sidebar(&views, |sidebar| {
                sidebar.user_presence("bob").label() == "Away"
            })
            .await;
            away.send(()).unwrap();
            let sidebar = wait_for_sidebar(&views, |sidebar| {
                sidebar.user_presence("charlie").label() == "Online"
            })
            .await;
            assert_eq!(sidebar.user_presence("bob").label(), "Away");
            assert_eq!(sidebar.user_presence("absent").label(), "Offline");
            loaded.send(()).unwrap();
            let sidebar = wait_for_sidebar(&views, |sidebar| {
                sidebar.user_presence("bob").label() == "Offline"
            })
            .await;
            assert!(
                sidebar
                    .participants("first")
                    .iter()
                    .any(|person| person.id == "bob")
            );
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn desktop_reports_status_and_activity_and_replays_them_after_reconnect() {
    let (activity, reports) = watch::channel(true);
    let (reported, progress) = async_channel::bounded(1);
    let (replayed, reconnected) = oneshot::channel();
    let (done, finished) = oneshot::channel();
    let end = voice_session_with_activity(
        reports,
        Call::start_for_test,
        async |listener, _, _| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            expect_presence_frame(
                &mut socket,
                value!({"type":"presence:set","status":"online"}),
            )
            .await;
            expect_presence_frame(&mut socket, value!({"type":"presence:active"})).await;
            reported.send(()).await.unwrap();

            for frame in [
                value!({"type":"presence:set","status":"away"}),
                value!({"type":"presence:set","status":"busy"}),
                value!({"type":"presence:idle"}),
            ] {
                expect_presence_frame(&mut socket, frame).await;
                reported.send(()).await.unwrap();
            }
            expect_presence_frame(
                &mut socket,
                value!({"type":"presence:set","status":"offline"}),
            )
            .await;
            socket.close(None).await.unwrap();

            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            expect_presence_frame(
                &mut socket,
                value!({"type":"presence:set","status":"offline"}),
            )
            .await;
            expect_presence_frame(&mut socket, value!({"type":"presence:idle"})).await;
            for frame in [
                value!({"type":"voice:presence","userId":"alice","channelId":"first"}),
                value!({"type":"presence:update","userId":"alice","status":"offline"}),
            ] {
                socket.send(Message::text(frame.to_string())).await.unwrap();
            }
            replayed.send(()).unwrap();
            expect_presence_frame(&mut socket, value!({"type":"presence:active"})).await;
            done.send(()).unwrap();
        },
        async |commands, views| {
            progress.recv().await.unwrap();
            for status in [UserStatus::Away, UserStatus::Busy] {
                commands.send(Command::SetUserStatus(status)).await.unwrap();
                let sidebar =
                    wait_for_sidebar(&views, |sidebar| sidebar.user_status == status).await;
                assert_eq!(
                    sidebar.action_results,
                    [ActionResult::error(
                        "Could not save your status for the next launch."
                    )]
                );
                progress.recv().await.unwrap();
            }
            activity.send_replace(false);
            progress.recv().await.unwrap();
            commands
                .send(Command::SetUserStatus(UserStatus::Invisible))
                .await
                .unwrap();
            reconnected.await.unwrap();
            let sidebar = wait_for_sidebar(&views, |sidebar| {
                sidebar
                    .participants("first")
                    .iter()
                    .any(|person| person.id == "alice")
            })
            .await;
            assert_eq!(sidebar.user_status.label(), "Invisible");
            assert_eq!(sidebar.user_presence("alice").label(), "Offline");
            activity.send_replace(true);
            finished.await.unwrap();
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

async fn expect_presence_frame(socket: &mut WebSocketStream<TcpStream>, expected: Value) {
    let frame = socket.next().await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(frame.to_text().unwrap()).unwrap(),
        expected
    );
}
