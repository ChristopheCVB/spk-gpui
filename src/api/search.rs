use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(try_from = "MessageIds")]
pub struct MessageTarget {
    channel_id: String,
    message_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageIds {
    channel_id: String,
    message_id: String,
}

impl TryFrom<MessageIds> for MessageTarget {
    type Error = &'static str;

    fn try_from(ids: MessageIds) -> Result<Self, Self::Error> {
        Self::new(&ids.channel_id, &ids.message_id).ok_or("Invalid message target")
    }
}

impl MessageTarget {
    pub fn new(channel_id: &str, message_id: &str) -> Option<Self> {
        [channel_id, message_id]
            .iter()
            .all(|id| valid_id(id))
            .then(|| Self {
                channel_id: channel_id.into(),
                message_id: message_id.into(),
            })
    }

    pub fn parse(base: &Url, link: &Url) -> Option<Self> {
        let mut segments = conversation_path(base, link)?.split('/');
        let channel = segments.next()?;
        if segments.next()? != "m" {
            return None;
        }
        let message = segments.next()?;
        if segments.next().is_some() {
            return None;
        }
        Self::new(channel, message)
    }

    pub fn permalink(&self, base: &Url) -> Url {
        base.join(&format!("/c/{}/m/{}", self.channel_id, self.message_id))
            .expect("validated SPK message target")
    }

    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    pub fn message_id(&self) -> &str {
        &self.message_id
    }
}

pub fn conversation_link<'a>(base: &Url, link: &'a Url) -> Option<&'a str> {
    conversation_path(base, link).filter(|id| valid_id(id))
}

fn conversation_path<'a>(base: &Url, link: &'a Url) -> Option<&'a str> {
    if link.origin() != base.origin() || !link.username().is_empty() || link.password().is_some() {
        return None;
    }
    let path = link.path();
    path.strip_suffix('/').unwrap_or(path).strip_prefix("/c/")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchScope {
    Conversation(String),
    TextChannels,
    DirectMessages,
    Everything,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    #[serde(flatten)]
    pub target: MessageTarget,
    pub user_id: String,
    pub snippet: String,
    pub text: String,
    pub attachments: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub results: Vec<SearchHit>,
    pub next_cursor: Option<String>,
}

impl SpkClient {
    pub async fn search_messages(
        &self,
        query: &str,
        scope: &SearchScope,
        cursor: Option<&str>,
    ) -> Result<SearchPage, Failure> {
        let (name, channel) = match scope {
            SearchScope::Conversation(channel) => ("channel", Some(channel)),
            SearchScope::TextChannels => ("channels", None),
            SearchScope::DirectMessages => ("dms", None),
            SearchScope::Everything => ("everything", None),
        };
        let query = {
            let mut params = url::form_urlencoded::Serializer::new(String::new());
            params.append_pair("q", query).append_pair("scope", name);
            if let Some(channel) = channel {
                params.append_pair("channelId", channel);
            }
            if let Some(cursor) = cursor {
                params.append_pair("cursor", cursor);
            }
            params.finish()
        };
        self.get(&format!("/api/search?{query}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_permalinks_round_trip_only_for_the_same_instance() {
        let base = Url::parse("https://spk.example.test").unwrap();
        let canonical = Url::parse("https://spk.example.test/c/channel-1/m/message_123").unwrap();
        let target = MessageTarget::parse(&base, &canonical).unwrap();
        assert_eq!(target.channel_id(), "channel-1");
        assert_eq!(target.message_id(), "message_123");
        assert_eq!(target.permalink(&base), canonical);
        assert_eq!(conversation_link(&base, &canonical), None);
        let conversation = base.join("/c/channel-1/").unwrap();
        assert_eq!(conversation_link(&base, &conversation), Some("channel-1"));
        assert_eq!(MessageTarget::parse(&base, &conversation), None);
        let escaped = base.join("/c/channel%2Fother").unwrap();
        assert_eq!(conversation_link(&base, &escaped), None);

        for invalid in [
            "http://spk.example.test/c/channel/m/message",
            "https://spk.example.test:444/c/channel/m/message",
            "https://spk.example.test.other.example/c/channel/m/message",
            "https://other.example/c/channel/m/message",
            "https://credential@spk.example.test/c/channel/m/message",
            "https://spk.example.test/c/channel/m/",
            "https://spk.example.test/c/channel/m/message/extra",
            "https://spk.example.test/c/channel%2Fother/m/message",
        ] {
            assert_eq!(
                MessageTarget::parse(&base, &Url::parse(invalid).unwrap()),
                None
            );
        }
    }
}
