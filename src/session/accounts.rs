use async_channel::{Receiver, Sender};
use cookie_store::CookieStore;
use reqwest::StatusCode;
use url::Url;

use super::{Command, SessionEnd, View, authentication::SESSION_EXPIRED};
use crate::api::{AccountLinkCheck, Failure, Profile, SpkClient, account_link_token};

const INVALID_LINK: &str = "Paste an invite or password reset link from this SPK instance.";
const RESET_UNAVAILABLE: &str =
    "This reset link is invalid, expired or already used. Ask an admin for a new link.";

#[derive(Clone, Copy)]
pub enum AccountLinkForm {
    Link,
    Invite,
    Reset,
}

pub(super) enum AccountLinkOutcome {
    Cancelled,
    PasswordChanged,
    SignedUp(SpkClient),
}

enum AccountLink {
    Invite(String),
    Reset(String),
}

impl AccountLink {
    fn parse(base: &Url, text: &str) -> Option<Self> {
        let url = Url::parse(text.trim()).ok()?;
        let path = url.path();
        match path {
            "/signup" => account_link_token(base, &url, path, "invite").map(Self::Invite),
            "/reset-password" => account_link_token(base, &url, path, "token").map(Self::Reset),
            _ => None,
        }
    }

    fn form(&self) -> AccountLinkForm {
        match self {
            Self::Invite(_) => AccountLinkForm::Invite,
            Self::Reset(_) => AccountLinkForm::Reset,
        }
    }

    async fn check(&self, client: &SpkClient) -> Result<(), &'static str> {
        let result = match self {
            Self::Invite(token) => client.check_invite(token).await,
            Self::Reset(token) => client.check_password_reset(token).await,
        };
        match result {
            Ok(AccountLinkCheck { valid: true, .. }) => Ok(()),
            Ok(_) if matches!(self, Self::Reset(_)) => Err(RESET_UNAVAILABLE),
            Ok(check) => Err(match check.reason.as_deref() {
                Some("expired") => "This invite has expired. Ask an admin for a new link.",
                Some("revoked") => "This invite was revoked. Ask an admin for a new link.",
                Some("used") => {
                    "This invite has already been used. Sign in if you have an account. Ask an admin for a new link."
                }
                _ => "This invite is invalid. Ask an admin for a new link.",
            }),
            Err(Failure::Connection) => {
                Err("Could not reach SPK. Check your connection and paste the link again.")
            }
            Err(_) => Err("Could not check the link. Paste it again to retry."),
        }
    }
}

pub(super) async fn redeem_link(
    base: &Url,
    commands: &Receiver<Command>,
    views: &Sender<View>,
) -> Option<AccountLinkOutcome> {
    let client = match SpkClient::new(base.clone(), CookieStore::default()) {
        Ok(client) => client,
        Err(_) => return Some(AccountLinkOutcome::Cancelled),
    };
    let mut link = None;
    let mut message = None;
    loop {
        let form = link
            .as_ref()
            .map_or(AccountLinkForm::Link, AccountLink::form);
        views.send(View::AccountLink { form, message }).await.ok()?;
        message = match (commands.recv().await.ok()?, link.as_ref()) {
            (Command::BackToSignIn, _) => return Some(AccountLinkOutcome::Cancelled),
            (Command::OpenAccountLink, _) => {
                link = None;
                None
            }
            (Command::SubmitAccountLink(text), None) => match AccountLink::parse(base, &text) {
                Some(candidate) => match candidate.check(&client).await {
                    Ok(()) => {
                        link = Some(candidate);
                        None
                    }
                    Err(error) => Some(error),
                },
                None => Some(INVALID_LINK),
            },
            (Command::RedeemAccountLink { password, .. }, Some(_))
                if !(8..=128).contains(&password.encode_utf16().count()) =>
            {
                Some("Use a password between 8 and 128 characters.")
            }
            (
                Command::RedeemAccountLink { email, password },
                Some(invite @ AccountLink::Invite(token)),
            ) => {
                let email = email.trim();
                if !email
                    .split_once('@')
                    .is_some_and(|(name, domain)| !name.is_empty() && !domain.is_empty())
                {
                    Some("Enter your email address.")
                } else {
                    match client.sign_up(token, email, &password).await {
                        Ok(()) => return Some(AccountLinkOutcome::SignedUp(client)),
                        Err(Failure::Http(StatusCode::FORBIDDEN)) => match invite
                            .check(&client)
                            .await
                        {
                            Ok(()) => Some("SPK denied account creation. Ask an admin for help."),
                            Err(error) => {
                                link = None;
                                Some(error)
                            }
                        },
                        Err(Failure::Http(
                            StatusCode::BAD_REQUEST
                            | StatusCode::UNPROCESSABLE_ENTITY
                            | StatusCode::CONFLICT,
                        )) => Some(
                            "Check your email and password. If you already have an account, sign in.",
                        ),
                        Err(error) => Some(redemption_error(
                            error,
                            "Could not create the account. Try again.",
                        )),
                    }
                }
            }
            (Command::RedeemAccountLink { password, .. }, Some(AccountLink::Reset(token))) => {
                match client.reset_password(token, &password).await {
                    Ok(()) => return Some(AccountLinkOutcome::PasswordChanged),
                    Err(Failure::Http(StatusCode::GONE)) => {
                        link = None;
                        Some(RESET_UNAVAILABLE)
                    }
                    Err(Failure::Http(StatusCode::CONFLICT)) => {
                        Some("This account has no password to reset. Ask an admin for help.")
                    }
                    Err(error) => Some(redemption_error(
                        error,
                        "Could not reset your password. Try again.",
                    )),
                }
            }
            _ => message,
        };
    }
}

fn redemption_error(error: Failure, fallback: &'static str) -> &'static str {
    match error {
        Failure::Connection => "Could not reach SPK. Check your connection and try again.",
        Failure::Http(StatusCode::TOO_MANY_REQUESTS) => {
            "Too many attempts. Wait a minute and try again."
        }
        _ => fallback,
    }
}

pub(super) async fn choose_nickname(
    client: &SpkClient,
    mut profile: Profile,
    notice: Option<&'static str>,
    commands: &Receiver<Command>,
    views: &Sender<View>,
) -> Result<Profile, SessionEnd> {
    if profile
        .display_name
        .as_ref()
        .is_some_and(|name| !name.is_empty())
    {
        return Ok(profile);
    }
    let mut message = None;
    loop {
        views
            .send(View::ChooseNickname { message, notice })
            .await
            .map_err(|_| SessionEnd::Closed)?;
        message = match commands.recv().await.map_err(|_| SessionEnd::Closed)? {
            Command::SignOut => return Err(SessionEnd::SignOut),
            Command::SetInitialNickname(nickname) => {
                let nickname = nickname.trim();
                if !(2..=32).contains(&nickname.encode_utf16().count()) {
                    Some("Use a nickname between 2 and 32 characters.")
                } else {
                    match client.set_nickname(nickname).await {
                        Ok(display_name) => {
                            profile.display_name = Some(display_name);
                            return Ok(profile);
                        }
                        Err(error) if error.unauthorized() => {
                            return Err(SessionEnd::Invalidated(SESSION_EXPIRED));
                        }
                        Err(Failure::Http(StatusCode::CONFLICT)) => {
                            Some("That nickname is taken. Choose another.")
                        }
                        Err(error) => Some(redemption_error(
                            error,
                            "Could not save your nickname. Try again.",
                        )),
                    }
                }
            }
            _ => message,
        };
    }
}
