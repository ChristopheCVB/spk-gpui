use gpui_kit::component::{dialog::DialogEntrance, switch::Switch};

use super::*;
use crate::{
    api::WebhookSettings,
    session::{WebhookAction, WebhookError},
    ui::icon_picker::{IconPicker, icon_picker, icon_picker_field},
};

#[derive(Clone)]
enum WebhookForm {
    Create,
    Edit(String),
    Rotate(String),
    Delete(String),
}

impl WebhookForm {
    fn title(&self) -> &'static str {
        match self {
            Self::Create => "New webhook",
            Self::Edit(_) => "Edit webhook",
            Self::Rotate(_) => "New secret URL",
            Self::Delete(_) => "Delete webhook",
        }
    }

    fn id(&self) -> Option<&str> {
        match self {
            Self::Create => None,
            Self::Edit(id) | Self::Rotate(id) | Self::Delete(id) => Some(id),
        }
    }

    fn confirmation(&self, name: &str) -> Option<String> {
        match self {
            Self::Rotate(_) => Some(format!(
                "Generate a new URL for {name}? The old URL stops working immediately."
            )),
            Self::Delete(_) => Some(format!(
                "Delete {name}? This removes the feed and its alerts for everyone. Its URL stops working immediately."
            )),
            _ => None,
        }
    }

    fn button(&self, pending: bool) -> &'static str {
        match (self, pending) {
            (Self::Create, false) => "Create",
            (Self::Create, true) => "Creating…",
            (Self::Edit(_), false) => "Save",
            (Self::Edit(_), true) => "Saving…",
            (Self::Rotate(_), false) => "Generate new URL",
            (Self::Rotate(_), true) => "Generating…",
            (Self::Delete(_), false) => "Delete",
            (Self::Delete(_), true) => "Deleting…",
        }
    }
}

impl Spk {
    pub(super) fn new_webhook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_webhook_form(WebhookForm::Create, window, cx);
    }

    fn open_webhook_form(
        &mut self,
        form: WebhookForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        if self.signing_out {
            return;
        }
        let channel = if let Some(id) = form.id() {
            let Some(channel) = sidebar
                .webhook_feed(id)
                .filter(|feed| sidebar.can_manage_webhook(feed))
            else {
                return;
            };
            Some(channel)
        } else {
            None
        };
        let opener = window.focused(cx);
        let confirmation = channel.and_then(|channel| form.confirmation(channel_name(channel)));
        let fields = confirmation
            .is_none()
            .then(|| WebhookFields::new(channel, window, cx));
        let cancel_focus = cx.focus_handle();
        let initial_focus = fields.as_ref().map_or_else(
            || cancel_focus.clone(),
            |fields| fields.name.focus_handle(cx),
        );
        let title = form.title();
        let app = cx.entity();
        let editor = cx.new(|cx: &mut Context<WebhookEditor>| {
            if let Some(fields) = &fields {
                cx.subscribe_in(&fields.name, window, |editor, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        editor.submit(window, cx);
                    }
                })
                .detach();
            }
            WebhookEditor {
                app,
                form,
                fields,
                confirmation,
                cancel_focus,
                pending: false,
                error: None,
                url: None,
                copied: false,
                copy_focus: cx.focus_handle(),
                opener,
            }
        });
        let focus_once = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus_once.replace(false) {
                initial_focus.focus(window, cx);
            }
            let state = editor.read(cx);
            let title = if state.url.is_some() {
                "Webhook URL"
            } else {
                title
            };
            let pending = state.pending;
            dialog
                .centered()
                .border_0()
                .bg(cx.theme().popover)
                .entrance(DialogEntrance::None)
                .width(px(440.0))
                .title(title)
                .close_button(!pending)
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_cancel(move |_, _, _| !pending)
                .on_close({
                    let editor = editor.clone();
                    move |_, window, cx| {
                        editor.update(cx, |editor, cx| editor.restore_focus(window, cx));
                    }
                })
                .child(editor.clone())
        });
    }
}

#[derive(Clone)]
pub(super) struct WebhookRowMenu {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) muted: bool,
    pub(super) app: WeakEntity<Spk>,
}

impl WebhookRowMenu {
    pub(super) fn render(self, menu: SidebarRowMenu) -> AnyElement {
        let focus = menu.focus();
        let button = tool_button(
            "webhook-actions",
            IconName::Ellipsis,
            format!("Actions for {}", self.name),
        );
        menu.render(button, true, Anchor::TopRight, move |menu, _, cx| {
            self.items(menu, Some(&focus), cx)
        })
    }

    pub(super) fn items(
        &self,
        mut menu: PopupMenu,
        focus: Option<&FocusHandle>,
        cx: &App,
    ) -> PopupMenu {
        let id = self.id.clone();
        menu = menu
            .item(mark_read_item(id.clone(), self.app.clone()))
            .item(notifications::mute_item(
                id.clone(),
                self.muted,
                self.app.clone(),
            ))
            .item(
                PopupMenuItem::new("Copy link")
                    .icon(IconName::Link)
                    .on_click(move |_, _, cx| search::copy_conversation_link(&id, cx)),
            );
        let can_manage = self.app.upgrade().is_some_and(|app| {
            matches!(&app.read(cx).view, View::SignedIn(sidebar)
                if sidebar.webhook_feed(&self.id).is_some_and(|feed| sidebar.can_manage_webhook(feed)))
        });
        if can_manage {
            menu = menu.separator();
            for (label, icon, form) in [
                (
                    "Edit…",
                    IconName::Pencil,
                    WebhookForm::Edit(self.id.clone()),
                ),
                (
                    "New secret URL…",
                    IconName::RefreshCw,
                    WebhookForm::Rotate(self.id.clone()),
                ),
                (
                    "Delete…",
                    IconName::Trash,
                    WebhookForm::Delete(self.id.clone()),
                ),
            ] {
                let app = self.app.clone();
                let focus = focus.cloned();
                menu = menu.item(PopupMenuItem::new(label).icon(icon).on_click(
                    move |_, window, cx| {
                        let (app, focus, form) = (app.clone(), focus.clone(), form.clone());
                        window.defer(cx, move |window, cx| {
                            if let Some(focus) = focus {
                                focus.focus(window, cx);
                            }
                            let _ =
                                app.update(cx, |app, cx| app.open_webhook_form(form, window, cx));
                        });
                    },
                ));
            }
        }
        menu
    }
}

struct WebhookFields {
    name: Entity<InputState>,
    icon: IconPicker,
    original_icon: String,
    private: bool,
}

impl WebhookFields {
    fn new(channel: Option<&Channel>, window: &mut Window, cx: &mut App) -> Self {
        let name = channel
            .and_then(|channel| channel.name.clone())
            .unwrap_or_default();
        let icon = channel
            .and_then(|channel| channel.icon.as_deref())
            .unwrap_or("webhook");
        Self {
            name: cx.new(|cx| InputState::new(window, cx).default_value(name)),
            icon: icon_picker(icon, |name| webhook_icon(Some(name)), window, cx),
            original_icon: icon.to_owned(),
            private: channel.is_none_or(|channel| channel.private),
        }
    }

    fn settings(&self, cx: &App) -> WebhookSettings {
        WebhookSettings {
            name: self.name.read(cx).value().to_string(),
            icon: self
                .icon
                .read(cx)
                .selected_value()
                .map_or_else(|| self.original_icon.clone(), ToString::to_string),
            private: self.private,
        }
    }
}

struct WebhookEditor {
    app: Entity<Spk>,
    form: WebhookForm,
    fields: Option<WebhookFields>,
    confirmation: Option<String>,
    cancel_focus: FocusHandle,
    pending: bool,
    error: Option<&'static str>,
    url: Option<Entity<InputState>>,
    copied: bool,
    copy_focus: FocusHandle,
    opener: Option<FocusHandle>,
}

impl WebhookEditor {
    fn restore_focus(&self, window: &mut Window, cx: &mut App) {
        let app = self.app.read(cx);
        if !app.sidebar_drawer_open {
            let focus = self
                .opener
                .as_ref()
                .filter(|opener| app.workspace_focus.contains(opener, window))
                .unwrap_or(&app.sidebar_toggle_focus)
                .clone();
            focus.focus(window, cx);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending || self.url.is_some() || self.app.read(cx).signing_out {
            return;
        }
        self.error = None;
        let action = match (&self.form, &self.fields) {
            (WebhookForm::Create, Some(fields)) => WebhookAction::Create(fields.settings(cx)),
            (WebhookForm::Edit(id), Some(fields)) => WebhookAction::Edit {
                id: id.clone(),
                settings: fields.settings(cx),
            },
            (WebhookForm::Rotate(id), _) => WebhookAction::Rotate(id.clone()),
            (WebhookForm::Delete(id), _) => WebhookAction::Delete(id.clone()),
            _ => unreachable!("webhook form settings"),
        };
        let (reply, result) = tokio::sync::oneshot::channel();
        if !self.app.update(cx, |app, cx| {
            app.try_send(Command::MutateWebhook { action, reply }, None, cx)
        }) {
            cx.notify();
            return;
        }
        self.pending = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = result.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                if !this.app.read(cx).signing_out
                    && matches!(this.app.read(cx).view, View::SignedIn(_))
                {
                    match result {
                        Ok(Ok(Some(url))) => {
                            this.url = Some(cx.new(|cx| {
                                InputState::new(window, cx).default_value(url.as_str().to_owned())
                            }));
                            this.app
                                .update(cx, |app, cx| app.close_sidebar_drawer(window, cx));
                            this.copy_focus.focus(window, cx);
                        }
                        Ok(Ok(None)) => this.close(window, cx),
                        Ok(Err(WebhookError::Validation(message))) => this.error = Some(message),
                        Ok(Err(WebhookError::Action(_))) => {}
                        Err(_) => this.app.update(cx, |app, cx| {
                            app.show_action_result(
                                ActionResult::error(
                                    "Could not reach the SPK connection. Try again.",
                                ),
                                cx,
                            )
                        }),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close(&self, window: &mut Window, cx: &mut App) {
        window.close_dialog(cx);
        self.restore_focus(window, cx);
    }
}

impl Render for WebhookEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = div().flex().flex_col().gap_3();
        if self.app.read(cx).signing_out || !matches!(self.app.read(cx).view, View::SignedIn(_)) {
            self.url = None;
            return body;
        }
        if let Some(url) = &self.url {
            return body
                .child(Label::new("Shown only once. Copy the URL now. Generating a new URL stops this one from working."))
                .child(surface_input(url, cx.theme().background, cx).w_full().aria_label("Webhook URL").readonly(true))
                .child(
                    div().flex().justify_end().gap_2()
                        .child(
                            control_button("copy-webhook-url")
                                .label(if self.copied { "Copied" } else { "Copy URL" })
                                .track_focus(&self.copy_focus)
                                .tab_stop(true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if !this.app.read(cx).signing_out
                                        && let Some(url) = &this.url
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(url.read(cx).value().to_string()));
                                        this.copied = true;
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(
                            control_button("done-webhook-url").primary().label("Done").tab_stop(true)
                                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                        ),
                );
        }

        body.children(
            self.confirmation
                .as_ref()
                .map(|text| Label::new(text.clone())),
        )
        .when_some(self.fields.as_ref(), |body, fields| {
            body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new("Name"))
                    .child(
                        surface_input(&fields.name, cx.theme().background, cx)
                            .w_full()
                            .aria_label("Name")
                            .disabled(self.pending),
                    ),
            )
            .child(icon_picker_field(
                &fields.icon,
                self.pending,
                cx.theme().background,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Switch::new("webhook-private")
                            .label("Private")
                            .accessibility_label("Private")
                            .checked(fields.private)
                            .disabled(self.pending)
                            .tab_stop(true)
                            .on_change(cx.listener(|this, private: &bool, _, cx| {
                                if let Some(fields) = &mut this.fields {
                                    fields.private = *private;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Label::new(if matches!(self.form, WebhookForm::Edit(_)) {
                            "Only the owner can see a private feed."
                        } else {
                            "Only you can see a private feed."
                        })
                        .text_color(cx.theme().muted_foreground),
                    ),
            )
        })
        .children(channel_admin::editor_error(self.error, cx))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    control_button("cancel-webhook")
                        .track_focus(&self.cancel_focus)
                        .label("Cancel")
                        .ghost()
                        .tab_stop(true)
                        .disabled(self.pending)
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                )
                .child(
                    control_button("submit-webhook")
                        .when(matches!(self.form, WebhookForm::Delete(_)), |button| {
                            button.danger()
                        })
                        .when(!matches!(self.form, WebhookForm::Delete(_)), |button| {
                            button.primary()
                        })
                        .tab_stop(true)
                        .label(self.form.button(self.pending))
                        .disabled(self.pending)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
        )
    }
}
