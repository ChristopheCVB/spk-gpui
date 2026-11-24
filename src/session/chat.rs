mod actions;
mod attachments;
mod gifs;
mod preview_images;

pub use actions::Pins;
use actions::{ActionOutcome, PendingAction, change_message};
pub use attachments::{AttachmentInput, UploadStatus, image_preview};
use attachments::{AttachmentWork, QueuedAttachment};
pub use gifs::GifPickerView;
use gifs::{GifPicker, StagedGif};
use preview_images::{PreviewImages, PreviewResult};

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use tokio::{task::JoinSet, time::Instant};

use crate::api::{
    AttachmentRef, Channel, ChatMessage, Failure, MessageChange, MessagePage, ReplyRef, SpkClient,
    VoiceSocket,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum History {
    Loading,
    Ready,
    Failed,
    Unavailable,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ChatJump {
    pub message_id: String,
    pub generation: u64,
}

#[derive(Clone)]
pub struct ChatView {
    pub channel_id: String,
    pub direct_message_peer: Option<String>,
    pub messages: Arc<Vec<ChatMessage>>,
    pub has_more_before: bool,
    pub first_unread_id: Option<String>,
    pub has_more_after: bool,
    pub draft: Draft,
    pub typing: Vec<String>,
    pub jump: Option<ChatJump>,
    pub jump_failed: bool,
    hold_read: bool,
    pending_unread: Option<u64>,
    pub outgoing: Option<Outgoing>,
    pub pins: Option<Pins>,
    pub preview_images: PreviewImages,
    pub gifs: Option<GifPickerView>,
    pub poll_send_error: Option<String>,
    pub action_pending: bool,
    pub action_outcome: Option<ActionOutcome>,
    pub paging_forward: bool,
    opening_latest: bool,
    history: History,
}

impl ChatView {
    pub fn loading(&self) -> bool {
        self.history == History::Loading
    }

    pub fn history_failed(&self) -> bool {
        self.history == History::Failed
    }

    pub fn unavailable(&self) -> bool {
        self.history == History::Unavailable
    }

    pub fn history_ready(&self) -> bool {
        self.history == History::Ready
    }

    pub fn latest_ready(&self) -> bool {
        self.history_ready() && !self.has_more_after
    }

    pub fn opening_latest(&self) -> bool {
        self.loading() && self.opening_latest
    }

    pub fn can_send(&self) -> bool {
        !self.unavailable()
            && (self.direct_message_peer.is_none() || self.history == History::Ready)
    }

    fn mark_jump(&mut self, message_id: String) {
        let generation = self.jump.as_ref().map_or(0, |jump| jump.generation + 1);
        self.jump = Some(ChatJump {
            message_id,
            generation,
        });
        self.jump_failed = false;
    }

    fn merge(&mut self, messages: impl IntoIterator<Item = ChatMessage>) {
        let current = Arc::make_mut(&mut self.messages);
        current.extend(messages);
        current.sort_by(|left, right| position(left).cmp(&position(right)));
        current.dedup_by(|right, left| right.id == left.id);
    }
}

fn position(message: &ChatMessage) -> (i64, &str) {
    (message.created_at, &message.id)
}

#[derive(Clone, Default)]
pub struct Draft {
    pub body: String,
    pub gif: Option<StagedGif>,
    pub reply: Option<ReplyRef>,
    pub uploads: Vec<QueuedAttachment>,
    pub upload_notice: Option<&'static str>,
}

impl Draft {
    pub fn can_send(&self, body: &str) -> bool {
        !self.attachments_block_send()
            && (!body.trim().is_empty() || self.gif.is_some() || self.attachments_ready())
    }
}

#[derive(Clone)]
pub struct Outgoing {
    pub body: String,
    pub status: Delivery,
    nonce: String,
    gif_url: Option<String>,
    reply: Option<ReplyRef>,
    attachments: Vec<AttachmentRef>,
}

impl Outgoing {
    fn matches_attachment(&self, message: &ChatMessage) -> bool {
        // SPK claims each uploaded attachment for a single message in the send transaction.
        self.attachments
            .first()
            .is_some_and(|sent| message.attachments.iter().any(|file| file.id == sent.id))
    }
}

#[derive(Clone)]
pub enum Delivery {
    Sending(Instant),
    Failed(String),
    Unconfirmed,
}

#[derive(Clone)]
enum HistoryRequest {
    Refresh { load_newer: bool },
    Older(String),
    Newer(String),
    Around(String),
    Latest,
}

#[derive(Default)]
pub(super) struct ChatSession {
    selected: Option<ChatView>,
    outgoing: HashMap<String, Outgoing>,
    pending_polls: HashMap<String, (String, Instant)>,
    poll_send_error: Option<String>,
    drafts: HashMap<String, Draft>,
    typers: HashMap<String, Instant>,
    last_typing: Option<Instant>,
    socket: Option<VoiceSocket>,
    history: Option<HistoryRequest>,
    buffered: Vec<ChatMessage>,
    unread_request: u64,
    buffered_changes: Vec<(String, MessageChange)>,
    pending_action: Option<PendingAction>,
    pub action_tasks: JoinSet<Result<MessageChange, Failure>>,
    pub pin_tasks: JoinSet<Result<Vec<ChatMessage>, Failure>>,
    pub preview_tasks: JoinSet<PreviewResult>,
    pub message_preview: JoinSet<()>,
    deleted_during_load: HashSet<String>,
    pub tasks: JoinSet<Result<MessagePage, Failure>>,
    pub(super) attachments: AttachmentWork,
    pub(super) gifs: GifPicker,
}

impl ChatSession {
    pub fn view(&self) -> Option<ChatView> {
        let mut view = self.selected.clone()?;
        view.poll_send_error.clone_from(&self.poll_send_error);
        // Hide cached DM content until SPK confirms access to the conversation.
        if view.direct_message_peer.is_some()
            && view.history != History::Ready
            && matches!(
                self.history,
                Some(
                    HistoryRequest::Refresh { .. }
                        | HistoryRequest::Latest
                        | HistoryRequest::Around(_)
                )
            )
        {
            view.messages = Arc::default();
            view.pins = None;
            view.preview_images = PreviewImages::default();
            view.has_more_before = false;
            view.has_more_after = false;
        } else if !view.unavailable() {
            view.outgoing = self.outgoing.get(&view.channel_id).cloned();
            view.draft = self
                .drafts
                .get(&view.channel_id)
                .cloned()
                .unwrap_or_default();
            if view.can_send() {
                view.gifs = self.gifs.view.clone();
            }
            view.typing = self.typers.keys().cloned().collect();
            view.typing.sort_unstable();
        }
        Some(view)
    }

    pub fn open(&mut self, client: &SpkClient, channel: &Channel) -> bool {
        if !self.select(channel) {
            if channel.kind == "text"
                && self.selected.as_ref().is_some_and(|view| {
                    view.history_ready() && (view.has_more_after || view.jump.is_some())
                })
            {
                self.latest(client);
                return true;
            }
            return false;
        }
        self.load(client, HistoryRequest::Refresh { load_newer: false });
        true
    }

    pub fn open_conversation_target(&mut self, client: &SpkClient, channel: &Channel) {
        self.select(channel);
        self.latest(client);
    }

    pub fn open_target(&mut self, client: &SpkClient, channel: &Channel, message_id: String) {
        self.select(channel);
        if let Some(view) = &mut self.selected {
            Arc::make_mut(&mut view.messages).retain(|message| message.id != message_id);
            view.jump = None;
        }
        self.load(client, HistoryRequest::Around(message_id));
    }

    fn select(&mut self, channel: &Channel) -> bool {
        if self.selected.as_ref().is_some_and(|chat| {
            chat.channel_id == channel.id && !chat.unavailable() && !chat.jump_failed
        }) {
            return false;
        }
        self.typers.clear();
        self.last_typing = None;
        self.reset_actions();
        self.gifs.close();
        self.message_preview = JoinSet::new();
        self.selected = Some(ChatView {
            channel_id: channel.id.clone(),
            direct_message_peer: channel
                .other_user_id
                .clone()
                .filter(|_| channel.kind == "dm"),
            messages: Arc::default(),
            has_more_before: false,
            first_unread_id: None,
            hold_read: false,
            pending_unread: None,
            has_more_after: false,
            draft: Draft::default(),
            typing: Vec::new(),
            jump: None,
            jump_failed: false,
            outgoing: None,
            pins: Some(Pins::Loading),
            preview_images: PreviewImages::default(),
            gifs: None,
            poll_send_error: None,
            action_pending: false,
            action_outcome: None,
            paging_forward: false,
            opening_latest: false,
            history: History::Loading,
        });
        true
    }

    pub(super) fn selected_text_channel(&self) -> Option<&str> {
        self.selected
            .as_ref()
            .filter(|chat| chat.direct_message_peer.is_none())
            .map(|chat| chat.channel_id.as_str())
    }

    pub fn close(&mut self) {
        self.cancel_history();
        self.reset_actions();
        self.gifs.close();
        self.message_preview = JoinSet::new();
        self.selected = None;
        self.typers.clear();
    }

    pub fn connected(&mut self, client: &SpkClient, socket: VoiceSocket) {
        self.socket = Some(socket);
        if matches!(
            self.history,
            Some(HistoryRequest::Around(_) | HistoryRequest::Latest)
        ) && self.selected.as_ref().is_some_and(ChatView::loading)
        {
            return;
        }
        let load_newer = self
            .selected
            .as_ref()
            .is_some_and(|view| view.loading() && view.paging_forward);
        self.load(client, HistoryRequest::Refresh { load_newer });
    }

    pub fn disconnected(&mut self) {
        self.socket = None;
        self.pending_polls.clear();
        self.typers.clear();
        self.last_typing = None;
        for outgoing in self.outgoing.values_mut() {
            if matches!(outgoing.status, Delivery::Sending(_)) {
                outgoing.status = Delivery::Unconfirmed;
            }
        }
    }

    pub fn older(&mut self, client: &SpkClient) {
        if let Some(view) = &self.selected
            && view.history == History::Ready
            && view.has_more_before
            && let Some(oldest) = view.messages.first()
        {
            self.load(client, HistoryRequest::Older(oldest.id.clone()));
        }
    }

    pub fn newer(&mut self, client: &SpkClient) {
        if let Some(view) = &self.selected
            && view.history == History::Ready
            && view.has_more_after
            && let Some(newest) = view.messages.last()
        {
            self.load(client, HistoryRequest::Newer(newest.id.clone()));
        }
    }

    pub fn jump(&mut self, client: &SpkClient, channel_id: &str, message_id: String) {
        let Some(view) = &mut self.selected else {
            return;
        };
        if view.channel_id != channel_id || view.unavailable() {
            return;
        }
        if view.history == History::Ready
            && view.messages.iter().any(|message| message.id == message_id)
        {
            view.mark_jump(message_id);
        } else {
            self.load(client, HistoryRequest::Around(message_id));
        }
    }

    pub fn latest(&mut self, client: &SpkClient) {
        if let Some(view) = &mut self.selected {
            view.jump = None;
            self.load(client, HistoryRequest::Latest);
        }
    }

    pub fn retry_history(&mut self, client: &SpkClient) {
        if self.selected.as_ref().is_some_and(ChatView::history_failed)
            && let Some(request) = self.history.clone()
        {
            self.load(client, request);
        }
    }

    fn cancel_history(&mut self) {
        self.tasks = JoinSet::new();
        self.buffered.clear();
        self.buffered_changes.clear();
        self.deleted_during_load.clear();
    }

    fn load(&mut self, client: &SpkClient, request: HistoryRequest) {
        self.cancel_history();
        let Some(view) = &mut self.selected else {
            return;
        };
        view.history = History::Loading;
        view.opening_latest = matches!(request, HistoryRequest::Latest);
        view.paging_forward = matches!(
            request,
            HistoryRequest::Newer(_) | HistoryRequest::Refresh { load_newer: true }
        );
        view.jump_failed = false;
        let channel = view.channel_id.clone();
        let messages = view.messages.clone();
        let historical = view.has_more_after;
        let client = client.clone();
        self.history = Some(request.clone());
        self.tasks.spawn(async move {
            match request {
                HistoryRequest::Refresh { load_newer } if historical && !messages.is_empty() => {
                    refresh_window(&client, &channel, &messages, load_newer).await
                }
                HistoryRequest::Refresh { .. } => {
                    refresh_history(&client, &channel, messages.first()).await
                }
                HistoryRequest::Older(before) => client.messages(&channel, Some(&before)).await,
                HistoryRequest::Newer(after) => client.messages_after(&channel, &after).await,
                HistoryRequest::Around(id) => {
                    let page = client.messages_around(&channel, &id).await?;
                    if !page.messages.iter().any(|message| message.id == id) {
                        return Err(Failure::InvalidResponse);
                    }
                    Ok(page)
                }
                HistoryRequest::Latest => client.messages(&channel, None).await,
            }
        });
    }

    pub fn complete(
        &mut self,
        client: &SpkClient,
        own_id: Option<&str>,
        result: Result<MessagePage, Failure>,
    ) {
        if let (Some(view), Some(own_id), Ok(page)) = (&self.selected, own_id, &result)
            && let Some(outgoing) = self.outgoing.get(&view.channel_id)
            && !outgoing.attachments.is_empty()
            && page.messages.iter().any(|message| {
                message.channel_id == view.channel_id
                    && message.user_id == own_id
                    && outgoing.matches_attachment(message)
            })
        {
            self.clear_delivered_send(&view.channel_id.clone());
        }
        let (Some(view), Some(request)) = (&mut self.selected, &self.history) else {
            return;
        };
        match result {
            Ok(mut page) => {
                for id in self.deleted_during_load.drain() {
                    remove_message(&mut page.messages, &id);
                }
                if matches!(request, HistoryRequest::Older(_)) {
                    view.merge(page.messages);
                } else if matches!(request, HistoryRequest::Newer(_)) {
                    view.merge(page.messages);
                    view.has_more_after = page.has_more_after;
                } else {
                    if view.first_unread_id.is_none() {
                        view.first_unread_id = page.first_unread_id;
                    }
                    view.messages = Arc::new(page.messages);
                    view.has_more_after = page.has_more_after;
                    if let HistoryRequest::Around(id) = request {
                        if view.messages.iter().any(|message| message.id == *id) {
                            view.mark_jump(id.clone());
                        } else {
                            view.jump = None;
                            view.jump_failed = true;
                        }
                    }
                }
                let buffered = std::mem::take(&mut self.buffered);
                if !view.has_more_after {
                    view.merge(buffered);
                }
                for (id, change) in self.buffered_changes.drain(..) {
                    change_message(&mut view.messages, &id, &change);
                }
                if !matches!(request, HistoryRequest::Newer(_)) {
                    view.has_more_before = page.has_more_before;
                }
                view.history = History::Ready;
                if matches!(
                    request,
                    HistoryRequest::Refresh { .. } | HistoryRequest::Latest
                ) || matches!(view.pins, Some(Pins::Loading))
                {
                    self.refresh_pins(client);
                }
                self.prune_previews();
            }
            Err(error) => {
                let jump = matches!(request, HistoryRequest::Around(_));
                if jump && matches!(error, Failure::Http(reqwest::StatusCode::NOT_FOUND)) {
                    view.history = History::Ready;
                    view.jump = None;
                    view.jump_failed = true;
                    self.buffered.clear();
                    if matches!(view.pins, Some(Pins::Loading)) {
                        self.refresh_pins(client);
                    }
                    return;
                }
                if matches!(error, Failure::Http(reqwest::StatusCode::NOT_FOUND))
                    && matches!(request, HistoryRequest::Older(_) | HistoryRequest::Newer(_))
                {
                    self.history = Some(HistoryRequest::Refresh {
                        load_newer: view.paging_forward,
                    });
                }
                if matches!(error, Failure::Http(reqwest::StatusCode::FORBIDDEN)) {
                    view.messages = Arc::default();
                    view.pins = None;
                    view.preview_images = PreviewImages::default();
                    self.gifs.close();
                    self.message_preview = JoinSet::new();
                    self.pin_tasks = JoinSet::new();
                    self.preview_tasks = JoinSet::new();
                    view.has_more_before = false;
                    view.has_more_after = false;
                    if view.direct_message_peer.is_some() {
                        self.outgoing.remove(&view.channel_id);
                        if let Some(draft) = self.drafts.remove(&view.channel_id) {
                            for upload in draft.uploads {
                                self.attachments.cancel(client, upload);
                            }
                        }
                        self.typers.clear();
                    }
                    view.jump_failed = jump;
                    if view.direct_message_peer.is_some() || view.jump_failed {
                        view.history = History::Unavailable;
                        self.buffered.clear();
                        return;
                    }
                }
                view.history = History::Failed;
                self.buffered.clear();
            }
        }
    }

    pub fn should_read(&self, channel_id: &str) -> bool {
        self.selected.as_ref().is_some_and(|view| {
            view.channel_id == channel_id
                && view.latest_ready()
                && !view.hold_read
                && view.pending_unread.is_none()
        })
    }

    pub fn start_mark_unread(&mut self, channel_id: &str, message_id: &str) -> Option<u64> {
        let view = self.selected.as_mut().filter(|view| {
            view.channel_id == channel_id
                && !view.unavailable()
                && view.pending_unread.is_none()
                && view.messages.iter().any(|message| message.id == message_id)
        })?;
        self.unread_request += 1;
        view.pending_unread = Some(self.unread_request);
        view.pending_unread
    }

    pub fn finish_mark_unread(
        &mut self,
        channel_id: &str,
        message_id: String,
        request: u64,
        success: bool,
    ) {
        if let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.channel_id == channel_id && view.pending_unread == Some(request))
        {
            view.pending_unread = None;
            if success {
                view.hold_read = true;
                view.first_unread_id = Some(message_id);
            }
        }
    }

    pub fn clear_unread_marker(&mut self, channel_id: &str) {
        if let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.channel_id == channel_id)
        {
            view.hold_read = false;
            view.first_unread_id = None;
        }
    }

    pub fn update_draft(&mut self, channel_id: String, body: String) {
        let Some(view) = self
            .selected
            .as_ref()
            .filter(|view| view.channel_id == channel_id && !view.unavailable())
        else {
            return;
        };
        if !body.trim().is_empty()
            && view.can_send()
            && self
                .last_typing
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(3))
            && self
                .socket
                .as_ref()
                .is_some_and(|socket| socket.typing(&channel_id))
        {
            self.last_typing = Some(Instant::now());
        }
        self.drafts.entry(channel_id).or_default().body = body;
    }

    pub fn set_reply(&mut self, channel_id: &str, message_id: Option<&str>) {
        let Some(view) = self
            .selected
            .as_ref()
            .filter(|view| view.channel_id == channel_id && view.can_send())
        else {
            return;
        };
        let reply = if let Some(id) = message_id {
            let Some(message) = view.messages.iter().find(|message| message.id == id) else {
                return;
            };
            Some(ReplyRef {
                id: id.to_owned(),
                user_id: message.user_id.clone(),
                preview: message
                    .body
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(120)
                    .collect(),
                deleted: false,
            })
        } else {
            None
        };
        self.drafts.entry(channel_id.to_owned()).or_default().reply = reply;
    }

    pub fn receive_typing(&mut self, channel_id: &str, user_id: String) {
        if self
            .selected
            .as_ref()
            .is_some_and(|view| view.channel_id == channel_id && !view.unavailable())
        {
            self.typers
                .insert(user_id, Instant::now() + Duration::from_secs(5));
        }
    }

    pub fn send(&mut self, channel_id: String, body: String) {
        if self
            .selected
            .as_ref()
            .is_none_or(|view| view.channel_id != channel_id || !view.can_send())
            || self.outgoing.contains_key(&channel_id)
        {
            return;
        }
        let draft = self.drafts.entry(channel_id.clone()).or_default();
        if !draft.can_send(&body) {
            return;
        }
        draft.body.clone_from(&body);
        let reply = draft.reply.clone();
        let attachments = draft.attachment_refs();
        let gif_url = draft.gif.as_ref().map(|gif| gif.item.url.clone());
        self.gifs.close();
        self.send_draft(channel_id, body, gif_url, reply, attachments);
    }

    pub fn send_poll(
        &mut self,
        channel_id: String,
        question: &str,
        options: &[String],
    ) -> Result<(), &'static str> {
        let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.channel_id == channel_id && !view.unavailable())
        else {
            return Err("This conversation is unavailable.");
        };
        let (question, options) = validate_poll(question, options.iter().map(String::as_str))?;
        let nonce = uuid::Uuid::new_v4().to_string();
        if !self.socket.as_ref().is_some_and(|socket| {
            view.can_send() && socket.send_poll(&channel_id, question, &nonce, &options)
        }) {
            return Err("Could not reach SPK. Try again.");
        }
        self.poll_send_error = None;
        self.pending_polls.insert(
            nonce,
            (channel_id, Instant::now() + Duration::from_secs(12)),
        );
        Ok(())
    }

    fn send_draft(
        &mut self,
        channel_id: String,
        body: String,
        gif_url: Option<String>,
        reply: Option<ReplyRef>,
        attachments: Vec<AttachmentRef>,
    ) {
        let wire_body = match (body.trim(), gif_url.as_deref()) {
            (_, None) => std::borrow::Cow::Borrowed(body.as_str()),
            ("", Some(url)) => std::borrow::Cow::Borrowed(url),
            (text, Some(url)) => std::borrow::Cow::Owned(format!("{text}\n{url}")),
        };
        let nonce = uuid::Uuid::new_v4().to_string();
        let status = if self.socket.as_ref().is_some_and(|socket| {
            socket.send_message(
                &channel_id,
                &wire_body,
                &nonce,
                reply.as_ref().map(|reply| reply.id.as_str()),
                &attachments,
            )
        }) {
            Delivery::Sending(Instant::now() + Duration::from_secs(12))
        } else {
            Delivery::Failed("Could not reach SPK. Try again.".into())
        };
        self.outgoing.insert(
            channel_id,
            Outgoing {
                body,
                status,
                nonce,
                gif_url,
                reply,
                attachments,
            },
        );
    }

    pub fn retry_send(&mut self) {
        if let Some(view) = &self.selected
            && view.can_send()
            && self
                .outgoing
                .get(&view.channel_id)
                .is_some_and(|outgoing| matches!(outgoing.status, Delivery::Failed(_)))
        {
            let channel = view.channel_id.clone();
            let outgoing = self.outgoing.remove(&channel).unwrap();
            self.send_draft(
                channel,
                outgoing.body,
                outgoing.gif_url,
                outgoing.reply,
                outgoing.attachments,
            );
        }
    }

    pub fn dismiss_send(&mut self) {
        if let Some(view) = &self.selected
            && self
                .outgoing
                .get(&view.channel_id)
                .is_some_and(|outgoing| !matches!(outgoing.status, Delivery::Sending(_)))
        {
            self.outgoing.remove(&view.channel_id);
        }
    }

    pub fn mark_send_delivered(&mut self, channel_id: &str) {
        if self
            .selected
            .as_ref()
            .is_some_and(|view| view.channel_id == channel_id)
            && self
                .outgoing
                .get(channel_id)
                .is_some_and(|outgoing| matches!(outgoing.status, Delivery::Unconfirmed))
        {
            self.clear_delivered_send(channel_id);
        }
    }

    fn clear_delivered_send(&mut self, channel_id: &str) {
        let outgoing = self.outgoing.remove(channel_id).unwrap();
        if let Some(draft) = self.drafts.get_mut(channel_id) {
            draft.uploads.clear();
            if draft.gif.as_ref().map(|gif| &gif.item.url) == outgoing.gif_url.as_ref() {
                draft.gif = None;
            }
            if draft.body == outgoing.body && draft.reply == outgoing.reply {
                draft.body.clear();
                draft.reply = None;
                draft.upload_notice = None;
            }
        }
    }

    pub fn message(&mut self, message: ChatMessage, nonce: Option<&str>, own_id: Option<&str>) {
        if own_id == Some(message.user_id.as_str())
            && let Some(nonce) = nonce
            && self
                .pending_polls
                .get(nonce)
                .is_some_and(|(channel, _)| *channel == message.channel_id)
        {
            self.pending_polls.remove(nonce);
        }
        if own_id == Some(message.user_id.as_str())
            && self
                .outgoing
                .get(&message.channel_id)
                .is_some_and(|pending| {
                    Some(pending.nonce.as_str()) == nonce || pending.matches_attachment(&message)
                })
        {
            self.clear_delivered_send(&message.channel_id);
        }
        if let Some(view) = &mut self.selected
            && view.channel_id == message.channel_id
            && !view.unavailable()
        {
            if matches!(
                self.history,
                Some(
                    HistoryRequest::Refresh { .. }
                        | HistoryRequest::Around(_)
                        | HistoryRequest::Latest
                        | HistoryRequest::Newer(_)
                )
            ) && view.history != History::Ready
            {
                if view.loading() {
                    self.buffered.push(message.clone());
                }
                if view.direct_message_peer.is_some()
                    && matches!(
                        self.history,
                        Some(HistoryRequest::Refresh { .. } | HistoryRequest::Latest)
                    )
                {
                    return;
                }
            }
            if !view.has_more_after {
                view.merge([message]);
            }
        }
    }

    pub fn delete_message(&mut self, channel_id: &str, id: &str) {
        if let Some(draft) = self.drafts.get_mut(channel_id)
            && draft.reply.as_ref().is_some_and(|reply| reply.id == id)
        {
            draft.reply = None;
        }
        if let Some(outgoing) = self.outgoing.get_mut(channel_id)
            && outgoing.reply.as_ref().is_some_and(|reply| reply.id == id)
        {
            outgoing.reply = None;
        }
        if let Some(view) = &mut self.selected
            && view.channel_id == channel_id
            && view.loading()
        {
            self.deleted_during_load.insert(id.to_owned());
            remove_message(&mut self.buffered, id);
        }
    }

    pub fn error(&mut self, nonce: &str, error: String) {
        if let Some(outgoing) = self
            .outgoing
            .values_mut()
            .find(|outgoing| outgoing.nonce == nonce)
        {
            outgoing.status = Delivery::Failed(error);
        } else if self.pending_polls.remove(nonce).is_some() {
            self.poll_send_error = Some(error);
        }
    }

    pub fn next_timeout(&self) -> Option<Instant> {
        self.outgoing
            .values()
            .filter_map(|outgoing| match outgoing.status {
                Delivery::Sending(deadline) => Some(deadline),
                _ => None,
            })
            .chain(self.pending_polls.values().map(|(_, deadline)| *deadline))
            .chain(self.typers.values().copied())
            .min()
    }

    pub fn expire(&mut self) {
        let now = Instant::now();
        self.typers.retain(|_, until| *until > now);
        self.pending_polls.retain(|_, (_, until)| *until > now);
        for outgoing in self.outgoing.values_mut() {
            if matches!(outgoing.status, Delivery::Sending(deadline) if deadline <= now) {
                outgoing.status = Delivery::Unconfirmed;
            }
        }
    }
}

pub fn validate_poll<'a>(
    question: &'a str,
    options: impl IntoIterator<Item = &'a str>,
) -> Result<(&'a str, Vec<&'a str>), &'static str> {
    fn trim(text: &str) -> &str {
        // JavaScript trim includes BOM and excludes NEL.
        text.trim_matches(|ch: char| ch == '\u{feff}' || ch != '\u{85}' && ch.is_whitespace())
    }

    if question.encode_utf16().count() > 300 {
        return Err("The question can be up to 300 characters.");
    }
    let question = trim(question);
    if question.is_empty() {
        return Err("Enter a question.");
    }
    let mut seen = HashSet::new();
    let mut labels = Vec::new();
    for option in options {
        if option.encode_utf16().count() > 100 {
            return Err("An option can be up to 100 characters.");
        }
        let option = trim(option);
        if option.is_empty() {
            return Err("Fill in every option.");
        }
        if !seen.insert(option.to_lowercase()) {
            return Err("Poll options must be different.");
        }
        labels.push(option);
    }
    if !(2..=10).contains(&labels.len()) {
        return Err("Enter between 2 and 10 options.");
    }
    Ok((question, labels))
}

fn remove_message(messages: &mut Vec<ChatMessage>, id: &str) {
    messages.retain(|message| message.id != id);
    for message in messages {
        if let Some(reply) = &mut message.reply_to
            && reply.id == id
        {
            reply.deleted = true;
            reply.preview.clear();
        }
    }
}

async fn refresh_history(
    client: &SpkClient,
    channel: &str,
    oldest: Option<&ChatMessage>,
) -> Result<MessagePage, Failure> {
    let mut page = client.messages(channel, None).await?;
    // Refetch the loaded range so a reconnect covers gaps beyond the newest page.
    let Some(oldest) = oldest else {
        return Ok(page);
    };
    let first_unread_id = page.first_unread_id.take();
    let mut pages = Vec::new();
    while page.has_more_before {
        let first = page.messages.first().ok_or(Failure::InvalidResponse)?;
        if position(first) <= position(oldest) {
            break;
        }
        let older = client.messages(channel, Some(&first.id)).await?;
        if older
            .messages
            .last()
            .is_some_and(|message| position(message) >= position(first))
        {
            return Err(Failure::InvalidResponse);
        }
        pages.push(page.messages);
        page = older;
    }
    page.messages.extend(pages.into_iter().rev().flatten());
    page.first_unread_id = first_unread_id;
    Ok(page)
}

async fn refresh_window(
    client: &SpkClient,
    channel: &str,
    messages: &[ChatMessage],
    load_newer: bool,
) -> Result<MessagePage, Failure> {
    let first = position(&messages[0]);
    let last = position(messages.last().expect("loaded history window"));
    for anchor in messages {
        let mut page = match client.messages_around(channel, &anchor.id).await {
            Err(Failure::Http(reqwest::StatusCode::NOT_FOUND)) => continue,
            result => result?,
        };
        while page.has_more_after {
            let cursor = page.messages.last().ok_or(Failure::InvalidResponse)?;
            if position(cursor) >= last {
                break;
            }
            append_newer(client, channel, &mut page).await?;
        }
        let start = page
            .messages
            .partition_point(|message| position(message) < first);
        let end = page
            .messages
            .partition_point(|message| position(message) <= last);
        page.has_more_before |= start > 0;
        page.has_more_after |= end < page.messages.len();
        page.messages.truncate(end);
        page.messages.drain(..start);
        if load_newer && page.has_more_after {
            append_newer(client, channel, &mut page).await?;
        }
        return Ok(page);
    }
    refresh_history(client, channel, messages.first()).await
}

async fn append_newer(
    client: &SpkClient,
    channel: &str,
    page: &mut MessagePage,
) -> Result<(), Failure> {
    let cursor = page.messages.last().ok_or(Failure::InvalidResponse)?;
    let next = client.messages_after(channel, &cursor.id).await?;
    if next
        .messages
        .first()
        .is_some_and(|message| position(message) <= position(cursor))
        || (next.has_more_after && next.messages.is_empty())
    {
        return Err(Failure::InvalidResponse);
    }
    page.has_more_after = next.has_more_after;
    page.messages.extend(next.messages);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_requires_content_and_waits_for_uploads() {
        let mut draft = Draft::default();
        assert!(!draft.can_send(" \n "));
        assert!(draft.can_send("message"));
        draft.uploads.push(QueuedAttachment {
            key: uuid::Uuid::nil(),
            filename: "attachment.txt".into(),
            status: UploadStatus::Uploading(50),
            spoiler: false,
            preview: None,
        });
        assert!(!draft.can_send("message"));
        draft.uploads[0].status = UploadStatus::Ready("file".into());
        assert!(draft.can_send(""));
    }
}
