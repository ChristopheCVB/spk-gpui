#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

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
    pub fn path(base: &Url, user_id: &str) -> Option<PathBuf> {
        Some(scoped_path(&dirs::config_dir()?, base, user_id))
    }

    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().expect("notifications file has a parent");
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.persist(path)?;
        Ok(())
    }
}

fn scoped_path(root: &Path, base: &Url, user_id: &str) -> PathBuf {
    let origin = base.origin().ascii_serialization();
    let mut path = root.join("spk-gpui").join("notifications");
    // Encode each component for case-insensitive filesystems and keep names within their limits.
    for (scope, value) in [("instance", origin.as_str()), ("account", user_id)] {
        path.push(scope);
        for chunk in value.as_bytes().chunks(100) {
            path.push(
                chunk
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            );
        }
    }
    path.join("preferences.json")
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
    pub fn remember_history(&mut self, messages: &[ChatMessage]) {
        self.seen
            .extend(messages.iter().map(|message| message.id.clone()));
    }

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

pub struct NativeActivation {
    pub response: Option<gpui_kit::SystemNotificationResponse>,
    #[cfg(target_os = "linux")]
    pub token: Option<String>,
}

impl From<gpui_kit::SystemNotificationResponse> for NativeActivation {
    fn from(response: gpui_kit::SystemNotificationResponse) -> Self {
        Self {
            response: Some(response),
            #[cfg(target_os = "linux")]
            token: None,
        }
    }
}

pub(crate) fn register_window(
    cx: &mut gpui_kit::App,
    window: gpui_kit::AnyWindowHandle,
    app: gpui_kit::Entity<crate::ui::Spk>,
    base: Url,
    activated: async_channel::Sender<NativeActivation>,
    activations: async_channel::Receiver<NativeActivation>,
) {
    let app = app.downgrade();
    // GPUI Kit installs its own handler at init. SPK's system alerts use exact targets.
    let native = activated.clone();
    cx.on_system_notification_response(move |response, _| {
        let _ = native.try_send(response.into());
    });
    // Keep the accessibility fixture from replacing the desktop app's toast activator.
    #[cfg(target_os = "windows")]
    if !(cfg!(debug_assertions) && std::env::var_os("SPK_TEST_ACCESSIBILITY").is_some())
        && let Some(notifications) = windows::Notifications::register(activated)
    {
        cx.set_global(notifications);
    }
    #[cfg(target_os = "linux")]
    cx.spawn(async move |cx| {
        if let Some(notifications) = linux::Notifications::register(activated).await {
            cx.update(|cx| cx.set_global(notifications));
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(activation) = activations.recv().await {
            let response = activation.response;
            let target = if let Some(response) = &response {
                if response
                    .action_id
                    .as_deref()
                    .is_some_and(|action| action != "open")
                {
                    continue;
                }
                let Some(target) = activation_target(&base, &response.tag) else {
                    continue;
                };
                Some(target)
            } else {
                None
            };
            cx.update(|cx| {
                if let Some(response) = response {
                    cx.dismiss_system_notification(&response.tag).detach();
                }
                cx.activate(true);
                let _ = window.update(cx, |_, window, cx| {
                    #[cfg(target_os = "linux")]
                    linux::activate_window(window, activation.token.as_deref());
                    #[cfg(not(target_os = "linux"))]
                    window.activate_window();
                    if let Some(target) = target {
                        let _ = app.update(cx, |this, cx| this.open_alert(target, window, cx));
                    }
                });
            });
        }
    })
    .detach();
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
    fn notification_preferences_are_isolated_by_account_and_instance() {
        let root = tempfile::tempdir().unwrap();
        let here = Url::parse("https://spk.example").unwrap();
        let there = Url::parse("https://spk.example:8443").unwrap();
        let alice = scoped_path(root.path(), &here, "Alice");
        let other_account = scoped_path(root.path(), &here, "alice");
        let other_instance = scoped_path(root.path(), &there, "Alice");

        let mut preferences = Preferences::load(&alice);
        preferences.muted.insert("general".into());
        preferences.system_previews = false;
        preferences.save(&alice).unwrap();

        for other in [other_account, other_instance] {
            let settings = Preferences::load(&other);
            assert!(settings.muted.is_empty());
            assert!(settings.system_previews);
            settings.save(&other).unwrap();
        }
        let settings = Preferences::load(&alice);
        assert!(settings.muted.contains("general"));
        assert!(!settings.system_previews);
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
