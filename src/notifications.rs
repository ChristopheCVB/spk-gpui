use std::{collections::HashSet, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    api::{ChatMessage, MessageTarget},
    session::Sidebar,
};

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub dms: bool,
    pub pings: bool,
    pub system_previews: bool,
    pub muted: HashSet<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            dms: true,
            pings: true,
            system_previews: true,
            muted: HashSet::new(),
        }
    }
}

impl Preferences {
    fn path() -> Option<PathBuf> {
        Some(
            dirs::config_dir()?
                .join("spk-gpui")
                .join("notifications.json"),
        )
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path().ok_or_else(|| io::Error::other("Missing config directory"))?;
        let directory = path.parent().expect("notifications file has a parent");
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.persist(path)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Kind {
    DirectMessage,
    Ping,
}

#[derive(Clone)]
pub struct Alert {
    pub target: MessageTarget,
    pub title: String,
    pub body: String,
    pub has_attachment: bool,
    kind: Kind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    InApp,
    System,
}

impl Alert {
    fn for_message(
        sidebar: &Sidebar,
        channel: &crate::api::Channel,
        message: &ChatMessage,
    ) -> Option<Self> {
        let me = sidebar.profile.id.as_deref()?;
        let author = sidebar.message_author(&message.user_id);
        let (kind, title) = match channel.kind.as_str() {
            "dm" if channel.other_user_id.is_some() => (Kind::DirectMessage, author.to_owned()),
            "text" if message.pings(me) => (
                Kind::Ping,
                format!(
                    "{author} in #{}",
                    channel.name.as_deref().unwrap_or("Text Channel")
                ),
            ),
            _ => return None,
        };
        Some(Self {
            target: MessageTarget::new(&message.channel_id, &message.id)?,
            title,
            body: message.body.clone(),
            has_attachment: !message.attachments.is_empty(),
            kind,
        })
    }

    pub fn delivery(
        &self,
        preferences: &Preferences,
        busy: bool,
        focused: bool,
        visible_channel: Option<&str>,
    ) -> Option<Delivery> {
        let channel = self.target.channel_id();
        if busy
            || preferences.muted.contains(channel)
            || (focused && visible_channel == Some(channel))
            || match self.kind {
                Kind::DirectMessage => !preferences.dms,
                Kind::Ping => !preferences.pings,
            }
        {
            return None;
        }
        Some(if focused {
            Delivery::InApp
        } else {
            Delivery::System
        })
    }
}

#[derive(Default)]
pub struct LiveAlerts {
    seen: HashSet<String>,
    unlisted: Vec<ChatMessage>,
    pub ready: Vec<Alert>,
}

impl LiveAlerts {
    pub fn receive(&mut self, sidebar: &Sidebar, message: &ChatMessage) {
        if sidebar
            .profile
            .id
            .as_deref()
            .is_none_or(|me| me == message.user_id)
            || !self.seen.insert(message.id.clone())
        {
            return;
        }
        if let Some(channel) = sidebar
            .channels
            .iter()
            .flatten()
            .find(|channel| channel.id == message.channel_id)
        {
            self.ready
                .extend(Alert::for_message(sidebar, channel, message));
        } else {
            self.unlisted.push(message.clone());
        }
    }

    pub fn resolve_unlisted(&mut self, sidebar: &Sidebar) {
        for message in self.unlisted.drain(..) {
            if let Some(channel) = sidebar
                .channels
                .iter()
                .flatten()
                .find(|channel| channel.id == message.channel_id)
            {
                self.ready
                    .extend(Alert::for_message(sidebar, channel, &message));
            }
        }
    }

    pub fn disconnected(&mut self) {
        self.unlisted.clear();
    }
}

const TAG_PREFIX: &str = "spk-alert:";

pub fn activation_tag(base: &Url, target: &MessageTarget) -> String {
    format!("{TAG_PREFIX}{}", target.permalink(base))
}

pub fn activation_target(base: &Url, tag: &str) -> Option<MessageTarget> {
    let url = Url::parse(tag.strip_prefix(TAG_PREFIX)?).ok()?;
    MessageTarget::parse(base, &url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(kind: Kind) -> Alert {
        Alert {
            target: MessageTarget::new("general", "42").unwrap(),
            title: "Alice".to_owned(),
            body: "Hello".into(),
            has_attachment: false,
            kind,
        }
    }

    #[test]
    fn message_alerts_follow_focus_busy_and_separate_preferences() {
        let mut preferences = Preferences::default();
        let dm = alert(Kind::DirectMessage);
        let ping = alert(Kind::Ping);
        assert_eq!(
            dm.delivery(&preferences, false, true, Some("general")),
            None
        );
        assert_eq!(
            dm.delivery(&preferences, false, true, None),
            Some(Delivery::InApp)
        );
        assert_eq!(
            dm.delivery(&preferences, false, false, Some("general")),
            Some(Delivery::System)
        );
        assert_eq!(dm.delivery(&preferences, true, false, None), None);

        preferences.dms = false;
        assert_eq!(dm.delivery(&preferences, false, true, None), None);
        assert_eq!(
            ping.delivery(&preferences, false, true, None),
            Some(Delivery::InApp)
        );
        preferences.dms = true;
        preferences.pings = false;
        assert_eq!(
            dm.delivery(&preferences, false, true, None),
            Some(Delivery::InApp)
        );
        assert_eq!(ping.delivery(&preferences, false, true, None), None);
        preferences.muted.insert("general".to_owned());
        assert_eq!(dm.delivery(&preferences, false, false, None), None);
    }

    #[test]
    fn native_activation_accepts_only_same_instance_message_targets() {
        let base = Url::parse("https://example.test").unwrap();
        let target = alert(Kind::Ping).target;
        let tag = activation_tag(&base, &target);
        assert_eq!(activation_target(&base, &tag), Some(target));
        assert_eq!(
            activation_target(&Url::parse("https://other.test").unwrap(), &tag),
            None
        );
        assert_eq!(
            activation_target(&base, "spk-alert:https://example.test/c/general/m/%2F"),
            None
        );
        assert_eq!(
            activation_target(&base, "https://example.test/c/general/m/42"),
            None
        );
    }
}
