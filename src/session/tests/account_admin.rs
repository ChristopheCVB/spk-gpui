use super::*;
use crate::api::{AccountRole, InviteStatus};

#[tokio::test]
async fn manages_invites_only_as_admin_and_retains_server_rejections() {
    let (advance, step) = async_channel::unbounded();
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            step.recv().await.unwrap();
            assert!(requests.is_empty());
            snapshots
                .send(SidebarSnapshot::AccountAdmin {
                    role: "admin",
                    target_role: "user",
                    banned: false,
                })
                .unwrap();
            users_changed(&mut socket).await;

            expect(&requests, "GET /api/invites")
                .await
                .respond_raw("403 Forbidden", &[], b"")
                .await;
            expect(&requests, "GET /api/invites")
                .await
                .respond(&[], "[]")
                .await;
            let request = expect(&requests, "POST /api/invites").await;
            assert_eq!(payload(&request), value!({"note":"For Bob"}));
            let origin = request.header("origin").unwrap();
            let issued = value!({"id":"invite-1","url":format!("{origin}/signup?invite=fixture")});
            request.respond(&[], &issued.to_string()).await;
            expect(&requests, "GET /api/invites")
                .await
                .respond(&[], &invite("active"))
                .await;
            expect(&requests, "POST /api/invites/invite-1/revoke")
                .await
                .respond(&[], r#"{"ok":true}"#)
                .await;
            expect(&requests, "GET /api/invites")
                .await
                .respond(&[], &invite("revoked"))
                .await;

            snapshots
                .send(SidebarSnapshot::AccountAdmin {
                    role: "user",
                    target_role: "user",
                    banned: false,
                })
                .unwrap();
            users_changed(&mut socket).await;
            step.recv().await.unwrap();
            assert!(requests.is_empty());
        },
        async |commands, views| {
            wait_for_sidebar(&views, |view| matches!(view.connection, Connection::Online)).await;
            let dispatch =
                async |action| commands.send(Command::AdminAccounts(action)).await.unwrap();
            dispatch(AdminAccountAction::Show(Some(AdminPage::Invites))).await;
            wait_for_sidebar(&views, |view| {
                view.admin_accounts.error == Some("Admin access is required.")
            })
            .await;
            advance.send(()).await.unwrap();
            wait_for_sidebar(&views, |view| view.is_admin()).await;

            dispatch(AdminAccountAction::Show(Some(AdminPage::Invites))).await;
            wait_for_sidebar(&views, |view| {
                view.admin_accounts.error == Some("Admin access is required.")
                    && !view.admin_accounts.loading
            })
            .await;
            dispatch(AdminAccountAction::Show(Some(AdminPage::Invites))).await;
            wait_for_sidebar(&views, |view| {
                view.admin_accounts
                    .invites
                    .as_ref()
                    .is_some_and(|rows| rows.is_empty())
            })
            .await;
            dispatch(AdminAccountAction::CreateInvite("  For Bob  ".into())).await;
            let active = wait_for_sidebar(&views, |view| {
                view.admin_accounts.invites.as_ref().is_some_and(|rows| {
                    rows.first()
                        .is_some_and(|row| row.status == InviteStatus::Active)
                })
            })
            .await;
            let link = active.admin_accounts.link.as_ref().unwrap();
            assert!(link.url.ends_with("/signup?invite=fixture"));
            let rows = active.admin_accounts.invites.as_ref().unwrap();
            assert_eq!(rows[0].note.as_deref(), Some("For Bob"));
            assert!(rows[1].status == InviteStatus::Used);
            assert_eq!(rows[1].used_by.as_ref().unwrap().name, "Bob");
            assert!(rows[2].status == InviteStatus::Expired);
            dispatch(AdminAccountAction::RevokeInvite("invite-1".into())).await;
            wait_for_sidebar(&views, |view| {
                view.admin_accounts.invites.as_ref().is_some_and(|rows| {
                    rows.first()
                        .is_some_and(|row| row.status == InviteStatus::Revoked)
                })
            })
            .await;
            let demoted = wait_for_sidebar(&views, |view| !view.is_admin()).await;
            assert!(demoted.admin_accounts.invites.is_none());
            assert!(demoted.admin_accounts.link.is_none());
            advance.send(()).await.unwrap();
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

fn invite(status: &str) -> String {
    value!([
        {
            "id":"invite-1","note":"For Bob","createdAt":1900000000000_i64,
            "expiresAt":1900000300000_i64,"status":status,"usedBy":null,
        },
        {
            "id":"invite-2","note":null,"createdAt":1800000000000_i64,
            "expiresAt":1800000300000_i64,"status":"used","usedBy":{"id":"bob","name":"Bob"},
        },
        {
            "id":"invite-3","note":null,"createdAt":1700000000000_i64,
            "expiresAt":1700000300000_i64,"status":"expired","usedBy":null,
        },
    ])
    .to_string()
}

async fn users_changed(socket: &mut WebSocketStream<TcpStream>) {
    socket
        .send(Message::text(r#"{"type":"users:changed"}"#))
        .await
        .unwrap();
}

#[tokio::test]
async fn admin_accounts_show_storage_and_follow_shared_role_and_ban_changes() {
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let account = |target_role, banned| SidebarSnapshot::AccountAdmin {
                role: "admin",
                target_role,
                banned,
            };
            snapshots.send(account("user", false)).unwrap();
            users_changed(&mut socket).await;

            let reset = expect(&requests, "POST /api/admin/users/bob/reset-link").await;
            let origin = reset.header("origin").unwrap().to_owned();
            reset
                .respond(
                    &[],
                    r#"{"url":"https://unrelated.test/reset-password?token=fixture"}"#,
                )
                .await;
            let reset = expect(&requests, "POST /api/admin/users/bob/reset-link").await;
            reset
                .respond(
                    &[],
                    &value!({"url":format!("{origin}/reset-password?token=fixture")}).to_string(),
                )
                .await;

            let role = expect(&requests, "POST /api/admin/users/alice/role").await;
            assert_eq!(payload(&role), value!({"role":"user"}));
            role.respond_raw("400 Bad Request", &[], b"").await;

            let role = expect(&requests, "POST /api/admin/users/bob/role").await;
            assert_eq!(payload(&role), value!({"role":"admin"}));
            snapshots.send(account("admin", false)).unwrap();
            role.respond(&[], r#"{"ok":true}"#).await;
            users_changed(&mut socket).await;

            let ban = expect(&requests, "POST /api/admin/users/bob/ban").await;
            assert_eq!(payload(&ban), value!({"banned":true,"reason":"Spam"}));
            snapshots.send(account("admin", true)).unwrap();
            ban.respond(&[], r#"{"ok":true}"#).await;
            users_changed(&mut socket).await;

            let unban = expect(&requests, "POST /api/admin/users/bob/ban").await;
            assert_eq!(payload(&unban), value!({"banned":false,"reason":""}));
            snapshots.send(account("admin", false)).unwrap();
            unban.respond(&[], r#"{"ok":true}"#).await;
            users_changed(&mut socket).await;

            // Another Admin's mutation arrives through the same event as it does on the web.
            snapshots.send(account("user", false)).unwrap();
            users_changed(&mut socket).await;
        },
        async |commands, views| {
            wait_for_sidebar(&views, |view| view.is_admin()).await;
            let dispatch =
                async |action| commands.send(Command::AdminAccounts(action)).await.unwrap();
            dispatch(AdminAccountAction::Show(Some(AdminPage::Users))).await;
            let listed = wait_for_sidebar(&views, |view| view.admin_accounts.users.is_some()).await;
            let directory = listed.admin_accounts.users.as_ref().unwrap();
            let bob = &directory.users[0];
            assert_eq!(directory.total_bytes, 4096);
            assert_eq!(bob.bytes, 512);
            assert_eq!(bob.email, "bob@example.test");
            assert_eq!(
                listed.inspect_person("bob").and_then(User::name),
                Some("Bob")
            );

            dispatch(AdminAccountAction::Ban {
                id: "alice".into(),
                reason: String::new(),
            })
            .await;
            wait_for_sidebar(&views, |view| {
                view.admin_accounts.error == Some("You can’t ban yourself.")
            })
            .await;

            let reset = || AdminAccountAction::ResetPassword {
                id: "bob".into(),
                name: "Bob".into(),
            };
            dispatch(reset()).await;
            let invalid = wait_for_sidebar(&views, |view| {
                view.admin_accounts.error == Some("Could not create the reset link. Try again.")
            })
            .await;
            assert!(invalid.admin_accounts.link.is_none());
            dispatch(reset()).await;
            let issued = wait_for_sidebar(&views, |view| view.admin_accounts.link.is_some()).await;
            let link = issued.admin_accounts.link.as_ref().unwrap();
            assert!(link.url.ends_with("/reset-password?token=fixture"));
            assert_eq!(link.user.as_deref(), Some("Bob"));

            dispatch(AdminAccountAction::SetRole(
                "alice".into(),
                AccountRole::User,
            ))
            .await;
            wait_for_sidebar(&views, |view| {
                view.admin_accounts.error == Some("The last Admin cannot be demoted.")
            })
            .await;
            dispatch(AdminAccountAction::SetRole(
                "bob".into(),
                AccountRole::Admin,
            ))
            .await;
            wait_for_sidebar(&views, |view| {
                listed_bob(view).is_some_and(|bob| bob.role == "admin")
                    && view.person("bob").is_some_and(|bob| bob.role == "admin")
            })
            .await;

            dispatch(AdminAccountAction::Ban {
                id: "bob".into(),
                reason: " Spam ".into(),
            })
            .await;
            let banned = wait_for_sidebar(&views, |view| {
                listed_bob(view).is_some_and(|bob| bob.banned) && view.person("bob").is_none()
            })
            .await;
            assert!(banned.inspect_person("bob").is_some_and(|bob| bob.banned));
            dispatch(AdminAccountAction::Unban("bob".into())).await;
            let peer_change = wait_for_sidebar(&views, |view| {
                listed_bob(view).is_some_and(|bob| !bob.banned && bob.role == "user")
                    && view.person("bob").is_some_and(|bob| bob.role == "user")
                    && view.admin_accounts.notice == Some("User unbanned.")
            })
            .await;
            assert_eq!(peer_change.admin_accounts.notice, Some("User unbanned."));
            dispatch(AdminAccountAction::Show(None)).await;
            let hidden = wait_for_sidebar(&views, |view| view.admin_accounts.page.is_none()).await;
            assert!(hidden.admin_accounts.users.is_none());
            assert!(hidden.admin_accounts.link.is_none());
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn sidebar_role_changes_report_server_results_and_refresh_users_without_settings() {
    let end = voice_session(
        Call::start_for_test,
        async |listener, requests, snapshots| {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            let account = |target_role| SidebarSnapshot::AccountAdmin {
                role: "admin",
                target_role,
                banned: false,
            };
            snapshots.send(account("user")).unwrap();
            users_changed(&mut socket).await;

            let demote = expect(&requests, "POST /api/admin/users/alice/role").await;
            assert_eq!(payload(&demote), value!({"role":"user"}));
            demote.respond_raw("400 Bad Request", &[], b"").await;

            let promote = expect(&requests, "POST /api/admin/users/bob/role").await;
            assert_eq!(payload(&promote), value!({"role":"admin"}));
            snapshots.send(account("admin")).unwrap();
            promote.respond(&[], r#"{"ok":true}"#).await;
        },
        async |commands, views| {
            wait_for_sidebar(&views, |view| view.is_admin()).await;
            let sidebar_role = async |id: &str, role| {
                commands
                    .send(Command::ModerateAccount(AdminAccountAction::SetRole(
                        id.into(),
                        role,
                    )))
                    .await
                    .unwrap();
            };
            sidebar_role("alice", AccountRole::User).await;
            let rejected = wait_for_sidebar(&views, |view| {
                view.account_moderation_notice == Some("The last Admin cannot be demoted.")
            })
            .await;
            assert!(rejected.is_admin());
            assert!(rejected.admin_accounts.page.is_none());

            sidebar_role("bob", AccountRole::Admin).await;
            let updated = wait_for_sidebar(&views, |view| {
                view.account_moderation_notice == Some("Role updated.")
                    && view.person("bob").is_some_and(|bob| bob.role == "admin")
            })
            .await;
            assert!(updated.admin_accounts.page.is_none());
            assert!(updated.admin_accounts.users.is_none());
        },
    )
    .await;
    assert!(matches!(end, SessionEnd::Closed));
}

#[tokio::test]
async fn a_ban_or_redeemed_password_reset_ends_the_affected_session() {
    for (frame, reason) in [
        (
            r#"{"type":"banned"}"#,
            "SPK removed access for this account.",
        ),
        (r#"{"type":"signed-out"}"#, SESSION_EXPIRED),
    ] {
        let (connected, ready) = async_channel::unbounded();
        let end = voice_session(
            Call::start_for_test,
            async |listener, _, _| {
                let mut socket = accept_async(listener.accept().await.unwrap().0)
                    .await
                    .unwrap();
                ready.recv().await.unwrap();
                socket.send(Message::text(frame)).await.unwrap();
                // Keep the connection open until the desktop processes the terminal frame.
                while let Some(Ok(message)) = socket.next().await {
                    if message.is_close() {
                        break;
                    }
                }
            },
            async |commands, views| {
                wait_for_sidebar(&views, |view| matches!(view.connection, Connection::Online))
                    .await;
                connected.send(()).await.unwrap();
                while views.recv().await.is_ok() {}
                drop(commands);
            },
        )
        .await;
        assert!(matches!(end, SessionEnd::Invalidated(message) if message == reason));
    }
}

fn listed_bob(view: &Sidebar) -> Option<&User> {
    view.admin_accounts
        .users
        .as_ref()?
        .users
        .first()
        .map(|entry| &entry.user)
}

async fn expect(requests: &Receiver<Request>, path: &str) -> Request {
    let request = requests.recv().await.unwrap();
    assert_eq!(request.line, format!("{path} HTTP/1.1"));
    request
}

fn payload(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}
