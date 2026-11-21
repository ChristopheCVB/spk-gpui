use super::*;

use super::super::Sidebar;
use crate::api::{MessageAction, MessageChange};

#[derive(Clone)]
pub enum Pins {
    Loading,
    Ready(Arc<Vec<ChatMessage>>),
    Failed,
}

#[derive(Clone)]
pub struct ActionOutcome {
    pub message_id: String,
    pub error: Option<&'static str>,
}

pub(in crate::session) struct PendingAction {
    message_id: String,
    saw_change: bool,
}

impl ChatView {
    pub fn can_pin(&self, is_admin: bool) -> bool {
        self.direct_message_peer.is_some() || is_admin
    }
}

impl Sidebar {
    pub fn can_remove_message_content(&self, message: &ChatMessage) -> bool {
        self.is_admin() || self.profile.id.as_deref() == Some(message.user_id.as_str())
    }
}

impl ChatSession {
    pub fn mutate(
        &mut self,
        client: &SpkClient,
        sidebar: &Sidebar,
        channel_id: &str,
        message_id: String,
        action: MessageAction,
    ) {
        let Some(view) = &mut self.selected else {
            return;
        };
        if self.pending_action.is_some() || view.channel_id != channel_id || !view.can_send() {
            return;
        }
        let pins = match &view.pins {
            Some(Pins::Ready(messages)) => messages.as_slice(),
            _ => &[],
        };
        let Some(message) = view
            .messages
            .iter()
            .chain(pins)
            .find(|message| message.id == message_id)
        else {
            return;
        };
        let own = sidebar.profile.id.as_deref() == Some(message.user_id.as_str());
        let allowed = match &action {
            MessageAction::Edit(_) => own,
            MessageAction::Delete | MessageAction::RemovePreview(_) => {
                sidebar.can_remove_message_content(message)
            }
            MessageAction::Pin(_) => view.can_pin(sidebar.is_admin()),
            MessageAction::React { .. } => true,
        };
        if !allowed {
            view.action_outcome = Some(ActionOutcome {
                message_id,
                error: Some("You don’t have permission to change this message."),
            });
            return;
        }
        let client = client.clone();
        let channel_id = channel_id.to_owned();
        self.pending_action = Some(PendingAction {
            message_id: message_id.clone(),
            saw_change: false,
        });
        view.action_pending = true;
        view.action_outcome = None;
        self.action_tasks.spawn(async move {
            client
                .mutate_message(&channel_id, &message_id, action)
                .await
        });
    }

    pub fn action_complete(&mut self, client: &SpkClient, result: Result<MessageChange, Failure>) {
        let (Some(pending), Some(view)) = (self.pending_action.take(), self.selected.as_mut())
        else {
            return;
        };
        view.action_pending = false;
        view.action_outcome = Some(ActionOutcome {
            message_id: pending.message_id.clone(),
            error: match result {
                Ok(_) => None,
                Err(Failure::Http(reqwest::StatusCode::FORBIDDEN)) => {
                    Some("You don’t have permission to change this message.")
                }
                Err(Failure::Http(reqwest::StatusCode::NOT_FOUND)) => {
                    Some("This message is unavailable.")
                }
                Err(Failure::Http(reqwest::StatusCode::BAD_REQUEST)) => {
                    Some("SPK rejected the change. Check the message or emoji.")
                }
                Err(Failure::Http(reqwest::StatusCode::TOO_MANY_REQUESTS)) => {
                    Some("Too many changes. Try again shortly.")
                }
                Err(_) => Some("Could not change the message. Try again."),
            },
        });
        // SPK broadcasts to the initiating session as well as peers. A late HTTP response
        // must not overwrite any newer state for this message received on the socket.
        if !pending.saw_change
            && let Ok(change) = result
        {
            let channel_id = view.channel_id.clone();
            self.apply_message_change(client, &channel_id, &pending.message_id, change);
        }
    }

    pub fn message_changed(
        &mut self,
        client: &SpkClient,
        channel_id: &str,
        message_id: &str,
        change: MessageChange,
    ) {
        if self
            .selected
            .as_ref()
            .is_some_and(|view| view.channel_id == channel_id)
            && let Some(pending) = &mut self.pending_action
            && pending.message_id == message_id
        {
            pending.saw_change = true;
        }
        self.apply_message_change(client, channel_id, message_id, change);
    }

    fn apply_message_change(
        &mut self,
        client: &SpkClient,
        channel_id: &str,
        message_id: &str,
        change: MessageChange,
    ) {
        if matches!(change, MessageChange::Delete) {
            self.delete_message(channel_id, message_id);
        }
        let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.channel_id == channel_id && !view.unavailable())
        else {
            return;
        };
        if view.loading() {
            self.buffered_changes
                .push((message_id.to_owned(), change.clone()));
        }
        change_message(&mut view.messages, message_id, &change);
        let refresh = match &mut view.pins {
            Some(Pins::Ready(pins)) => {
                change_message(pins, message_id, &change);
                matches!(change, MessageChange::Pin(_))
                    // An older pins response could restore a deleted message or stale content.
                    || !self.pin_tasks.is_empty()
            }
            Some(Pins::Loading | Pins::Failed) => true,
            None => false,
        };
        if matches!(
            change,
            MessageChange::Edit(_)
                | MessageChange::Delete
                | MessageChange::Previews(_)
                | MessageChange::RemovePreview(_)
        ) {
            self.prune_previews();
        }
        if refresh {
            self.refresh_pins(client);
        }
    }

    pub fn show_pins(&mut self, client: &SpkClient, channel_id: &str, shown: bool) {
        let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.channel_id == channel_id && !view.unavailable())
        else {
            return;
        };
        self.pin_tasks = JoinSet::new();
        view.pins = shown.then_some(Pins::Loading);
        if shown && view.history == History::Ready {
            self.refresh_pins(client);
        }
    }

    pub fn refresh_pins(&mut self, client: &SpkClient) {
        let Some(view) = self
            .selected
            .as_ref()
            .filter(|view| view.pins.is_some() && !view.unavailable())
        else {
            return;
        };
        let client = client.clone();
        let channel_id = view.channel_id.clone();
        self.pin_tasks = JoinSet::new();
        self.pin_tasks
            .spawn(async move { client.pins(&channel_id).await });
    }

    pub fn pins_complete(&mut self, result: Result<Vec<ChatMessage>, Failure>) {
        if let Some(view) = &mut self.selected
            && view.pins.is_some()
        {
            view.pins = Some(match result {
                Ok(pins) => Pins::Ready(Arc::new(pins)),
                Err(_) => Pins::Failed,
            });
        }
    }

    pub(super) fn reset_actions(&mut self) {
        self.pending_action = None;
        self.action_tasks = JoinSet::new();
        self.preview_tasks = JoinSet::new();
        self.pin_tasks = JoinSet::new();
    }
}

pub(super) fn change_message(
    messages: &mut Arc<Vec<ChatMessage>>,
    id: &str,
    change: &MessageChange,
) {
    if matches!(change, MessageChange::Delete) {
        remove_message(Arc::make_mut(messages), id);
    } else if let Some(index) = messages.iter().position(|message| message.id == id) {
        let message = &mut Arc::make_mut(messages)[index];
        match change {
            MessageChange::Edit(edited) => *message = edited.clone(),
            MessageChange::Reactions(reactions) => message.reactions.clone_from(reactions),
            MessageChange::Pin(pinned) => message.pinned = *pinned,
            MessageChange::Previews(previews) => message.previews.clone_from(previews),
            MessageChange::RemovePreview(url) => {
                message.previews.retain(|preview| preview.url != *url)
            }
            MessageChange::Delete => unreachable!(),
        }
    }
}
