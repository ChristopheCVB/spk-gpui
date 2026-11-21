use super::*;

#[tokio::test]
async fn invite_redemption_requires_a_nickname_and_opens_the_signed_in_session() {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (tx, commands) = async_channel::unbounded();
    let (views, rx) = async_channel::unbounded();
    let (saved, storage) = SavedSession::with_test_store(None);
    let settings = fixture_settings("ws://127.0.0.1:1/_ws".into(), Call::start_for_test);

    let server = async {
        for reason in ["invalid", "expired", "revoked", "used"] {
            Request::read(
                &http,
                &format!("GET /api/invites/check?token={reason} HTTP/1.1"),
            )
            .await
            .respond(&[], &value!({"valid": false, "reason": reason}).to_string())
            .await;
        }
        for token in ["raced", "fixture-invite"] {
            let check = format!("GET /api/invites/check?token={token} HTTP/1.1");
            Request::read(&http, &check)
                .await
                .respond(&[], r#"{"valid":true,"reason":null}"#)
                .await;
            let signup = Request::read(&http, "POST /api/auth/sign-up/email HTTP/1.1").await;
            assert_eq!(signup.header("x-invite-token"), Some(token));
            assert_eq!(signup.header("cookie"), None);
            assert_eq!(
                serde_json::from_slice::<Value>(&signup.body).unwrap(),
                value!({"email":"alice@example.test","password":"fixture password","name":"alice"}),
            );
            if token == "raced" {
                signup.respond_raw("403 Forbidden", &[], b"").await;
                Request::read(&http, &check)
                    .await
                    .respond(&[], r#"{"valid":false,"reason":"revoked"}"#)
                    .await;
            } else {
                signup
                    .respond(&["session=new; Path=/; HttpOnly; Max-Age=3600"], "{}")
                    .await;
            }
        }
        let profile = Request::read(&http, "GET /api/profile/me HTTP/1.1").await;
        assert_eq!(profile.header("cookie"), Some("session=new"));
        profile
            .respond(
                &[],
                r#"{"id":"alice","email":"alice@example.test","displayName":null}"#,
            )
            .await;
        for name in ["Taken", "Alice"] {
            let nickname = Request::read(&http, "POST /api/profile/nickname HTTP/1.1").await;
            assert_eq!(nickname.header("cookie"), Some("session=new"));
            assert_eq!(
                serde_json::from_slice::<Value>(&nickname.body).unwrap(),
                value!({"displayName":name}),
            );
            if name == "Taken" {
                nickname.respond_raw("409 Conflict", &[], b"").await;
            } else {
                nickname.respond(&[], r#"{"displayName":"Alice"}"#).await;
            }
        }
    };
    let desktop = async {
        assert!(matches!(rx.recv().await.unwrap(), View::SignIn { .. }));
        tx.send(Command::OpenAccountLink).await.unwrap();
        assert!(matches!(
            rx.recv().await.unwrap(),
            View::AccountLink {
                form: AccountLinkForm::Link,
                message: None
            }
        ));
        for link in [
            "https://different.example/signup?invite=foreign".into(),
            format!("{base}signup?invite=first&invite=second"),
            format!("{base}signup?invite=malformed%0D"),
        ] {
            tx.send(Command::SubmitAccountLink(link)).await.unwrap();
            assert!(matches!(
                rx.recv().await.unwrap(),
                View::AccountLink {
                    form: AccountLinkForm::Link,
                    message: Some("Paste an invite or password reset link from this SPK instance."),
                }
            ));
        }
        for (token, state) in [
            ("invalid", "invalid"),
            ("expired", "expired"),
            ("revoked", "revoked"),
            ("used", "already been used"),
        ] {
            tx.send(Command::SubmitAccountLink(format!(
                "{base}signup?invite={token}"
            )))
            .await
            .unwrap();
            let View::AccountLink {
                form: AccountLinkForm::Link,
                message: Some(message),
            } = rx.recv().await.unwrap()
            else {
                panic!("expected an unavailable invite state");
            };
            assert!(message.contains(state));
            assert!(message.contains("Ask an admin for a new link."));
        }
        for token in ["raced", "fixture-invite"] {
            tx.send(Command::SubmitAccountLink(format!(
                "{base}signup?invite={token}"
            )))
            .await
            .unwrap();
            assert!(matches!(
                rx.recv().await.unwrap(),
                View::AccountLink {
                    form: AccountLinkForm::Invite,
                    message: None
                }
            ));
            tx.send(Command::RedeemAccountLink {
                email: "alice@example.test".into(),
                password: "fixture password".into(),
            })
            .await
            .unwrap();
            if token == "raced" {
                assert!(matches!(
                    rx.recv().await.unwrap(),
                    View::AccountLink {
                        form: AccountLinkForm::Link,
                        message: Some("This invite was revoked. Ask an admin for a new link."),
                    }
                ));
            }
        }
        assert!(matches!(
            rx.recv().await.unwrap(),
            View::ChooseNickname { message: None, .. }
        ));
        {
            let stored = storage.lock().unwrap();
            let cookies = json::load(stored.as_deref().unwrap()).unwrap();
            assert!(
                cookies
                    .matches(&base)
                    .iter()
                    .any(|cookie| { cookie.name() == "session" && cookie.value() == "new" })
            );
        }
        tx.send(Command::SetInitialNickname("Taken".into()))
            .await
            .unwrap();
        assert!(matches!(
            rx.recv().await.unwrap(),
            View::ChooseNickname {
                message: Some("That nickname is taken. Choose another."),
                ..
            }
        ));
        tx.send(Command::SetInitialNickname(" Alice ".into()))
            .await
            .unwrap();
        let sidebar = wait_for_sidebar(&rx, |_| true).await;
        assert_eq!(sidebar.profile.display_name.as_deref(), Some("Alice"));
        drop(tx);
    };

    timeout(Duration::from_secs(7), async {
        tokio::join!(
            server,
            desktop,
            run_session(base.clone(), saved, settings, commands, views)
        )
    })
    .await
    .expect("invite redemption workflow timed out");
}

#[tokio::test]
async fn reset_link_rechecks_unavailability_and_returns_to_sign_in() {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", http.local_addr().unwrap())).unwrap();
    let (tx, commands) = async_channel::unbounded();
    let (views, rx) = async_channel::unbounded();
    let (saved, _) = SavedSession::with_test_store(None);
    let settings = fixture_settings("ws://127.0.0.1:1/_ws".into(), Call::start_for_test);

    let server = async {
        for token in ["expired", "used", "fresh"] {
            Request::read(
                &http,
                &format!("GET /api/reset-password/check?token={token} HTTP/1.1"),
            )
            .await
            .respond(&[], &value!({"valid": token != "expired"}).to_string())
            .await;
            if token == "expired" {
                continue;
            }
            let reset = Request::read(&http, "POST /api/reset-password HTTP/1.1").await;
            assert_eq!(reset.header("cookie"), None);
            assert_eq!(
                serde_json::from_slice::<Value>(&reset.body).unwrap(),
                value!({"token":token,"password":"new fixture password"}),
            );
            if token == "used" {
                reset.respond_raw("410 Gone", &[], b"").await;
            } else {
                reset.respond(&[], r#"{"ok":true}"#).await;
            }
        }
        let sign_in = Request::read(&http, "POST /api/auth/sign-in/email HTTP/1.1").await;
        assert_eq!(
            serde_json::from_slice::<Value>(&sign_in.body).unwrap(),
            value!({"email":"alice@example.test","password":"new fixture password","rememberMe":true}),
        );
        sign_in
            .respond(&["session=reset; Path=/; HttpOnly; Max-Age=3600"], "{}")
            .await;
        let profile = Request::read(&http, "GET /api/profile/me HTTP/1.1").await;
        assert_eq!(profile.header("cookie"), Some("session=reset"));
        profile
            .respond(
                &[],
                r#"{"email":"alice@example.test","displayName":"Alice"}"#,
            )
            .await;
    };
    let desktop = async {
        assert!(matches!(rx.recv().await.unwrap(), View::SignIn { .. }));
        tx.send(Command::OpenAccountLink).await.unwrap();
        assert!(matches!(
            rx.recv().await.unwrap(),
            View::AccountLink {
                form: AccountLinkForm::Link,
                message: None
            }
        ));
        for token in ["expired", "used", "fresh"] {
            tx.send(Command::SubmitAccountLink(format!(
                "{base}reset-password?token={token}"
            )))
            .await
            .unwrap();
            if token != "expired" {
                assert!(matches!(
                    rx.recv().await.unwrap(),
                    View::AccountLink {
                        form: AccountLinkForm::Reset,
                        message: None
                    }
                ));
                tx.send(Command::RedeemAccountLink {
                    email: String::new(),
                    password: "new fixture password".into(),
                })
                .await
                .unwrap();
            }
            if token != "fresh" {
                let View::AccountLink {
                    form: AccountLinkForm::Link,
                    message: Some(message),
                } = rx.recv().await.unwrap()
                else {
                    panic!("expected an unavailable reset link state");
                };
                assert!(message.contains("invalid, expired or already used"));
                assert!(message.contains("Ask an admin for a new link."));
            }
        }
        let View::SignIn {
            message: Some(message),
            ..
        } = rx.recv().await.unwrap()
        else {
            panic!("expected sign-in after a successful password reset");
        };
        assert!(message.contains("Sign in with the new password."));
        tx.send(Command::SignIn {
            email: "alice@example.test".into(),
            password: "new fixture password".into(),
        })
        .await
        .unwrap();
        let sidebar = wait_for_sidebar(&rx, |_| true).await;
        assert_eq!(sidebar.profile.name(), "Alice");
        drop(tx);
    };

    timeout(Duration::from_secs(7), async {
        tokio::join!(
            server,
            desktop,
            run_session(base.clone(), saved, settings, commands, views)
        )
    })
    .await
    .expect("password reset workflow timed out");
}
