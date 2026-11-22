use super::*;
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Text,
    Voice,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Voice => "voice",
        }
    }

    pub fn from_channel(channel: &Channel) -> Option<Self> {
        match channel.kind.as_str() {
            "text" => Some(Self::Text),
            "voice" => Some(Self::Voice),
            _ => None,
        }
    }
}

pub enum ChannelMutation {
    Create(ChannelKind, String),
    Rename(String, String),
    Reorder(ChannelKind, Vec<String>),
    Delete(String),
}

impl SpkClient {
    pub async fn mutate_channel(&self, action: ChannelMutation) -> Result<(), Failure> {
        let mut url = self
            .base
            .join("/api/admin/channels")
            .expect("fixed SPK endpoint path");
        let (method, segment, body) = match action {
            ChannelMutation::Create(kind, name) => (
                Method::POST,
                None,
                Some(json!({"type": kind.as_str(), "name": name})),
            ),
            ChannelMutation::Rename(id, name) => {
                (Method::PATCH, Some(id), Some(json!({"name": name})))
            }
            ChannelMutation::Reorder(kind, ids) => (
                Method::POST,
                Some("order".into()),
                Some(json!({"type": kind.as_str(), "ids": ids})),
            ),
            ChannelMutation::Delete(id) => (Method::DELETE, Some(id), None),
        };
        if let Some(segment) = segment {
            if matches!(segment.as_str(), "" | "." | "..") {
                return Err(Failure::InvalidResponse);
            }
            url.path_segments_mut()
                .expect("SPK URL has a host")
                .push(&segment);
        }
        let mut request = self
            .http
            .request(method, url)
            .header(header::ORIGIN, &self.origin);
        if let Some(body) = body {
            request = request.json(&body);
        }
        Self::send(request).await?;
        Ok(())
    }
}
