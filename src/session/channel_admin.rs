use super::Sidebar;
use crate::api::{Channel, ChannelMutation, Failure};
use reqwest::StatusCode;

pub use crate::api::ChannelKind;

pub enum ChannelAction {
    Create(ChannelKind, String),
    Rename(String, String),
    Move { id: String, down: bool },
    Delete(String),
}

#[derive(Clone, Copy, Default)]
pub struct ChannelAdmin {
    pub revision: u64,
    pub pending: bool,
    pub succeeded: bool,
    pub validation: Option<&'static str>,
}

pub(super) enum ChannelMutationError {
    Validation(&'static str),
    Action(&'static str),
}

impl Sidebar {
    pub fn channels_of_kind(&self, kind: ChannelKind) -> impl Iterator<Item = &Channel> + Clone {
        self.channels
            .iter()
            .flatten()
            .filter(move |channel| channel.kind == kind.as_str())
    }

    pub(super) fn channel_mutation(
        &self,
        action: ChannelAction,
    ) -> Result<(ChannelMutation, &'static str), ChannelMutationError> {
        if !self.is_admin() {
            return Err(ChannelMutationError::Action(
                "You don’t have permission to manage channels.",
            ));
        }
        let locate = |id: &str| {
            self.channels
                .iter()
                .flatten()
                .find(|channel| channel.id == id)
                .and_then(ChannelKind::from_channel)
                .ok_or(ChannelMutationError::Action(
                    "That channel no longer exists.",
                ))
        };
        Ok(match action {
            ChannelAction::Create(kind, name) => (
                ChannelMutation::Create(
                    kind,
                    channel_name(name).map_err(ChannelMutationError::Validation)?,
                ),
                "Channel created.",
            ),
            ChannelAction::Rename(id, name) => {
                locate(&id)?;
                (
                    ChannelMutation::Rename(
                        id,
                        channel_name(name).map_err(ChannelMutationError::Validation)?,
                    ),
                    "Channel renamed.",
                )
            }
            ChannelAction::Delete(id) => {
                locate(&id)?;
                (ChannelMutation::Delete(id), "Channel deleted.")
            }
            ChannelAction::Move { id, down } => {
                let kind = locate(&id)?;
                let mut ids = self
                    .channels_of_kind(kind)
                    .map(|channel| channel.id.clone())
                    .collect::<Vec<_>>();
                let index = ids
                    .iter()
                    .position(|current| *current == id)
                    .expect("located channel is in its section");
                let next = if down {
                    (index + 1 < ids.len()).then_some(index + 1)
                } else {
                    index.checked_sub(1)
                };
                let next = next.ok_or(ChannelMutationError::Action(
                    "That channel is already at the edge of its section.",
                ))?;
                ids.swap(index, next);
                (ChannelMutation::Reorder(kind, ids), "Channel reordered.")
            }
        })
    }
}

fn channel_name(mut name: String) -> Result<String, &'static str> {
    name.retain(|ch| !matches!(ch, '\u{0}'..='\u{1f}' | '\u{7f}'));
    let name =
        name.trim_matches(|ch: char| ch.is_whitespace() && ch != '\u{85}' || ch == '\u{feff}');
    if !(1..=32).contains(&name.encode_utf16().count()) {
        return Err("Channel names must be 1–32 characters.");
    }
    Ok(name.to_owned())
}

pub(super) fn failure_notice(failure: &Failure) -> &'static str {
    match failure {
        Failure::Http(StatusCode::FORBIDDEN) => "You don’t have permission to manage channels.",
        Failure::Http(StatusCode::BAD_REQUEST) => "SPK rejected the channel changes.",
        Failure::Http(StatusCode::NOT_FOUND) => "That channel no longer exists.",
        Failure::Http(StatusCode::CONFLICT) => "The channel list changed. Try again.",
        _ => "Could not update channels. Try again.",
    }
}
