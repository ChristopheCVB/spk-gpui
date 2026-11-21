use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{task::JoinSet, time::Instant};

use crate::api::{ChatMessage, Failure, MessagePage, SpkClient, VoiceSocket};

#[derive(Clone, Copy, PartialEq, Eq)]
enum History {
    Loading,
    Ready,
    Failed,
}

#[derive(Clone)]
pub struct ChatView {
    pub channel_id: String,
    pub messages: Arc<Vec<ChatMessage>>,
    pub has_more_before: bool,
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

#[derive(Clone)]
pub struct Outgoing {
    pub body: String,
    pub status: Delivery,
    nonce: String,
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
}

#[derive(Default)]
pub(super) struct ChatSession {
    selected: Option<ChatView>,
    outgoing: HashMap<String, Outgoing>,
    socket: Option<VoiceSocket>,
    history: Option<HistoryRequest>,
    buffered: Vec<ChatMessage>,
    pub tasks: JoinSet<Result<MessagePage, Failure>>,
}

impl ChatSession {
    pub fn view(&self) -> Option<ChatView> {
        let mut view = self.selected.clone()?;
        view.outgoing = self.outgoing.get(&view.channel_id).cloned();
        Some(view)
    }

    pub fn open(&mut self, client: &SpkClient, channel_id: String) {
        if self
            .selected
            .as_ref()
            .is_some_and(|chat| chat.channel_id == channel_id)
        {
            return;
        }
        self.selected = Some(ChatView {
            channel_id,
            messages: Arc::default(),
            has_more_before: false,
            outgoing: None,
            history: History::Loading,
        });
        self.load(client, HistoryRequest::Refresh);
    }

    pub fn close(&mut self) {
        self.cancel_history();
        self.selected = None;
    }

    pub fn connected(&mut self, client: &SpkClient, socket: VoiceSocket) {
        self.socket = Some(socket);
        self.load(client, HistoryRequest::Refresh);
    }

    pub fn disconnected(&mut self) {
        self.socket = None;
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
    }

    fn load(&mut self, client: &SpkClient, request: HistoryRequest) {
        self.cancel_history();
        let Some(view) = &mut self.selected else {
            return;
        };
        view.history = History::Loading;
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
            }
        });
    }

    pub fn complete(&mut self, result: Result<MessagePage, Failure>) {
        let (Some(view), Some(request)) = (&mut self.selected, &self.history) else {
            return;
        };
        match result {
            Ok(page) => {
                if matches!(request, HistoryRequest::Refresh) {
                    view.messages = Arc::new(page.messages);
                    view.merge(std::mem::take(&mut self.buffered));
                } else {
                    view.merge(page.messages);
                }
                view.has_more_before = page.has_more_before;
                view.history = History::Ready;
            }
            Err(error) => {
                if matches!(error, Failure::Http(reqwest::StatusCode::NOT_FOUND))
                    && matches!(request, HistoryRequest::Older(_))
                {
                    self.history = Some(HistoryRequest::Refresh);
                }
                if matches!(error, Failure::Http(reqwest::StatusCode::FORBIDDEN)) {
                    view.messages = Arc::default();
                    view.has_more_before = false;
                }
                view.history = History::Failed;
                self.buffered.clear();
            }
        }
    }

    pub fn send(&mut self, channel_id: String, body: String) {
        if self
            .selected
            .as_ref()
            .is_none_or(|view| view.channel_id != channel_id)
            || self.outgoing.contains_key(&channel_id)
            || body.trim().is_empty()
        {
            return;
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        let status = if self
            .socket
            .as_ref()
            .is_some_and(|socket| socket.send_message(&channel_id, &body, &nonce))
        {
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
            },
        );
    }

    pub fn retry_send(&mut self) {
        if let Some(view) = &self.selected
            && self
                .outgoing
                .get(&view.channel_id)
                .is_some_and(|outgoing| matches!(outgoing.status, Delivery::Failed(_)))
        {
            let channel = view.channel_id.clone();
            let outgoing = self.outgoing.remove(&channel).unwrap();
            self.send(channel, outgoing.body);
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
            self.outgoing.remove(&message.channel_id);
        }
        if let Some(view) = &mut self.selected
            && view.channel_id == message.channel_id
        {
            if view.loading() && matches!(self.history, Some(HistoryRequest::Refresh)) {
                self.buffered.push(message.clone());
            }
            view.merge([message]);
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
            .min()
    }

    pub fn expire(&mut self) {
        let now = Instant::now();
        for outgoing in self.outgoing.values_mut() {
            if matches!(outgoing.status, Delivery::Sending(deadline) if deadline <= now) {
                outgoing.status = Delivery::Unconfirmed;
            }
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
