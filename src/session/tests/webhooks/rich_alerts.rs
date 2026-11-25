use super::*;
use crate::api::ChatMessage;

#[tokio::test]
async fn signed_in_reader_receives_plain_and_rich_alerts_in_history_and_live() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let sender_avatar = base.join("/sender.gif").unwrap();
    let live_avatar = base.join("/live-sender.gif").unwrap();
    let missing_avatar = base.join("/missing.gif").unwrap();
    let alerts = [
        value!({"id":"plain","channelId":"feed","userId":"alice","createdAt":1,"body":"Build started","embed":null}),
        value!({"id":"card","channelId":"feed","userId":"alice","createdAt":2,"body":"","embed":{
            "title":"Build succeeded", "url":"https://ci.example.test/runs/12", "color":"success",
            "description":"[`8a072c0`](https://github.com/spk/app/commit/8a072c0) Add tests — [Alice](https://github.com/alice)\n[`c9407ec`](https://github.com/spk/app/commit/c9407ec) Clean up — Local Author",
            "author":{"name":"Push Sender","url":"https://github.com/push-sender","icon":sender_avatar.as_str()},
            "fields":[
                {"name":"Branch","value":"main","inline":true},
                {"name":"Summary","value":"No failures"},
                {"name":"Run","value":"https://ci.example.test/runs/12","inline":true},
                {"name":"Contact","value":"mailto:ci@example.test","inline":true},
                {"name":"Note","value":"https://ci.example.test is online","inline":true},
                {"name":"Incomplete","value":"https://","inline":true}
            ],
            "footer":"Build service", "timestamp":"2026-11-24T09:15:00.000Z"
        }}),
        value!({"id":"combined","channelId":"feed","userId":"alice","createdAt":3,"body":"Deployment needs attention","embed":{
            "title":"Review deployment", "url":"mailto:ci@example.test", "color":"warning",
            "author":{"name":"Deployment Author","url":"javascript:alert(1)","icon":missing_avatar.as_str()}
        }}),
    ];
    let history = alerts.clone();
    let (missing_request, missing_response) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&http).await;
            if matches!(
                line.as_str(),
                "GET /sender.gif HTTP/1.1"
                    | "GET /live-sender.gif HTTP/1.1"
                    | "GET /missing.gif HTTP/1.1"
            ) {
                assert_eq!(request.header("cookie"), None);
                assert_eq!(request.header("authorization"), None);
                assert_eq!(request.header("origin"), None);
                if !line.starts_with("GET /missing.gif ") {
                    request
                        .respond_raw(
                            "200 OK",
                            &["Content-Type: image/gif"],
                            include_bytes!("../../../../tests/fixtures/attachments/preview.gif"),
                        )
                        .await;
                } else {
                    request.respond_raw("404 Not Found", &[], b"").await;
                    missing_request.send(()).await.unwrap();
                }
                continue;
            }
            assert_eq!(request.header("cookie"), Some("session=bob"));
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" => value!([
                    {"id":"feed","type":"webhook","name":"Builds","ownerId":"alice","private":false}
                ]),
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "GET /api/channels/feed/messages?unread=1 HTTP/1.1" => value!({
                    "messages":history,"hasMoreBefore":false,"hasMoreAfter":false
                }),
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    let assert_alerts = |messages: &[ChatMessage], link: &str| {
        let [plain, card, combined] = messages else {
            panic!("expected three alerts");
        };
        assert_eq!(plain.body, "Build started");
        assert!(plain.embed.is_none());
        assert!(card.body.is_empty());
        let embed = card.embed.as_ref().unwrap();
        assert_eq!(embed.title.as_deref(), Some("Build succeeded"));
        assert_eq!(embed.title_link().unwrap().as_str(), link);
        let author = embed.author().unwrap();
        assert_eq!(author.name, "Push Sender");
        assert_eq!(
            author.link().unwrap().as_str(),
            "https://github.com/push-sender"
        );
        assert_eq!(embed.color, Some(crate::api::EmbedColor::Success));
        assert_eq!(embed.fields.len(), 6);
        assert_eq!(embed.fields[0].name, "Branch");
        assert_eq!(embed.fields[0].value, "main");
        assert!(embed.fields[0].inline);
        assert_eq!(embed.fields[1].name, "Summary");
        assert_eq!(embed.fields[1].value, "No failures");
        assert!(!embed.fields[1].inline);
        assert_eq!(
            embed.fields[2].link().unwrap().as_str(),
            "https://ci.example.test/runs/12"
        );
        assert!(embed.fields[3..].iter().all(|field| field.link().is_none()));
        assert_eq!(embed.footer.as_deref(), Some("Build service"));
        assert_eq!(embed.timestamp.as_deref(), Some("2026-11-24T09:15:00.000Z"));
        assert_eq!(combined.body, "Deployment needs attention");
        let embed = combined.embed.as_ref().unwrap();
        assert_eq!(embed.title.as_deref(), Some("Review deployment"));
        assert!(embed.title_link().is_none());
        let author = embed.author().unwrap();
        assert_eq!(author.name, "Deployment Author");
        assert!(author.link().is_none());
    };

    timeout(Duration::from_secs(10), async {
        let (commands, views, _, mut peer, session) =
            chat::direct_message_session(base, "bob").await;
        commands
            .send(Command::OpenWebhook("feed".into()))
            .await
            .unwrap();
        let opened = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(|chat| {
                chat.preview_images
                    .avatar("card", sender_avatar.as_str())
                    .is_some()
            })
        })
        .await;
        missing_response.recv().await.unwrap();
        assert_alerts(
            &opened.chat.unwrap().messages,
            "https://ci.example.test/runs/12",
        );

        for (index, mut alert) in alerts.into_iter().enumerate() {
            alert["id"] = value!(format!("live-{index}"));
            alert["createdAt"] = value!(index + 4);
            if index == 1 {
                alert["embed"]["url"] = value!("http://ci.example.test/runs/13");
                alert["embed"]["author"]["icon"] = value!(live_avatar.as_str());
            }
            peer.send(Message::text(
                value!({"type":"message:new","message":alert}).to_string(),
            ))
            .await
            .unwrap();
        }
        let updated = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(|chat| {
                chat.messages.len() == 6
                    && chat
                        .preview_images
                        .avatar("live-1", live_avatar.as_str())
                        .is_some()
            })
        })
        .await;
        missing_response.recv().await.unwrap();
        assert_alerts(
            &updated.chat.unwrap().messages[3..],
            "http://ci.example.test/runs/13",
        );
        commands.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("rich webhook alerts timed out");
    http.abort();
}
