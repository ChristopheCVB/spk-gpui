use std::sync::Arc;

use reqwest::StatusCode;
use tokio::task::JoinSet;

use super::Sidebar;
use crate::api::{AccountRole, AdminDirectory, Failure, Invite, IssuedAccountLink, SpkClient};

const ACCESS_REQUIRED: &str = "Admin access is required.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AdminPage {
    Invites,
    Users,
}

pub enum AdminAccountAction {
    Show(Option<AdminPage>),
    CreateInvite(String),
    RevokeInvite(String),
    ResetPassword { id: String, name: String },
    SetRole(String, AccountRole),
    Ban { id: String, reason: String },
    Unban(String),
}

impl AdminAccountAction {
    fn page(&self) -> AdminPage {
        match self {
            Self::Show(Some(page)) => *page,
            Self::Show(None) | Self::CreateInvite(_) | Self::RevokeInvite(_) => AdminPage::Invites,
            Self::ResetPassword { .. } | Self::SetRole(..) | Self::Ban { .. } | Self::Unban(_) => {
                AdminPage::Users
            }
        }
    }

    async fn send(&self, client: &SpkClient) -> Result<Option<IssuedAccountLink>, Failure> {
        match self {
            Self::Show(_) => unreachable!("list requests are handled separately"),
            Self::CreateInvite(note) => client.create_invite(&limited_text(note)).await.map(Some),
            Self::RevokeInvite(id) => client.revoke_invite(id).await.map(|()| None),
            Self::ResetPassword { id, .. } => client.issue_password_reset(id).await.map(Some),
            Self::SetRole(id, role) => client.set_account_role(id, *role).await.map(|()| None),
            Self::Ban { id, reason } => client
                .set_account_banned(id, true, &limited_text(reason))
                .await
                .map(|()| None),
            Self::Unban(id) => client
                .set_account_banned(id, false, "")
                .await
                .map(|()| None),
        }
    }

    fn error(&self, error: Failure) -> &'static str {
        match (self, error) {
            (Self::RevokeInvite(_), Failure::Http(StatusCode::CONFLICT)) => {
                "That Invite Link is no longer active."
            }
            (Self::SetRole(..), Failure::Http(StatusCode::BAD_REQUEST)) => {
                "The last Admin cannot be demoted."
            }
            (Self::CreateInvite(_), _) => "Could not create the Invite Link. Try again.",
            (Self::RevokeInvite(_), _) => "Could not revoke the Invite Link. Try again.",
            (Self::ResetPassword { .. }, _) => "Could not create the reset link. Try again.",
            (Self::SetRole(..), _) => "Could not change the role. Try again.",
            (Self::Ban { .. }, _) => "Could not ban the user. Try again.",
            (Self::Unban(_), _) => "Could not unban the user. Try again.",
            (Self::Show(_), _) => unreachable!("list requests are handled separately"),
        }
    }
}

fn limited_text(text: &str) -> String {
    let mut units = 0;
    text.trim()
        .chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= 200
        })
        .collect()
}

#[derive(Clone, Default)]
pub struct AdminAccountsView {
    pub page: Option<AdminPage>,
    pub invites: Option<Arc<Vec<Invite>>>,
    pub users: Option<Arc<AdminDirectory>>,
    pub link: Option<Arc<RevealedLink>>,
    pub loading: bool,
    pub busy: bool,
    pub error: Option<&'static str>,
    pub notice: Option<&'static str>,
}

pub struct RevealedLink {
    pub id: Option<String>,
    pub url: String,
    pub user: Option<String>,
}

pub(super) enum Listing {
    Invites(Vec<Invite>),
    Users(AdminDirectory),
}

#[derive(Default)]
pub(super) struct AccountAdmin {
    reload: bool,
    pub(super) loads: JoinSet<Result<Listing, Failure>>,
    pub(super) mutations: JoinSet<(
        AdminAccountAction,
        Result<Option<IssuedAccountLink>, Failure>,
    )>,
}

impl AccountAdmin {
    pub(super) fn command(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        action: AdminAccountAction,
    ) {
        if matches!(action, AdminAccountAction::Show(None)) {
            self.clear(sidebar);
            return;
        }
        if !sidebar.is_admin() {
            sidebar.admin_accounts.error = Some(ACCESS_REQUIRED);
            return;
        }
        if let AdminAccountAction::Show(page) = action {
            if sidebar.admin_accounts.page != page {
                self.loads = JoinSet::new();
                self.reload = false;
                sidebar.admin_accounts = AdminAccountsView {
                    page,
                    busy: !self.mutations.is_empty(),
                    ..Default::default()
                };
            }
            sidebar.admin_accounts.error = None;
            self.refresh(sidebar, client);
            return;
        }
        if sidebar.admin_accounts.page != Some(action.page()) || !self.mutations.is_empty() {
            return;
        }
        let view = &mut sidebar.admin_accounts;
        view.error = None;
        view.notice = None;
        if matches!(&action, AdminAccountAction::Ban { id, .. } if sidebar.profile.id.as_ref() == Some(id))
        {
            view.error = Some("You can’t ban yourself.");
            return;
        }
        if matches!(
            &action,
            AdminAccountAction::CreateInvite(_) | AdminAccountAction::ResetPassword { .. }
        ) {
            view.link = None;
        }
        view.busy = true;
        let client = client.clone();
        self.mutations.spawn(async move {
            let result = action.send(&client).await;
            (action, result)
        });
    }

    fn clear(&mut self, sidebar: &mut Sidebar) {
        *self = Self::default();
        sidebar.admin_accounts = AdminAccountsView::default();
    }

    pub(super) fn clear_if_demoted(&mut self, sidebar: &mut Sidebar) {
        if sidebar.admin_accounts.page.is_some() && !sidebar.is_admin() {
            self.clear(sidebar);
        }
    }

    pub(super) fn users_changed(&mut self, sidebar: &mut Sidebar, client: &SpkClient) {
        if sidebar.admin_accounts.page == Some(AdminPage::Users) {
            self.refresh(sidebar, client);
        }
    }

    pub(super) fn refresh(&mut self, sidebar: &mut Sidebar, client: &SpkClient) {
        let Some(page) = sidebar.admin_accounts.page else {
            return;
        };
        if !self.loads.is_empty() {
            self.reload = true;
            return;
        }
        sidebar.admin_accounts.loading = true;
        let client = client.clone();
        self.loads.spawn(async move {
            match page {
                AdminPage::Invites => client.invites().await.map(Listing::Invites),
                AdminPage::Users => client.admin_users().await.map(Listing::Users),
            }
        });
    }

    pub(super) fn loaded(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        result: Result<Listing, Failure>,
    ) -> Result<(), Failure> {
        let view = &mut sidebar.admin_accounts;
        view.loading = false;
        match result {
            Ok(Listing::Invites(invites)) => view.invites = Some(Arc::new(invites)),
            Ok(Listing::Users(users)) => view.users = Some(Arc::new(users)),
            Err(error) if error.unauthorized() => return Err(error),
            Err(Failure::Http(StatusCode::FORBIDDEN)) => {
                *view = AdminAccountsView {
                    page: view.page,
                    busy: view.busy,
                    error: Some(ACCESS_REQUIRED),
                    ..Default::default()
                };
            }
            Err(_) => {
                view.error = Some(match view.page {
                    Some(AdminPage::Invites) => "Could not load Invite Links. Try again.",
                    Some(AdminPage::Users) => "Could not load users. Try again.",
                    None => return Ok(()),
                })
            }
        }
        if std::mem::take(&mut self.reload) {
            self.refresh(sidebar, client);
        }
        Ok(())
    }

    pub(super) fn mutated(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        action: AdminAccountAction,
        result: Result<Option<IssuedAccountLink>, Failure>,
    ) -> Result<bool, Failure> {
        let view = &mut sidebar.admin_accounts;
        view.busy = false;
        if result.as_ref().is_err_and(Failure::unauthorized) {
            return result.map(|_| false);
        }
        if matches!(result, Err(Failure::Http(StatusCode::FORBIDDEN))) {
            self.loads = JoinSet::new();
            self.reload = false;
            *view = AdminAccountsView {
                page: view.page,
                error: Some(ACCESS_REQUIRED),
                ..Default::default()
            };
            return Ok(true);
        }
        let changed_users = result.is_ok()
            && matches!(
                &action,
                AdminAccountAction::Ban { .. }
                    | AdminAccountAction::Unban(_)
                    | AdminAccountAction::SetRole(..)
            );
        if view.page == Some(action.page()) {
            match result {
                Ok(Some(link)) => {
                    view.link = Some(Arc::new(RevealedLink {
                        id: link.id,
                        url: link.url,
                        user: match &action {
                            AdminAccountAction::ResetPassword { name, .. } => Some(name.clone()),
                            _ => None,
                        },
                    }));
                }
                Ok(None) => {
                    view.notice = Some(match &action {
                        AdminAccountAction::RevokeInvite(id) => {
                            if view.link.as_ref().and_then(|link| link.id.as_ref()) == Some(id) {
                                view.link = None;
                            }
                            "Invite Link revoked."
                        }
                        AdminAccountAction::SetRole(..) => "Role updated.",
                        AdminAccountAction::Ban { .. } => "User banned.",
                        AdminAccountAction::Unban(_) => "User unbanned.",
                        _ => unreachable!("creation endpoints return links"),
                    });
                }
                Err(error) => view.error = Some(action.error(error)),
            }
            if !matches!(action, AdminAccountAction::ResetPassword { .. }) {
                self.refresh(sidebar, client);
            }
        }
        Ok(changed_users)
    }
}
