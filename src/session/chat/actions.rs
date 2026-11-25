use super::*;

use super::super::Sidebar;
use crate::api::{MessageAction, MessageChange, PollEditOption};

const CHANGE_DENIED: &str = "You don’t have permission to change this message.";

fn message_action_error(
    error: &Failure,
    bad_request: &'static str,
    fallback: &'static str,
) -> &'static str {
    match error {
        Failure::Http(reqwest::StatusCode::FORBIDDEN) => CHANGE_DENIED,
        Failure::Http(reqwest::StatusCode::NOT_FOUND) => "This message is unavailable.",
        Failure::Http(reqwest::StatusCode::TOO_MANY_REQUESTS) => {
            "Too many changes. Try again shortly."
        }
        Failure::Http(reqwest::StatusCode::BAD_REQUEST) => bad_request,
        _ => fallback,
    }
}

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

#[derive(Debug, PartialEq, Eq)]
pub enum PollEditOutcome {
    Saved,
    ConfirmVoteChange,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PollEditError {
    Validation(&'static str),
    Action(&'static str),
}

pub type PollEditReply = tokio::sync::oneshot::Sender<Result<PollEditOutcome, PollEditError>>;

pub(in crate::session) type VoteResult = (String, String, Result<MessageChange, Failure>);

pub(in crate::session) struct PendingAction {
    message_id: String,
    saw_change: bool,
    editing: bool,
    poll_edit_reply: Option<PollEditReply>,
}

impl ChatView {
    pub fn can_delete_messages(&self) -> bool {
        self.can_send() || (self.webhook && self.history_ready())
    }

    fn message(&self, id: &str) -> Option<&ChatMessage> {
        let pins = match &self.pins {
            Some(Pins::Ready(messages)) => messages.as_slice(),
            _ => &[],
        };
        self.messages
            .iter()
            .chain(pins)
            .find(|message| message.id == id)
    }

    pub fn can_pin(&self, is_admin: bool) -> bool {
        !self.webhook && (self.direct_message_peer.is_some() || is_admin)
    }
}

impl Sidebar {
    pub fn can_remove_message_content(&self, message: &ChatMessage) -> bool {
        if let Some(feed) = self.webhook_feed(&message.channel_id) {
            self.can_manage_webhook(feed)
        } else {
            self.is_admin() || self.profile.id.as_deref() == Some(message.user_id.as_str())
        }
    }
}

impl ChatSession {
    pub fn mutate(
        &mut self,
        client: &SpkClient,
        sidebar: &mut Sidebar,
        channel_id: &str,
        message_id: String,
        action: MessageAction,
    ) {
        let Some(view) = &mut self.selected else {
            return;
        };
        let voting = matches!(action, MessageAction::Vote(_));
        if (!voting && self.pending_action.is_some())
            || view.channel_id != channel_id
            || !match &action {
                MessageAction::Delete => view.can_delete_messages(),
                _ => view.can_send(),
            }
        {
            return;
        }
        let Some(message) = view.message(&message_id) else {
            return;
        };
        let own = sidebar.profile.id.as_deref() == Some(message.user_id.as_str());
        let allowed = match &action {
            MessageAction::Edit(_) => own,
            MessageAction::EditPoll { .. } => false,
            MessageAction::Delete | MessageAction::RemovePreview(_) => {
                sidebar.can_remove_message_content(message)
            }
            MessageAction::Pin(_) => view.can_pin(sidebar.is_admin()),
            MessageAction::React { .. } => true,
            MessageAction::Vote(_) => message.poll.is_some(),
        };
        if !allowed {
            sidebar
                .action_results
                .push(ActionResult::error(CHANGE_DENIED));
            if !voting {
                view.action_outcome = Some(Arc::new(ActionOutcome {
                    message_id,
                    error: Some(CHANGE_DENIED),
                }));
            }
        } else if voting {
            let client = client.clone();
            let channel_id = channel_id.to_owned();
            self.vote_tasks.spawn(async move {
                let result = client
                    .mutate_message(&channel_id, &message_id, action)
                    .await;
                (channel_id, message_id, result)
            });
        } else if matches!(&action, MessageAction::Edit(body) if *body == message.body) {
            view.action_outcome = Some(Arc::new(ActionOutcome {
                message_id,
                error: None,
            }));
        } else {
            self.start_message_action(client, channel_id, message_id, action, None);
        }
    }

    pub fn vote_complete(
        &mut self,
        client: &SpkClient,
        channel_id: &str,
        message_id: &str,
        result: Result<MessageChange, Failure>,
    ) -> Option<ActionResult> {
        // The web client applies both HTTP and socket results as they arrive.
        match result {
            Ok(change) => {
                self.apply_message_change(client, channel_id, message_id, change);
                None
            }
            Err(error) => Some(ActionResult::error(message_action_error(
                &error,
                "SPK rejected the vote. Try again.",
                "Could not vote. Try again.",
            ))),
        }
    }

    pub fn prepare_poll_edit(
        &self,
        sidebar: &Sidebar,
        channel_id: &str,
        message_id: &str,
        question: &str,
        mut options: Vec<PollEditOption>,
        reset_votes: Option<bool>,
    ) -> Result<Option<MessageAction>, PollEditError> {
        let view = self
            .selected
            .as_ref()
            .filter(|view| view.channel_id == channel_id && view.can_send())
            .ok_or(PollEditError::Action("This conversation is unavailable."))?;
        let message = view
            .message(message_id)
            .ok_or(PollEditError::Action("This message is unavailable."))?;
        if sidebar.profile.id.as_deref() != Some(message.user_id.as_str()) {
            return Err(PollEditError::Action(CHANGE_DENIED));
        }
        let poll = message
            .poll
            .as_ref()
            .ok_or(PollEditError::Action("This poll is unavailable."))?;
        if self.pending_action.is_some() {
            return Err(PollEditError::Action(
                "A message change is in progress. Try again.",
            ));
        }

        let (body, labels) =
            validate_poll(question, options.iter().map(|option| option.label.as_str()))
                .map_err(PollEditError::Validation)?;
        let changed = poll.options.len() != options.len()
            || poll.options.iter().zip(options.iter().zip(&labels)).any(
                |(existing, (option, label))| {
                    option.id.as_deref() != Some(existing.id.as_str()) || existing.label != *label
                },
            );
        if reset_votes.is_none() && changed && poll.total_votes() > 0 {
            return Ok(None);
        }
        let body = body.to_owned();
        let trimmed_labels = labels.into_iter().map(str::to_owned).collect::<Vec<_>>();
        for (option, label) in options.iter_mut().zip(trimmed_labels) {
            option.label = label;
        }
        Ok(Some(MessageAction::EditPoll {
            body,
            options,
            reset_votes: reset_votes.unwrap_or(false),
        }))
    }

    pub fn start_message_action(
        &mut self,
        client: &SpkClient,
        channel_id: &str,
        message_id: String,
        action: MessageAction,
        poll_edit_reply: Option<PollEditReply>,
    ) {
        let view = self
            .selected
            .as_mut()
            .expect("message action has a selected conversation");
        let client = client.clone();
        let channel_id = channel_id.to_owned();
        self.pending_action = Some(PendingAction {
            message_id: message_id.clone(),
            saw_change: false,
            editing: matches!(
                action,
                MessageAction::Edit(_) | MessageAction::EditPoll { .. }
            ),
            poll_edit_reply,
        });
        view.action_pending = true;
        view.action_outcome = None;
        self.action_tasks.spawn(async move {
            client
                .mutate_message(&channel_id, &message_id, action)
                .await
        });
    }

    pub fn action_complete(
        &mut self,
        client: &SpkClient,
        result: Result<MessageChange, Failure>,
    ) -> Option<ActionResult> {
        let pending = self.pending_action.take()?;
        let view = self.selected.as_mut()?;
        view.action_pending = false;
        let error = result.as_ref().err().map(|error| {
            let (bad_request, fallback) = if pending.poll_edit_reply.is_some() {
                (
                    "SPK rejected the poll edit. Check the question and options.",
                    "Could not save the poll. Try again.",
                )
            } else {
                (
                    "SPK rejected the change. Check the message or emoji.",
                    "Could not change the message. Try again.",
                )
            };
            message_action_error(error, bad_request, fallback)
        });
        let field_error = pending.poll_edit_reply.is_some()
            && matches!(
                &result,
                Err(Failure::Http(reqwest::StatusCode::BAD_REQUEST))
            );
        view.action_outcome = Some(Arc::new(ActionOutcome {
            message_id: pending.message_id.clone(),
            error,
        }));
        if let Ok(change) = result
            && !pending.saw_change
        {
            let channel_id = view.channel_id.clone();
            self.apply_message_change(client, &channel_id, &pending.message_id, change);
        }
        if let Some(reply) = pending.poll_edit_reply {
            let _ = reply.send(error.map_or(Ok(PollEditOutcome::Saved), |message| {
                Err(if field_error {
                    PollEditError::Validation(message)
                } else {
                    PollEditError::Action(message)
                })
            }));
        }
        error.filter(|_| !field_error).map(ActionResult::error)
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
            && if pending.editing {
                matches!(change, MessageChange::Edit(_) | MessageChange::Delete)
            } else {
                !matches!(change, MessageChange::Poll(_))
            }
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
                if matches!(change, MessageChange::Pin(false)) {
                    remove_message(Arc::make_mut(pins), message_id);
                } else {
                    change_message(pins, message_id, &change);
                }
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

    pub fn retry_pins(&mut self, client: &SpkClient, channel_id: &str) {
        if let Some(view) = &mut self.selected
            && view.channel_id == channel_id
            && matches!(view.pins, Some(Pins::Failed))
        {
            view.pins = Some(Pins::Loading);
            self.refresh_pins(client);
        }
    }

    pub fn refresh_pins(&mut self, client: &SpkClient) {
        let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| !view.webhook && !view.unavailable())
        else {
            return;
        };
        view.pins.get_or_insert(Pins::Loading);
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
            MessageChange::Edit(edited) => message.clone_from(edited),
            MessageChange::Reactions(reactions) => message.reactions.clone_from(reactions),
            MessageChange::Poll(poll) => message.poll = Some(poll.clone()),
            MessageChange::Pin(pinned) => message.pinned = *pinned,
            MessageChange::Previews(previews) => message.previews.clone_from(previews),
            MessageChange::RemovePreview(url) => {
                message.previews.retain(|preview| preview.url != *url)
            }
            MessageChange::Delete => unreachable!(),
        }
    }
}
