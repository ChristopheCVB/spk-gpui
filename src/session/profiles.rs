use std::{fs::File, io::Read as _, path::Path};

use image::ImageFormat;
use reqwest::StatusCode;
use tokio::{sync::oneshot, task::JoinSet};

use super::{Command, Sidebar};
use crate::api::{Failure, MAX_AVATAR_UPLOAD, SpkClient};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileEdit {
    Idle,
    Saving,
    Uploading,
    Saved,
    Uploaded,
    Failed(&'static str),
}

impl ProfileEdit {
    pub fn busy(self) -> bool {
        matches!(self, Self::Saving | Self::Uploading)
    }
}

pub(super) enum Work {
    Nickname(Result<String, Failure>),
    Avatar(Result<String, Failure>),
    Email(
        oneshot::Sender<Option<String>>,
        Result<Option<String>, Failure>,
    ),
}

impl Work {
    pub(super) fn start(
        tasks: &mut JoinSet<Self>,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        command: Command,
    ) {
        let client = client.clone();
        match command {
            Command::SaveNickname(name) if !sidebar.profile_edit.busy() => {
                if !(2..=32).contains(&name.trim().encode_utf16().count()) {
                    sidebar.profile_edit =
                        ProfileEdit::Failed("Nickname must be 2 to 32 characters.");
                    return;
                }
                sidebar.profile_edit = ProfileEdit::Saving;
                tasks.spawn(async move { Self::Nickname(client.set_nickname(name.trim()).await) });
            }
            Command::UploadAvatar(path) if !sidebar.profile_edit.busy() => {
                sidebar.profile_edit = ProfileEdit::Uploading;
                tasks.spawn(async move {
                    let result = match tokio::task::spawn_blocking(move || read_avatar(&path)).await
                    {
                        Ok(Some((bytes, mime))) => client.upload_avatar(bytes, mime).await,
                        _ => Err(Failure::InvalidResponse),
                    };
                    Self::Avatar(result)
                });
            }
            Command::InspectProfileEmail(id, reply) => {
                if sidebar.is_admin() && sidebar.inspect_person(&id).is_some() {
                    tasks.spawn(async move {
                        let result = client.admin_profile_email(&id).await;
                        Self::Email(reply, result)
                    });
                } else {
                    let _ = reply.send(None);
                }
            }
            _ => {}
        }
    }

    pub(super) fn complete(self, sidebar: &mut Sidebar) -> Result<bool, Failure> {
        let (result, fallback) = match self {
            Self::Nickname(Ok(name)) => {
                sidebar.profile.display_name = Some(name);
                sidebar.profile_edit = ProfileEdit::Saved;
                return Ok(true);
            }
            Self::Avatar(Ok(key)) => {
                sidebar.profile.avatar_key = Some(key);
                sidebar.profile_edit = ProfileEdit::Uploaded;
                return Ok(true);
            }
            Self::Email(reply, result) => {
                match result {
                    Err(error) if error.unauthorized() => return Err(error),
                    result => {
                        let _ = reply.send(result.ok().flatten().filter(|_| sidebar.is_admin()));
                    }
                }
                return Ok(false);
            }
            Self::Nickname(Err(Failure::Http(StatusCode::CONFLICT))) => {
                sidebar.profile_edit = ProfileEdit::Failed("That nickname is taken.");
                return Ok(true);
            }
            Self::Nickname(Err(error)) => (error, "Could not save the nickname. Try again."),
            Self::Avatar(Err(error)) => (
                error,
                "Avatar upload failed. Choose a PNG, JPEG, WebP, or GIF up to 5 MB and try again.",
            ),
        };
        if result.unauthorized() {
            return Err(result);
        }
        sidebar.profile_edit = ProfileEdit::Failed(fallback);
        Ok(true)
    }
}

fn read_avatar(path: &Path) -> Option<(Vec<u8>, &'static str)> {
    if !path.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_AVATAR_UPLOAD as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_AVATAR_UPLOAD {
        return None;
    }
    let mime = match image::guess_format(&bytes).ok()? {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Gif => "image/gif",
        _ => return None,
    };
    Some((bytes, mime))
}
