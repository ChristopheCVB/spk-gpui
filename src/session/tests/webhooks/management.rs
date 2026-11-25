use super::*;
use crate::api::{SearchScope, WebhookSettings};
use crate::session::{WebhookAction, WebhookError, WebhookReply};

#[tokio::test]
async fn admin_making_another_users_webhook_private_loses_access_with_other_readers() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let (changed, listed) = watch::channel(false);
    let (requests, submissions) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&listener).await;
            let owner = request.header("cookie") == Some("session=alice");
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" => match (*listed.borrow(), owner) {
                    (true, true) => value!([feed("Deployments", true)]),
                    (true, false) => value!([]),
                    (false, true) => value!([feed("Builds", false), {
                        "id":"personal","type":"webhook","name":"Personal","ownerId":"alice","private":true
                    }]),
                    (false, false) => value!([feed("Builds", false)]),
                },
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "GET /api/channels/feed/messages?unread=1 HTTP/1.1" => value!({
                    "messages":[{"id":"build","channelId":"feed","userId":"alice","createdAt":1,"body":"Build passed"}],
                    "hasMoreBefore":false,"hasMoreAfter":false
                }),
                "GET /api/search?q=build&scope=everything HTTP/1.1" => value!({
                    "results":[{
                        "messageId":"build","channelId":"feed","userId":"alice","createdAt":1,
                        "snippet":"Build passed","text":"Build passed","attachments":0
                    }],
                    "nextCursor":null
                }),
                "PATCH /api/webhooks/feed HTTP/1.1" => {
                    assert_eq!(request.header("cookie"), Some("session=admin"));
                    requests.send(request).await.unwrap();
                    continue;
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (owner, owner_views, _, mut owner_peer, owner_session) =
            chat::direct_message_session(base.clone(), "alice").await;
        let (reader, reader_views, _, mut reader_peer, reader_session) =
            chat::direct_message_session(base.clone(), "bob").await;
        let (admin, admin_views, _, _, admin_session) =
            chat::signed_in_chat_session(base, "admin", "admin").await;
        let owner_view = wait_for_sidebar(&owner_views, |view| view.channels.is_some()).await;
        assert!(owner_view.can_manage_webhook(owner_view.webhook_feed("personal").unwrap()));
        let reader_view = wait_for_sidebar(&reader_views, |view| view.channels.is_some()).await;
        assert!(!reader_view.can_manage_webhook(reader_view.webhook_feed("feed").unwrap()));
        let admin_view = wait_for_sidebar(&admin_views, |view| view.channels.is_some()).await;
        assert!(admin_view.can_manage_webhook(admin_view.webhook_feed("feed").unwrap()));
        assert!(admin_view.webhook_feed("personal").is_none());

        assert!(matches!(
            mutate(&reader, edit("feed")).await.await.unwrap(),
            Err(WebhookError::Action("You cannot manage this webhook."))
        ));
        assert!(matches!(
            mutate(&admin, edit("personal")).await.await.unwrap(),
            Err(WebhookError::Action("Webhook feed unavailable."))
        ));

        for (commands, views) in [(&admin, &admin_views), (&reader, &reader_views)] {
            commands
                .send(Command::OpenWebhook("feed".into()))
                .await
                .unwrap();
            wait_for_sidebar(views, |view| {
                view.chat.as_ref().is_some_and(ChatView::history_ready)
            })
            .await;
            commands
                .send(Command::SearchMessages {
                    query: "build".into(),
                    scope: SearchScope::Everything,
                })
                .await
                .unwrap();
            wait_for_sidebar(views, |view| view.search.results.len() == 1).await;
        }

        let pending = mutate(&admin, edit("feed")).await;
        let request = submissions.recv().await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            value!({"name":"Deployments","icon":"future-icon","private":true}),
        );
        changed.send_replace(true);
        request
            .respond(&[], &feed("Deployments", true).to_string())
            .await;
        assert!(matches!(pending.await.unwrap(), Ok(None)));
        owner_peer
            .send(Message::text(r#"{"type":"channels:changed"}"#))
            .await
            .unwrap();
        reader_peer
            .send(Message::text(r#"{"type":"channels:changed"}"#))
            .await
            .unwrap();

        for views in [&admin_views, &reader_views] {
            let unavailable = wait_for_sidebar(views, |view| {
                view.chat_notice == Some("Webhook feed unavailable.")
            })
            .await;
            assert!(unavailable.webhook_feed("feed").is_none());
            assert!(unavailable.chat.is_none());
            assert!(unavailable.search.results.is_empty());
        }
        let owner_view = wait_for_sidebar(&owner_views, |view| {
            view.webhook_feed("feed").is_some_and(|feed| feed.private)
        })
        .await;
        let private = owner_view.webhook_feed("feed").unwrap();
        assert_eq!(private.name.as_deref(), Some("Deployments"));
        assert_eq!(private.icon.as_deref(), Some("future-icon"));
        assert_eq!(private.owner_id.as_deref(), Some("alice"));

        for commands in [&owner, &reader, &admin] {
            commands.close();
        }
        for session in [owner_session, reader_session, admin_session] {
            assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
        }
    })
    .await
    .expect("webhook visibility change timed out");
    assert!(!http.is_finished());
    http.abort();
}

#[tokio::test]
async fn owner_rotates_private_webhook_and_receives_only_the_replacement_url() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let (visible, listed) = watch::channel(true);
    let (requests, submissions) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&listener).await;
            assert_eq!(request.header("cookie"), Some("session=alice"));
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" if *listed.borrow() => value!([feed("Builds", true)]),
                "GET /api/channels HTTP/1.1" => value!([]),
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "POST /api/webhooks/feed/secret HTTP/1.1" => {
                    requests.send(request).await.unwrap();
                    continue;
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (owner, owner_views, _, mut peer, owner_session) =
            chat::direct_message_session(base.clone(), "alice").await;
        wait_for_sidebar(&owner_views, |view| view.webhook_feed("feed").is_some()).await;

        let rotate = || WebhookAction::Rotate("feed".into());
        let failed = mutate(&owner, rotate()).await;
        submissions
            .recv()
            .await
            .unwrap()
            .respond_raw("403 Forbidden", &[], b"untrusted response body")
            .await;
        assert!(matches!(
            failed.await.unwrap(),
            Err(WebhookError::Action(
                "SPK denied permission to manage this webhook."
            ))
        ));
        wait_for_sidebar(&owner_views, |view| {
            view.action_results
                == [ActionResult::error(
                    "SPK denied permission to manage this webhook.",
                )]
        })
        .await;

        let invalid = WebhookAction::Create(WebhookSettings {
            name: " ".into(),
            icon: "bell".into(),
            private: false,
        });
        assert!(matches!(
            mutate(&owner, invalid).await.await.unwrap(),
            Err(WebhookError::Validation(
                "Webhook names must be 1–32 characters."
            ))
        ));

        let pending = mutate(&owner, rotate()).await;
        let request = submissions.recv().await.unwrap();
        assert!(request.body.is_empty());
        assert!(matches!(
            mutate(&owner, rotate()).await.await.unwrap(),
            Err(WebhookError::Action(
                "A webhook change is already in progress."
            ))
        ));
        let url = format!("{base}api/hooks/feed/replacement-fixture-secret");
        request.respond(&[], &value!({"url":url}).to_string()).await;
        let replacement = pending.await.unwrap().unwrap().expect("replacement URL");
        assert!(replacement.as_str() == url);
        let after =
            wait_for_sidebar(&owner_views, |view| view.webhook_feed("feed").is_some()).await;
        assert!(after.chat.is_none());

        let late = mutate(&owner, rotate()).await;
        let request = submissions.recv().await.unwrap();
        visible.send_replace(false);
        peer.send(Message::text(r#"{"type":"channels:changed"}"#))
            .await
            .unwrap();
        wait_for_sidebar(&owner_views, |view| view.webhook_feed("feed").is_none()).await;
        request.respond(&[], &value!({"url":url}).to_string()).await;
        assert!(matches!(
            late.await.unwrap(),
            Err(WebhookError::Action("Webhook feed unavailable."))
        ));

        owner.close();
        assert!(matches!(owner_session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("webhook rotation timed out");
    assert!(!http.is_finished());
    http.abort();
}

#[tokio::test]
async fn deleting_an_open_webhook_requires_acknowledgement_and_discards_stale_list() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let (hold_channels, holding_channels) = watch::channel(false);
    let (lists, snapshots) = async_channel::unbounded();
    let (requests, submissions) = async_channel::unbounded();
    let http = tokio::spawn(async move {
        loop {
            let (line, request) = Request::receive(&listener).await;
            assert_eq!(request.header("cookie"), Some("session=alice"));
            let body = match line.as_str() {
                "GET /api/channels HTTP/1.1" if *holding_channels.borrow() => {
                    lists.send(request).await.unwrap();
                    continue;
                }
                "GET /api/channels HTTP/1.1" => value!([feed("Builds", true)]),
                "GET /api/users HTTP/1.1" | "GET /api/voice HTTP/1.1" => value!([]),
                "GET /api/users/presence HTTP/1.1" | "GET /api/unread HTTP/1.1" => value!({}),
                "GET /api/channels/feed/messages?unread=1 HTTP/1.1" => value!({
                    "messages":[{"id":"build","channelId":"feed","userId":"alice","createdAt":1,"body":"Build passed"}],
                    "hasMoreBefore":false,"hasMoreAfter":false
                }),
                "DELETE /api/webhooks/feed HTTP/1.1" => {
                    requests.send(request).await.unwrap();
                    continue;
                }
                _ => panic!("unexpected request {line}"),
            };
            request.respond(&[], &body.to_string()).await;
        }
    });

    timeout(Duration::from_secs(10), async {
        let (owner, views, _, mut peer, session) =
            chat::direct_message_session(base, "alice").await;
        wait_for_sidebar(&views, |view| view.webhook_feed("feed").is_some()).await;
        owner
            .send(Command::OpenWebhook("feed".into()))
            .await
            .unwrap();
        wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_ready)
        })
        .await;

        let delete = || WebhookAction::Delete("feed".into());
        let failed = mutate(&owner, delete()).await;
        submissions
            .recv()
            .await
            .unwrap()
            .respond(&[], r#"{"ok":false}"#)
            .await;
        assert!(matches!(
            failed.await.unwrap(),
            Err(WebhookError::Action(
                "Could not update the webhook. Refresh the feed list before trying again."
            ))
        ));
        let still_open = wait_for_sidebar(&views, |view| {
            view.chat.as_ref().is_some_and(ChatView::history_ready)
        })
        .await;
        assert!(still_open.webhook_feed("feed").is_some());

        hold_channels.send_replace(true);
        owner.send(Command::Refresh).await.unwrap();
        let stale = snapshots.recv().await.unwrap();
        let pending = mutate(&owner, delete()).await;
        let request = submissions.recv().await.unwrap();
        assert!(request.body.is_empty());
        request.respond(&[], r#"{"ok":true}"#).await;
        assert!(matches!(pending.await.unwrap(), Ok(None)));
        let removed = wait_for_sidebar(&views, |view| view.webhook_feed("feed").is_none()).await;
        assert!(removed.chat.is_none());
        assert_eq!(removed.chat_notice, Some("Webhook feed unavailable."));

        stale
            .respond(&[], &value!([feed("Builds", true)]).to_string())
            .await;
        let fresh = snapshots.recv().await.unwrap();
        peer.send(Message::text(
            r#"{"type":"presence:update","userId":"bob","status":"busy"}"#,
        ))
        .await
        .unwrap();
        let reconciling = wait_for_sidebar(&views, |view| {
            view.user_presence("bob") == UserPresence::Busy
        })
        .await;
        assert!(reconciling.webhook_feed("feed").is_none());
        fresh.respond(&[], "[]").await;
        owner.close();
        assert!(matches!(session.await.unwrap(), SessionEnd::Closed));
    })
    .await
    .expect("webhook deletion timed out");
    assert!(!http.is_finished());
    http.abort();
}

fn edit(id: &str) -> WebhookAction {
    WebhookAction::Edit {
        id: id.into(),
        settings: WebhookSettings {
            name: "Deployments".into(),
            icon: "future-icon".into(),
            private: true,
        },
    }
}

fn feed(name: &str, private: bool) -> Value {
    value!({"id":"feed","type":"webhook","name": name,"icon":"future-icon","ownerId":"alice","private": private})
}

async fn mutate(
    commands: &Sender<Command>,
    action: WebhookAction,
) -> oneshot::Receiver<Result<Option<crate::api::WebhookUrl>, WebhookError>> {
    let (reply, result) = oneshot::channel();
    let reply: WebhookReply = reply;
    commands
        .send(Command::MutateWebhook { action, reply })
        .await
        .unwrap();
    result
}
