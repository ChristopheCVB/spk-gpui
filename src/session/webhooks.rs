use reqwest::StatusCode;
use tokio::sync::oneshot;

use super::{ChatNavigation, ChatSession, SearchSession, Sidebar};
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

pub(super) fn mutation_error(error: Failure, creating: bool) -> &'static str {
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
    search: &mut SearchSession,
    previous: Vec<String>,
) {
    search.reconcile_webhooks(sidebar, previous);
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
