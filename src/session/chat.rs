use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use tokio::{task::JoinSet, time::Instant};

use crate::api::{Channel, ChatMessage, Failure, MessagePage, ReplyRef, SpkClient, VoiceSocket};

#[derive(Clone, Copy, PartialEq, Eq)]
enum History {
    Loading,
    Ready,
    Failed,
    Unavailable,
}

#[derive(Clone)]
pub struct ChatView {
    pub channel_id: String,
    pub direct_message_peer: Option<String>,
    pub messages: Arc<Vec<ChatMessage>>,
    pub has_more_before: bool,
    pub has_more_after: bool,
    pub draft: Draft,
    pub typing: Vec<String>,
    pub jump: Option<(String, u64)>,
    pub jump_failed: bool,
    pub outgoing: Option<Outgoing>,
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

    pub fn can_send(&self) -> bool {
        !self.unavailable()
            && (self.direct_message_peer.is_none() || self.history == History::Ready)
    }

    fn mark_jump(&mut self, id: String) {
        let generation = self
            .jump
            .as_ref()
            .map_or(0, |(_, generation)| generation + 1);
        self.jump = Some((id, generation));
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
    pub reply: Option<ReplyRef>,
}

#[derive(Clone)]
pub struct Outgoing {
    pub body: String,
    pub status: Delivery,
    nonce: String,
    reply: Option<ReplyRef>,
}

#[derive(Clone)]
pub enum Delivery {
    Sending(Instant),
    Failed(String),
    Unconfirmed,
}

#[derive(Clone)]
enum HistoryRequest {
    Refresh,
    Older(String),
    Around(String),
    Latest,
}

#[derive(Default)]
pub(super) struct ChatSession {
    selected: Option<ChatView>,
    outgoing: HashMap<String, Outgoing>,
    drafts: HashMap<String, Draft>,
    typers: HashMap<String, Instant>,
    last_typing: Option<Instant>,
    socket: Option<VoiceSocket>,
    history: Option<HistoryRequest>,
    buffered: Vec<ChatMessage>,
    deleted_during_load: HashSet<String>,
    pub tasks: JoinSet<Result<MessagePage, Failure>>,
}

impl ChatSession {
    pub fn view(&self) -> Option<ChatView> {
        let mut view = self.selected.clone()?;
        // Hide cached DM content until SPK confirms access to the conversation.
        if view.direct_message_peer.is_some()
            && view.history != History::Ready
            && matches!(
                self.history,
                Some(HistoryRequest::Refresh | HistoryRequest::Latest | HistoryRequest::Around(_))
            )
        {
            view.messages = Arc::default();
            view.has_more_before = false;
            view.has_more_after = false;
        } else if !view.unavailable() {
            view.outgoing = self.outgoing.get(&view.channel_id).cloned();
            view.draft = self
                .drafts
                .get(&view.channel_id)
                .cloned()
                .unwrap_or_default();
            view.typing = self.typers.keys().cloned().collect();
            view.typing.sort_unstable();
        }
        Some(view)
    }

    pub fn open(&mut self, client: &SpkClient, channel: &Channel) {
        if self.select(channel) {
            self.load(client, HistoryRequest::Refresh);
        }
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
        self.selected = Some(ChatView {
            channel_id: channel.id.clone(),
            direct_message_peer: channel
                .other_user_id
                .clone()
                .filter(|_| channel.kind == "dm"),
            messages: Arc::default(),
            has_more_before: false,
            has_more_after: false,
            draft: Draft::default(),
            typing: Vec::new(),
            jump: None,
            jump_failed: false,
            outgoing: None,
            history: History::Loading,
        });
        true
    }

    pub fn close(&mut self) {
        self.cancel_history();
        self.selected = None;
        self.typers.clear();
    }

    pub fn connected(&mut self, client: &SpkClient, socket: VoiceSocket) {
        self.socket = Some(socket);
        if matches!(self.history, Some(HistoryRequest::Around(_)))
            && self.selected.as_ref().is_some_and(ChatView::loading)
        {
            return;
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|view| view.has_more_after)
        {
            self.latest(client);
        } else {
            self.load(client, HistoryRequest::Refresh);
        }
    }

    pub fn disconnected(&mut self) {
        self.socket = None;
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
        self.deleted_during_load.clear();
    }

    fn load(&mut self, client: &SpkClient, request: HistoryRequest) {
        self.cancel_history();
        let Some(view) = &mut self.selected else {
            return;
        };
        view.history = History::Loading;
        view.jump_failed = false;
        let channel = view.channel_id.clone();
        let messages = view.messages.clone();
        let client = client.clone();
        self.history = Some(request.clone());
        self.tasks.spawn(async move {
            match request {
                HistoryRequest::Refresh => {
                    refresh_history(&client, &channel, messages.first()).await
                }
                HistoryRequest::Older(before) => client.messages(&channel, Some(&before)).await,
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

    pub fn complete(&mut self, result: Result<MessagePage, Failure>) {
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
                } else {
                    view.messages = Arc::new(page.messages);
                    view.has_more_after = page.has_more_after;
                    let buffered = std::mem::take(&mut self.buffered);
                    if !view.has_more_after {
                        view.merge(buffered);
                    }
                    if let HistoryRequest::Around(id) = request {
                        if view.messages.iter().any(|message| message.id == *id) {
                            view.mark_jump(id.clone());
                        } else {
                            view.jump = None;
                            view.jump_failed = true;
                        }
                    }
                }
                view.has_more_before = page.has_more_before;
                view.history = History::Ready;
            }
            Err(error) => {
                let jump = matches!(request, HistoryRequest::Around(_));
                if jump && matches!(error, Failure::Http(reqwest::StatusCode::NOT_FOUND)) {
                    view.history = History::Ready;
                    view.jump = None;
                    view.jump_failed = true;
                    self.buffered.clear();
                    return;
                }
                if matches!(error, Failure::Http(reqwest::StatusCode::NOT_FOUND))
                    && matches!(request, HistoryRequest::Older(_))
                {
                    self.history = Some(HistoryRequest::Refresh);
                }
                if matches!(error, Failure::Http(reqwest::StatusCode::FORBIDDEN)) {
                    view.messages = Arc::default();
                    view.has_more_before = false;
                    view.has_more_after = false;
                    if view.direct_message_peer.is_some() {
                        self.outgoing.remove(&view.channel_id);
                        self.drafts.remove(&view.channel_id);
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
            || body.trim().is_empty()
        {
            return;
        }
        let draft = self.drafts.entry(channel_id.clone()).or_default();
        draft.body.clone_from(&body);
        let reply = draft.reply.clone();
        self.send_draft(channel_id, body, reply);
    }

    fn send_draft(&mut self, channel_id: String, body: String, reply: Option<ReplyRef>) {
        let nonce = uuid::Uuid::new_v4().to_string();
        let status = if self.socket.as_ref().is_some_and(|socket| {
            socket.send_message(
                &channel_id,
                &body,
                &nonce,
                reply.as_ref().map(|reply| reply.id.as_str()),
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
                reply,
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
            self.send_draft(channel, outgoing.body, outgoing.reply);
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

    pub fn message(&mut self, message: ChatMessage, nonce: Option<&str>, own_id: Option<&str>) {
        if own_id == Some(message.user_id.as_str())
            && self
                .outgoing
                .get(&message.channel_id)
                .is_some_and(|pending| Some(pending.nonce.as_str()) == nonce)
        {
            let outgoing = self.outgoing.remove(&message.channel_id).unwrap();
            if self
                .drafts
                .get(&message.channel_id)
                .is_some_and(|draft| draft.body == outgoing.body && draft.reply == outgoing.reply)
            {
                self.drafts.remove(&message.channel_id);
            }
        }
        if let Some(view) = &mut self.selected
            && view.channel_id == message.channel_id
            && !view.unavailable()
        {
            if matches!(
                self.history,
                Some(HistoryRequest::Refresh | HistoryRequest::Around(_) | HistoryRequest::Latest)
            ) && view.history != History::Ready
            {
                if view.loading() {
                    self.buffered.push(message.clone());
                }
                if view.direct_message_peer.is_some()
                    && matches!(
                        self.history,
                        Some(HistoryRequest::Refresh | HistoryRequest::Latest)
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
            && !view.unavailable()
        {
            if view.loading() {
                self.deleted_during_load.insert(id.to_owned());
                remove_message(&mut self.buffered, id);
            }
            remove_message(Arc::make_mut(&mut view.messages), id);
        }
    }

    pub fn error(&mut self, nonce: &str, error: String) {
        if let Some(outgoing) = self
            .outgoing
            .values_mut()
            .find(|outgoing| outgoing.nonce == nonce)
        {
            outgoing.status = Delivery::Failed(error);
        }
    }

    pub fn next_timeout(&self) -> Option<Instant> {
        self.outgoing
            .values()
            .filter_map(|outgoing| match outgoing.status {
                Delivery::Sending(deadline) => Some(deadline),
                _ => None,
            })
            .chain(self.typers.values().copied())
            .min()
    }

    pub fn expire(&mut self) {
        let now = Instant::now();
        self.typers.retain(|_, until| *until > now);
        for outgoing in self.outgoing.values_mut() {
            if matches!(outgoing.status, Delivery::Sending(deadline) if deadline <= now) {
                outgoing.status = Delivery::Unconfirmed;
            }
        }
    }
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
    Ok(page)
}
