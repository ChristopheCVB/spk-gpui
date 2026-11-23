use gpui_kit::component::{
    menu::PopupMenuItem,
    notification::{Notification, NotificationDelivery},
};
use url::Url;

use crate::{
    api::MessageTarget,
    notifications::{self, Alert, Delivery},
};

use super::*;

pub(super) fn account_id(view: &View) -> Option<&str> {
    match view {
        View::SignedIn(sidebar) => sidebar.profile.id.as_deref().filter(|id| !id.is_empty()),
        _ => None,
    }
}

pub(super) fn preferences_path(user_id: &str) -> Option<std::path::PathBuf> {
    let base = Url::parse(env!("SPK_URL")).expect("validated SPK URL");
    notifications::Preferences::path(&base, user_id)
}

pub(super) fn mute_item(channel_id: String, muted: bool, app: WeakEntity<Spk>) -> PopupMenuItem {
    PopupMenuItem::new(if muted {
        "Unmute notifications"
    } else {
        "Mute notifications"
    })
    .on_click(move |_, _, cx| {
        let _ = app.update(cx, |this, cx| {
            this.change_notifications(
                |preferences| {
                    if !preferences.muted.remove(&channel_id) {
                        preferences.muted.insert(channel_id.clone());
                    }
                },
                cx,
            );
        });
    })
}

impl Spk {
    pub(super) fn change_notifications(
        &mut self,
        change: impl FnOnce(&mut notifications::Preferences),
        cx: &mut Context<Self>,
    ) {
        let Some(user_id) = account_id(&self.view).filter(|_| !self.signing_out) else {
            return;
        };
        change(&mut self.notification_preferences);
        self.notification_error = preferences_path(user_id)
            .is_none_or(|path| self.notification_preferences.save(&path).is_err());
        cx.notify();
    }

    pub(super) fn show_alert(
        &mut self,
        alert: Alert,
        sidebar: &Sidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible_channel = sidebar.chat.as_ref().filter(|chat| {
            !self.video_stage.fullscreen && !chat.unavailable() && !window.has_active_dialog(cx)
        });
        let Some(delivery) = alert.delivery(
            &self.notification_preferences,
            sidebar.user_status == crate::api::UserStatus::Busy,
            window.is_window_active(),
            visible_channel.map(|chat| chat.channel_id.as_str()),
        ) else {
            return;
        };
        self.cues.play(Sound::Message, true);
        let body =
            crate::chat_markdown::notification_preview(&alert.body, alert.has_attachment, |id| {
                sidebar.message_author(id)
            });
        let target = alert.target;
        match delivery {
            Delivery::InApp => {
                let app = cx.weak_entity();
                window.push_notification(
                    Notification::new()
                        .delivery(NotificationDelivery::InApp)
                        .title(alert.title)
                        .message(body)
                        .action(move |_, _, cx| {
                            let app = app.clone();
                            let target = target.clone();
                            control_button("open-message-alert")
                                .label("Open")
                                .small()
                                .tab_stop(true)
                                .on_click(cx.listener(move |notification, _, window, cx| {
                                    notification.dismiss(window, cx);
                                    let _ = app.update(cx, |this, cx| {
                                        this.open_alert(target.clone(), window, cx);
                                    });
                                }))
                        })
                        .autohide(true),
                    cx,
                );
            }
            Delivery::System => {
                let (title, body) = if self.notification_preferences.system_previews {
                    (alert.title, body)
                } else {
                    ("SPK".into(), "New message".into())
                };
                let base = Url::parse(env!("SPK_URL")).expect("validated SPK URL");
                let notification = SystemNotification {
                    tag: notifications::activation_tag(&base, &target).into(),
                    title: title.into(),
                    body: body.into(),
                    actions: vec![SystemNotificationAction {
                        id: "open".into(),
                        label: "Open".into(),
                    }],
                };
                #[cfg(target_os = "linux")]
                if !notifications::linux::Notifications::show(&notification, cx) {
                    show_linux_notification(notification, target, window, cx);
                }
                #[cfg(target_os = "windows")]
                if !notifications::windows::Notifications::show(&notification, cx) {
                    cx.show_system_notification(notification);
                }
                #[cfg(target_os = "macos")]
                cx.show_system_notification(notification);
            }
        }
    }

    pub(crate) fn open_alert(
        &mut self,
        target: MessageTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.signing_out && matches!(self.view, View::SignedIn(_)) {
            self.dismiss_workspace_dialogs(window, cx);
            self.exit_video_fullscreen(window);
            self.device_picker = None;
            cx.notify();
        }
        self.alert_activation.send_replace(Some(target));
    }
}

#[cfg(target_os = "linux")]
fn show_linux_notification(
    notification: SystemNotification,
    target: MessageTarget,
    window: &mut Window,
    cx: &mut Context<Spk>,
) {
    use notify_rust::{Hint, Notification as XdgNotification, NotificationResponse, Timeout};

    let body = notification
        .body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let (activated, activation) = async_channel::bounded(1);
    let mut native = XdgNotification::new();
    native
        .appname("SPK")
        .summary(&notification.title)
        .body(&body)
        .hint(Hint::SuppressSound(true))
        .timeout(Timeout::Milliseconds(10_000))
        .action("default", "Open")
        .action("open", "Open");
    cx.background_spawn(async move {
        if let Ok(handle) = native.show_async().await {
            handle
                .wait_for_action_async(|action| {
                    if action.is_default_action()
                        || matches!(action, NotificationResponse::Action(key) if key == "open")
                    {
                        let _ = activated.try_send(());
                    }
                })
                .await;
        }
    })
    .detach();
    cx.spawn_in(window, async move |app, cx| {
        if activation.recv().await.is_ok() {
            let _ = app.update_in(cx, |app, window, cx| {
                cx.activate(true);
                window.activate_window();
                app.open_alert(target, window, cx);
            });
        }
    })
    .detach();
}
