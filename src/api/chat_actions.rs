use super::*;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub emoji: String,
    pub user_ids: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPreview {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub image: bool,
}

impl LinkPreview {
    pub fn link(&self) -> Option<Url> {
        let url = Url::parse(&self.url).ok()?;
        (matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none())
        .then_some(url)
    }
}

pub enum MessageAction {
    Edit(String),
    Delete,
    React { emoji: String, on: bool },
    Pin(bool),
    RemovePreview(String),
}

#[derive(Clone)]
pub enum MessageChange {
    Edit(Box<ChatMessage>),
    Delete,
    Reactions(Vec<Reaction>),
    Pin(bool),
    Previews(Vec<LinkPreview>),
    RemovePreview(String),
}

impl SpkClient {
    fn chat_resource(&self, scope: &str, id: &str, resource: Option<&str>) -> Result<Url, Failure> {
        if matches!(id, "" | "." | "..") {
            return Err(Failure::InvalidResponse);
        }
        let mut url = self.base.join("/api/").expect("fixed SPK endpoint path");
        {
            let mut path = url.path_segments_mut().expect("SPK URL has a host");
            path.pop_if_empty().extend([scope, id]);
            path.extend(resource);
        }
        Ok(url)
    }

    pub async fn mutate_message(
        &self,
        channel_id: &str,
        message_id: &str,
        action: MessageAction,
    ) -> Result<MessageChange, Failure> {
        use serde_json::json;

        let (method, route, body) = match &action {
            MessageAction::Edit(body) => (Method::PATCH, None, Some(json!({ "body": body }))),
            MessageAction::Delete => (Method::DELETE, None, None),
            MessageAction::React { emoji, on } => (
                Method::POST,
                Some("reactions"),
                Some(json!({ "emoji": emoji, "on": on })),
            ),
            MessageAction::Pin(on) => (Method::POST, Some("pin"), Some(json!({ "on": on }))),
            MessageAction::RemovePreview(url) => (
                Method::DELETE,
                Some("previews"),
                Some(json!({ "url": url })),
            ),
        };
        let url = self.chat_resource("messages", message_id, route)?;
        let request = self
            .http
            .request(method, url)
            .header(header::ORIGIN, &self.origin);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        let response = Self::send(request).await?;
        match action {
            MessageAction::Edit(_) => {
                let message: Box<ChatMessage> = response
                    .json()
                    .await
                    .map_err(|_| Failure::InvalidResponse)?;
                if message.id != message_id || message.channel_id != channel_id {
                    return Err(Failure::InvalidResponse);
                }
                Ok(MessageChange::Edit(message))
            }
            MessageAction::Delete => Ok(MessageChange::Delete),
            MessageAction::RemovePreview(url) => Ok(MessageChange::RemovePreview(url)),
            MessageAction::React { .. } | MessageAction::Pin(_) => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Updated {
                    channel_id: String,
                    message_id: String,
                    reactions: Option<Vec<Reaction>>,
                    pinned: Option<bool>,
                }

                let state: Updated = response
                    .json()
                    .await
                    .map_err(|_| Failure::InvalidResponse)?;
                if state.channel_id != channel_id || state.message_id != message_id {
                    return Err(Failure::InvalidResponse);
                }
                match action {
                    MessageAction::React { .. } => state.reactions.map(MessageChange::Reactions),
                    _ => state.pinned.map(MessageChange::Pin),
                }
                .ok_or(Failure::InvalidResponse)
            }
        }
    }

    pub async fn pins(&self, channel_id: &str) -> Result<Vec<ChatMessage>, Failure> {
        let url = self.chat_resource("channels", channel_id, Some("pins"))?;
        let messages: Vec<ChatMessage> =
            Self::send(self.http.get(url).header(header::ORIGIN, &self.origin))
                .await?
                .json()
                .await
                .map_err(|_| Failure::InvalidResponse)?;
        if messages
            .iter()
            .any(|message| message.channel_id != channel_id)
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(messages)
    }

    pub async fn preview_image(
        &self,
        message_id: &str,
        preview_url: &str,
    ) -> Result<Vec<u8>, Failure> {
        let mut url = self.chat_resource("messages", message_id, Some("preview-image"))?;
        url.query_pairs_mut().append_pair("url", preview_url);
        self.storage_redirect(
            self.http.get(url).header(header::ORIGIN, &self.origin),
            2 * 1024 * 1024,
        )
        .await
    }
}
