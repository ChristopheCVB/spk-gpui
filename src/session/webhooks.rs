use reqwest::StatusCode;
use tokio::{sync::oneshot, task::JoinSet};

use super::{
    ChatNavigation, ChatSession, SearchSession, Sidebar, UnreadSession,
    presence::{Refresh, Resource},
};
use crate::api::{Channel, Failure, SpkClient, WebhookSettings, WebhookUrl};

pub enum WebhookAction {
    Create(WebhookSettings),
    Edit {
        id: String,
        settings: WebhookSettings,
    },
    Rotate(String),
    Delete(String),
}

pub type WebhookReply = oneshot::Sender<Result<Option<WebhookUrl>, &'static str>>;
pub(super) type WebhookCompletion = (Result<WebhookChange, Failure>, WebhookReply, bool);

pub(super) enum WebhookChange {
    Created(Channel, WebhookUrl),
    Edited(Channel),
    Rotated(String, WebhookUrl),
    Deleted(String),
}

impl Sidebar {
    fn owns_webhook(&self, feed: &Channel) -> bool {
        self.profile.id.is_some() && self.profile.id == feed.owner_id
    }

    pub(super) fn can_view_webhook(&self, feed: &Channel) -> bool {
        !feed.private || self.owns_webhook(feed)
    }

    pub fn can_manage_webhook(&self, feed: &Channel) -> bool {
        self.can_view_webhook(feed) && (self.is_admin() || self.owns_webhook(feed))
    }
}

impl WebhookAction {
    pub(super) fn validated(self, sidebar: &Sidebar) -> Result<Self, &'static str> {
        let id = match &self {
            Self::Create(_) => None,
            Self::Edit { id, .. } | Self::Rotate(id) | Self::Delete(id) => Some(id),
        };
        if let Some(id) = id {
            let feed = sidebar
                .webhook_feed(id)
                .ok_or("Webhook feed unavailable.")?;
            if !sidebar.can_manage_webhook(feed) {
                return Err("You cannot manage this webhook.");
            }
        }
        match self {
            Self::Create(settings) => settings.validated().map(Self::Create),
            Self::Edit { id, settings } => Ok(Self::Edit {
                id,
                settings: settings.validated()?,
            }),
            Self::Rotate(_) | Self::Delete(_) => Ok(self),
        }
    }

    pub(super) async fn send(self, client: &SpkClient, reply: WebhookReply) -> WebhookCompletion {
        let creating = matches!(self, Self::Create(_));
        let result = match self {
            Self::Create(settings) => client
                .create_webhook(settings)
                .await
                .map(|(feed, url)| WebhookChange::Created(feed, url)),
            Self::Edit { id, settings } => client
                .edit_webhook(&id, settings)
                .await
                .map(WebhookChange::Edited),
            Self::Rotate(id) => client
                .rotate_webhook(&id)
                .await
                .map(|url| WebhookChange::Rotated(id, url)),
            Self::Delete(id) => client
                .delete_webhook(&id)
                .await
                .map(|()| WebhookChange::Deleted(id)),
        };
        (result, reply, creating)
    }
}

pub(super) struct MutationContext<'a> {
    pub sidebar: &'a mut Sidebar,
    pub chat: &'a mut ChatSession,
    pub navigation: &'a mut Option<ChatNavigation>,
    pub direct_message_opens: &'a mut JoinSet<Result<Channel, Failure>>,
    pub refresh: &'a mut Refresh,
    pub unread: &'a mut UnreadSession,
    pub search: &'a mut SearchSession,
}

impl MutationContext<'_> {
    pub fn complete(
        self,
        client: &SpkClient,
        result: Result<WebhookChange, Failure>,
        creating: bool,
    ) -> Result<Option<WebhookUrl>, &'static str> {
        let change = match result {
            Ok(change) => change,
            Err(error) => {
                if !creating
                    && matches!(
                        error,
                        Failure::Http(StatusCode::NOT_FOUND | StatusCode::FORBIDDEN)
                    )
                {
                    self.refresh.fetch(client, Resource::Channels);
                }
                return Err(mutation_error(error, creating));
            }
        };
        let removed = match change {
            WebhookChange::Created(channel, url) => {
                self.refresh.webhook_changed(client);
                *self.direct_message_opens = JoinSet::new();
                self.sidebar.chat_notice = None;
                ChatNavigation::open_chat(self.navigation, self.chat, client, &channel);
                let channels = self.sidebar.channels.get_or_insert_default();
                if let Some(listed) = channels.iter_mut().find(|listed| listed.id == channel.id) {
                    *listed = channel;
                } else {
                    channels.push(channel);
                    self.unread.refresh(client);
                }
                return Ok(Some(url));
            }
            WebhookChange::Edited(channel) => {
                self.refresh.webhook_changed(client);
                if self.sidebar.can_view_webhook(&channel) {
                    if let Some(listed) = self
                        .sidebar
                        .channels
                        .iter_mut()
                        .flatten()
                        .find(|listed| listed.id == channel.id)
                    {
                        *listed = channel;
                    }
                    return Ok(None);
                }
                channel.id
            }
            WebhookChange::Rotated(id, url) => {
                self.refresh.fetch(client, Resource::Channels);
                return self
                    .sidebar
                    .webhook_feed(&id)
                    .filter(|feed| self.sidebar.can_manage_webhook(feed))
                    .map(|_| Some(url))
                    .ok_or("Webhook feed unavailable.");
            }
            WebhookChange::Deleted(id) => {
                self.refresh.webhook_changed(client);
                id
            }
        };
        if let Some(channels) = &mut self.sidebar.channels {
            let count = channels.len();
            channels.retain(|listed| listed.id != removed);
            if channels.len() != count {
                self.unread.refresh(client);
            }
        }
        reconcile_access(self.sidebar, self.chat, self.navigation);
        self.search.reconcile_channels(self.sidebar);
        Ok(None)
    }
}

fn mutation_error(error: Failure, creating: bool) -> &'static str {
    match error {
        Failure::Http(StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED) if creating => {
            "This SPK server needs a version that supports Webhooks."
        }
        Failure::Http(StatusCode::NOT_FOUND) => "Webhook feed unavailable.",
        Failure::Http(StatusCode::FORBIDDEN) if creating => {
            "SPK denied permission to create this webhook."
        }
        Failure::Http(StatusCode::FORBIDDEN) => "SPK denied permission to manage this webhook.",
        Failure::Http(StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY) => {
            "SPK rejected the webhook settings. Check the name and icon, then try again."
        }
        Failure::Connection => "Could not reach SPK. Check your connection and try again.",
        _ if creating => "Could not create the webhook. Refresh the feed list before trying again.",
        _ => "Could not update the webhook. Refresh the feed list before trying again.",
    }
}

pub(super) fn reconcile_access(
    sidebar: &mut Sidebar,
    chat: &mut ChatSession,
    navigation: &mut Option<ChatNavigation>,
) {
    if let Some(id) = chat.selected_webhook()
        && sidebar.webhook_feed(id).is_none()
    {
        chat.close();
        if !matches!(navigation, Some(ChatNavigation::AwaitingChannel { .. })) {
            *navigation = None;
            sidebar.chat_notice = Some("Webhook feed unavailable.");
        }
    }
}
