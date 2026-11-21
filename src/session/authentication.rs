use async_channel::{Receiver, Sender};
use cookie_store::{CookieStore, serde::json};
use reqwest::StatusCode;
use tokio::task::spawn_blocking;
use url::Url;

use super::{Command, View};
use crate::api::{Failure, Profile, SpkClient};

pub(super) const SESSION_EXPIRED: &str = "Your session expired. Sign in again.";
const STORE_UNAVAILABLE: &str =
    "Secure storage is unavailable. Your sign-in lasts until you close SPK.";
const DELETE_FAILED: &str = "Could not remove the saved session from secure storage.";

pub(super) struct SavedSession {
    origin: String,
    persisted: Option<Vec<u8>>,
    #[cfg(test)]
    storage: Option<TestStore>,
}

#[cfg(test)]
type TestStore = std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>;

impl SavedSession {
    pub(super) fn new(base: &Url) -> Self {
        Self {
            origin: base.origin().ascii_serialization(),
            persisted: None,
            #[cfg(test)]
            storage: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_test_store(bytes: Option<Vec<u8>>) -> (Self, TestStore) {
        let storage = TestStore::new(std::sync::Mutex::new(bytes));
        (
            Self {
                origin: String::new(),
                persisted: None,
                storage: Some(storage.clone()),
            },
            storage,
        )
    }

    async fn read(&mut self) -> Result<Option<CookieStore>, &'static str> {
        let bytes = self.read_bytes().await?;
        let Some(bytes) = bytes else { return Ok(None) };
        match json::load(bytes.as_slice()) {
            Ok(cookies) => {
                self.persisted = Some(bytes);
                Ok(Some(cookies))
            }
            Err(_) => {
                self.clear().await?;
                Err("The saved session could not be read. Sign in again.")
            }
        }
    }

    async fn read_bytes(&self) -> Result<Option<Vec<u8>>, &'static str> {
        #[cfg(test)]
        if let Some(storage) = &self.storage {
            return Ok(storage.lock().unwrap().clone());
        }
        let origin = self.origin.clone();
        spawn_blocking(
            move || match keyring::Entry::new("spk-gpui", &origin)?.get_secret() {
                Ok(bytes) => Ok(Some(bytes)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(error),
            },
        )
        .await
        .map_err(|_| STORE_UNAVAILABLE)?
        .map_err(|_| STORE_UNAVAILABLE)
    }

    pub(super) async fn save(&mut self, client: &SpkClient) -> Result<(), &'static str> {
        let bytes = {
            let cookies = client.cookies().lock().map_err(|_| STORE_UNAVAILABLE)?;
            let mut bytes = Vec::new();
            json::save(&cookies, &mut bytes).map_err(|_| STORE_UNAVAILABLE)?;
            bytes
        };

        if self.persisted.as_ref() == Some(&bytes) {
            return Ok(());
        }
        if bytes == b"[]" {
            self.clear().await?;
            return Err(
                "SPK did not issue a persistent session. Sign in again after closing the app.",
            );
        }

        self.persisted = Some(self.write_bytes(bytes).await?);
        Ok(())
    }

    async fn write_bytes(&self, bytes: Vec<u8>) -> Result<Vec<u8>, &'static str> {
        #[cfg(test)]
        if let Some(storage) = &self.storage {
            *storage.lock().unwrap() = Some(bytes.clone());
            return Ok(bytes);
        }
        let origin = self.origin.clone();
        spawn_blocking(move || {
            keyring::Entry::new("spk-gpui", &origin)?.set_secret(&bytes)?;
            Ok::<_, keyring::Error>(bytes)
        })
        .await
        .map_err(|_| STORE_UNAVAILABLE)?
        .map_err(|_| STORE_UNAVAILABLE)
    }

    pub(super) async fn clear(&mut self) -> Result<(), &'static str> {
        self.delete_bytes().await?;
        self.persisted = None;
        Ok(())
    }

    async fn delete_bytes(&self) -> Result<(), &'static str> {
        #[cfg(test)]
        if let Some(storage) = &self.storage {
            *storage.lock().unwrap() = None;
            return Ok(());
        }
        let origin = self.origin.clone();
        spawn_blocking(move || {
            match keyring::Entry::new("spk-gpui", &origin)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(|_| DELETE_FAILED)?
        .map_err(|_| DELETE_FAILED)
    }
}

pub(super) async fn restore(
    base: &Url,
    saved: &mut SavedSession,
) -> Result<Option<(SpkClient, Profile)>, (bool, &'static str)> {
    const RETRY: (bool, &str) = (true, "Could not verify your saved session. Try again.");
    let Some(cookies) = saved.read().await.map_err(|message| (false, message))? else {
        return Ok(None);
    };
    let client = SpkClient::new(base.clone(), cookies).map_err(|_| RETRY)?;
    match client.profile().await {
        Ok(profile) => Ok(Some((client, profile))),
        Err(failure) if failure.unauthorized() => {
            let warning = saved.clear().await.err().unwrap_or(SESSION_EXPIRED);
            Err((false, warning))
        }
        Err(_) => Err(RETRY),
    }
}

pub(super) async fn login(
    commands: &Receiver<Command>,
    views: &Sender<View>,
    message: Option<&'static str>,
    retry_restore: bool,
) -> Option<Command> {
    views
        .send(View::SignIn {
            message,
            retry_restore,
        })
        .await
        .ok()?;
    while let Ok(command) = commands.recv().await {
        if matches!(
            command,
            Command::SignIn { .. } | Command::Restore | Command::OpenAccountLink
        ) {
            return Some(command);
        }
    }
    None
}

pub(super) async fn sign_in(
    base: &Url,
    email: String,
    password: String,
) -> Result<(SpkClient, Profile), Failure> {
    let client = SpkClient::new(base.clone(), CookieStore::default())?;
    client.sign_in(email, password).await?;
    let profile = client.profile().await?;
    Ok((client, profile))
}

pub(super) async fn sign_out(
    client: &SpkClient,
    saved: &mut SavedSession,
    commands: &Receiver<Command>,
    views: &Sender<View>,
) -> Option<Option<&'static str>> {
    loop {
        views.send(View::Busy("Signing out…")).await.ok()?;
        let (remote, local) = tokio::join!(client.sign_out(), saved.clear());
        match (remote, local) {
            (Ok(()), cleared) => return Some(cleared.err()),
            (Err(error), cleared) if error.unauthorized() => return Some(cleared.err()),
            (Err(_), Ok(())) => {
                return Some(Some(
                    "Signed out on this device. SPK could not confirm server sign-out.",
                ));
            }
            (Err(_), Err(_)) => {
                views.send(View::RetrySignOut).await.ok()?;
                loop {
                    if matches!(commands.recv().await.ok()?, Command::SignOut) {
                        break;
                    }
                }
            }
        }
    }
}

pub(super) fn sign_in_message(failure: Failure) -> &'static str {
    match failure {
        Failure::Http(StatusCode::UNAUTHORIZED | StatusCode::BAD_REQUEST) => {
            "Check your email and password and try again."
        }
        Failure::Http(StatusCode::FORBIDDEN) => "SPK denied sign-in for this account.",
        Failure::Http(StatusCode::TOO_MANY_REQUESTS) => {
            "Too many sign-in attempts. Wait a minute and try again."
        }
        Failure::Connection => "Could not reach SPK. Check your connection and try again.",
        _ => "SPK could not complete sign-in. Try again.",
    }
}
