use gpui_kit::component::Disableable;

use super::*;

use crate::session::{ChatView, Delivery};

fn chat(view: &View) -> Option<&ChatView> {
    match view {
        View::SignedIn(sidebar) => sidebar.chat.as_ref(),
        _ => None,
    }
}

impl Spk {
    pub(super) fn update_chat(&mut self, next: &View, window: &mut Window, cx: &mut Context<Self>) {
        let previous = chat(&self.view);
        let next = chat(next);
        if previous.map(|chat| (&chat.channel_id, chat.unavailable()))
            != next.map(|chat| (&chat.channel_id, chat.unavailable()))
        {
            self.chat_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.chat_scroll = ScrollHandle::new();
            self.chat_scroll.scroll_to_bottom();
        } else if let (Some(previous), Some(next)) = (previous, next)
            && !Arc::ptr_eq(&previous.messages, &next.messages)
        {
            if previous.messages.first().map(|message| &message.id)
                != next.messages.first().map(|message| &message.id)
                && let Some(anchor) = previous.messages.get(self.chat_scroll.top_item())
                && let Some(index) = next
                    .messages
                    .iter()
                    .position(|message| message.id == anchor.id)
            {
                self.chat_scroll.scroll_to_top_of_item(index);
            } else if self.chat_scroll.max_offset().y + self.chat_scroll.offset().y < px(32.)
                || previous.messages.is_empty()
            {
                self.chat_scroll.scroll_to_bottom();
            }
        }
    }

    pub(super) fn submit_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = chat(&self.view).filter(|chat| chat.can_send() && chat.outgoing.is_none())
        else {
            return;
        };
        let body = self.chat_input.read(cx).value().to_string();
        if body.trim().is_empty() {
            return;
        }
        self.send(
            Command::SendChatMessage {
                channel_id: chat.channel_id.clone(),
                body,
            },
            None,
            cx,
        );
        if self.form_error.is_none() {
            self.chat_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.chat_scroll.scroll_to_bottom();
        }
    }

    pub(super) fn chat_stage(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let status = if chat.unavailable() {
            Some("Direct message unavailable.")
        } else if chat.history_failed() {
            Some("Could not load message history.")
        } else if chat.loading() {
            Some("Loading messages…")
        } else if chat.messages.is_empty() {
            Some("No messages yet.")
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when_some(status, |area, status| {
                area.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .child(Label::new(status).text_color(theme.muted_foreground))
                        .when(chat.history_failed(), |bar| {
                            bar.child(
                                Button::new("retry-history")
                                    .small()
                                    .tab_stop(true)
                                    .label("Retry")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(Command::RetryChatHistory, None, cx)
                                    })),
                            )
                        }),
                )
            })
            .when(
                chat.has_more_before && !chat.loading() && !chat.history_failed(),
                |area| {
                    area.child(
                        div().px_4().py_2().child(
                            Button::new("older-messages")
                                .small()
                                .tab_stop(true)
                                .label("Load older messages")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.send(Command::LoadOlderMessages, None, cx)
                                })),
                        ),
                    )
                },
            )
            .child(
                div()
                    .id("chat-history")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.chat_scroll)
                    .children(chat.messages.iter().map(|message| {
                        div()
                            .px_4()
                            .py_2()
                            .flex_shrink_0()
                            .child(
                                Label::new(sidebar.message_author(&message.user_id).to_owned())
                                    .font_semibold(),
                            )
                            .child(div().child(SharedString::from(message.body.clone())))
                    })),
            )
            .children((!chat.unavailable()).then(|| {
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .flex_shrink_0()
                    .when_some(chat.outgoing.as_ref(), |composer, outgoing| {
                        let (status, retry, dismiss) = match &outgoing.status {
                            Delivery::Sending(_) => ("Sending…".into(), false, false),
                            Delivery::Failed(error) => {
                                (format!("Could not send message. {error}"), true, true)
                            }
                            Delivery::Unconfirmed => (
                                "Delivery unconfirmed. Check message history.".into(),
                                false,
                                true,
                            ),
                        };
                        composer.child(
                            div()
                                .pb_2()
                                .child(Label::new(outgoing.body.clone()).truncate())
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            Label::new(status).text_color(theme.muted_foreground),
                                        )
                                        .when(retry, |bar| {
                                            bar.child(
                                                Button::new("retry-send")
                                                    .small()
                                                    .tab_stop(true)
                                                    .label("Retry send")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.send(Command::RetryChatSend, None, cx)
                                                    })),
                                            )
                                        })
                                        .when(dismiss, |bar| {
                                            bar.child(
                                                Button::new("dismiss-send")
                                                    .small()
                                                    .tab_stop(true)
                                                    .label(if retry {
                                                        "Edit message"
                                                    } else {
                                                        "Dismiss"
                                                    })
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.dismiss_chat_send(window, cx)
                                                        },
                                                    )),
                                            )
                                        }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap_2()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Textarea::new(&self.chat_input)
                                        .aria_label("Write a message")
                                        .disabled(chat.outgoing.is_some() || !chat.can_send()),
                                ),
                            )
                            .child(
                                Button::new("send-chat")
                                    .label("Send")
                                    .tab_stop(true)
                                    .disabled(chat.outgoing.is_some() || !chat.can_send())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_chat(window, cx)
                                    })),
                            ),
                    )
            }))
    }

    fn dismiss_chat_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let edit = chat(&self.view)
            .and_then(|chat| chat.outgoing.as_ref())
            .and_then(|outgoing| {
                matches!(outgoing.status, Delivery::Failed(_)).then(|| outgoing.body.clone())
            });
        self.send(Command::DismissChatSend, None, cx);
        if self.form_error.is_none()
            && let Some(body) = edit
        {
            self.chat_input
                .update(cx, |input, cx| input.set_value(body, window, cx));
        }
    }
}
