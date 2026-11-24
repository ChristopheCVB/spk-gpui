use gpui_kit::component::{dialog::DialogEntrance, switch::Switch};

use super::*;
use crate::{
    api::WebhookSettings,
    ui::icon_picker::{IconPicker, bundled_icon_names, icon_picker, icon_picker_field},
};

impl Spk {
    pub(super) fn new_webhook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_out || !matches!(self.view, View::SignedIn(_)) {
            return;
        }

        let opener = window.focused(cx);
        let fields = WebhookFields::new(None, window, cx);
        let input = fields.name.clone();
        let app = cx.entity();
        let editor = cx.new(|cx: &mut Context<WebhookEditor>| {
            cx.observe(&fields.icon, |_, _, cx| cx.notify()).detach();
            cx.subscribe_in(&input, window, |editor, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    editor.submit(window, cx);
                }
            })
            .detach();
            WebhookEditor {
                app,
                fields,
                pending: false,
                error: None,
                url: None,
                copied: false,
                copy_focus: cx.focus_handle(),
                opener,
            }
        });
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                input.focus_handle(cx).focus(window, cx);
            }
            let state = editor.read(cx);
            let title = if state.url.is_some() {
                "Webhook URL"
            } else {
                "New webhook"
            };
            let pending = state.pending;
            dialog
                .centered()
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
            icon: icon_picker(&bundled_icon_names(), icon, window, cx),
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
    fields: WebhookFields,
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
        self.pending = true;
        self.error = None;
        let settings = self.fields.settings(cx);
        let (reply, result) = tokio::sync::oneshot::channel();
        self.app.update(cx, |app, cx| {
            app.send(Command::CreateWebhook { settings, reply }, None, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = result
                .await
                .unwrap_or(Err("Could not reach the SPK connection. Try again."));
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                if !this.app.read(cx).signing_out
                    && matches!(this.app.read(cx).view, View::SignedIn(_))
                {
                    match result {
                        Ok(url) => {
                            this.url = Some(cx.new(|cx| {
                                InputState::new(window, cx).default_value(url.as_str().to_owned())
                            }));
                            this.app
                                .update(cx, |app, cx| app.close_sidebar_drawer(window, cx));
                            this.copy_focus.focus(window, cx);
                        }
                        Err(message) => this.error = Some(message),
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
        if self.app.read(cx).signing_out {
            self.url = None;
            return body;
        }
        if let Some(url) = &self.url {
            return body
                .child(Label::new("Shown only once. Copy the URL now. Generating a new URL stops this one from working."))
                .child(Input::new(url).w_full().aria_label("Webhook URL").readonly(true))
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

        body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Label::new("Name"))
                .child(
                    Input::new(&self.fields.name)
                        .w_full()
                        .aria_label("Name")
                        .disabled(self.pending),
                ),
        )
        .child(icon_picker_field(
            &self.fields.icon,
            Some(&self.fields.original_icon),
            |name| webhook_icon(Some(name)),
            self.pending,
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
                        .checked(self.fields.private)
                        .disabled(self.pending)
                        .tab_stop(true)
                        .on_change(cx.listener(|this, private: &bool, _, cx| {
                            this.fields.private = *private;
                            cx.notify();
                        })),
                )
                .child(
                    Label::new("Only you can see a private feed.")
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .children(channel_admin::channel_status(self.error.map(Err), cx))
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    control_button("cancel-webhook")
                        .label("Cancel")
                        .ghost()
                        .tab_stop(true)
                        .disabled(self.pending)
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                )
                .child(
                    control_button("create-webhook")
                        .primary()
                        .tab_stop(true)
                        .label(if self.pending {
                            "Creating…"
                        } else {
                            "Create"
                        })
                        .disabled(self.pending)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
        )
    }
}
