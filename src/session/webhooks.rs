use reqwest::StatusCode;
use tokio::sync::oneshot;

use crate::api::{Channel, Failure, WebhookUrl};

pub type WebhookCreateReply = oneshot::Sender<Result<WebhookUrl, &'static str>>;
pub(super) type WebhookCreateCompletion =
    (Result<(Channel, WebhookUrl), Failure>, WebhookCreateReply);

pub(super) fn create_error(error: Failure) -> &'static str {
    match error {
        Failure::Http(StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED) => {
            "This SPK server needs a version that supports Webhooks."
        }
        Failure::Http(StatusCode::FORBIDDEN) => "SPK denied permission to create this webhook.",
        Failure::Http(StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY) => {
            "SPK rejected the webhook settings. Check the name and icon, then try again."
        }
        Failure::Connection => "Could not reach SPK. Check your connection and try again.",
        _ => "Could not create the webhook. Refresh the feed list before trying again.",
    }
}
