use super::*;
use crate::session::Outgoing;

const UNCONFIRMED: &str =
    "Delivery unconfirmed. Resending may post a duplicate. Check message history.";

impl Spk {
    pub(super) fn chat_send_recovery(
        &self,
        sidebar: &Sidebar,
        outgoing: &Outgoing,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let unconfirmed = matches!(outgoing.status, Delivery::Unconfirmed);
        let status = match &outgoing.status {
            Delivery::Failed(error) => format!("Could not send message. {error}"),
            _ => UNCONFIRMED.into(),
        };
        let body = message_markdown(
            &outgoing.draft.body,
            |id| sidebar.message_author(id),
            |_| false,
            None,
        )
        .into_owned();
        let mut context = Vec::new();
        if let Some(reply) = &outgoing.draft.reply {
            context.push(format!(
                "Reply to {}",
                sidebar.message_author(&reply.user_id)
            ));
        }
        if let Some(gif) = &outgoing.draft.gif {
            context.push(format!("GIF · {}", gif.item.title));
        }
        context.extend(
            outgoing
                .draft
                .uploads
                .iter()
                .map(|upload| upload.filename.clone()),
        );
        let context = context.join(" · ");
        let nonce = outgoing.nonce.clone();
        let edit = outgoing.clone();
        div()
            .id(SharedString::from(format!("send-recovery-{nonce}")))
            .mb_2()
            .rounded_md()
            .bg(theme.muted)
            .p_2()
            .when(!body.is_empty(), |row| {
                row.child(Label::new(body).truncate())
            })
            .when(!context.is_empty(), |row| {
                row.child(
                    Label::new(context.clone())
                        .text_color(theme.muted_foreground)
                        .truncate(),
                )
            })
            .child(Label::new(status).text_color(theme.muted_foreground))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        control_button("edit-send")
                            .small()
                            .tab_stop(true)
                            .label(if unconfirmed {
                                "Edit and resend"
                            } else {
                                "Edit message"
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_chat_send(&edit, &context, window, cx);
                            })),
                    )
                    .child(
                        control_button("resolve-send")
                            .small()
                            .tab_stop(true)
                            .label(if unconfirmed {
                                "Already sent"
                            } else {
                                "Retry send"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let command = if unconfirmed {
                                    Command::MarkChatSendDelivered(nonce.clone())
                                } else {
                                    Command::RetryChatSend(nonce.clone())
                                };
                                this.send(command, None, cx);
                            })),
                    ),
            )
    }

    fn edit_chat_send(
        &self,
        outgoing: &Outgoing,
        snapshot_context: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| {
            let input = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, 8)
                    .default_value(outgoing.draft.body.clone())
            });
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            SendRecovery {
                input,
                outgoing: outgoing.clone(),
                snapshot_context: snapshot_context.to_owned(),
                commands: self.commands.clone(),
                opener: self.chat_input.focus_handle(cx),
                sending: false,
                error: None,
            }
        });
        let title = if matches!(outgoing.status, Delivery::Unconfirmed) {
            "Edit and resend"
        } else {
            "Edit message"
        };
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                editor.read(cx).input.focus_handle(cx).focus(window, cx);
            }
            dialog
                .centered()
                .border_0()
                .bg(cx.theme().popover)
                .width((window.viewport_size().width * 0.9).min(px(480.)))
                .title(title)
                .on_cancel({
                    let editor = editor.clone();
                    move |_, _, cx| !editor.read(cx).sending
                })
                .on_ok(|_, _, _| false)
                .on_close({
                    let editor = editor.clone();
                    move |_, window, cx| editor.read(cx).opener.clone().focus(window, cx)
                })
                .child(editor.clone())
        });
    }
}

struct SendRecovery {
    input: Entity<TextareaState>,
    outgoing: Outgoing,
    snapshot_context: String,
    commands: async_channel::Sender<Command>,
    opener: FocusHandle,
    sending: bool,
    error: Option<&'static str>,
}

impl SendRecovery {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.input.read(cx).value();
        if self.sending || !self.outgoing.draft.can_send(&body) {
            return;
        }
        let (completed, result) = tokio::sync::oneshot::channel();
        if self
            .commands
            .try_send(Command::ResendChatMessage {
                nonce: self.outgoing.nonce.clone(),
                body: body.to_string(),
                completed,
            })
            .is_err()
        {
            self.error = Some("Could not reach SPK. Try again.");
            cx.notify();
            return;
        }
        self.sending = true;
        cx.spawn_in(window, async move |editor, cx| {
            let result = result
                .await
                .unwrap_or(Err("Could not reach SPK. Try again."));
            let _ = editor.update_in(cx, |editor, window, cx| match result {
                Ok(()) => {
                    window.close_dialog(cx);
                    editor.opener.focus(window, cx);
                }
                Err(error) => {
                    editor.error = Some(error);
                    editor.sending = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for SendRecovery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.sending || !self.outgoing.draft.can_send(&self.input.read(cx).value());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when(
                matches!(self.outgoing.status, Delivery::Unconfirmed),
                |form| form.child(Label::new(UNCONFIRMED)),
            )
            .when(!self.snapshot_context.is_empty(), |form| {
                form.child(Label::new(self.snapshot_context.clone()).truncate())
            })
            .child(
                Textarea::new(&self.input)
                    .aria_label("Message to resend")
                    .disabled(self.sending),
            )
            .when_some(self.error, |form, error| {
                form.child(Label::new(error).text_color(cx.theme().danger))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        control_button("cancel-send-recovery")
                            .outline()
                            .tab_stop(true)
                            .label("Cancel")
                            .disabled(self.sending)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        control_button("resend-message")
                            .primary()
                            .tab_stop(true)
                            .label("Resend")
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|editor, _, window, cx| editor.submit(window, cx)),
                            ),
                    ),
            )
    }
}
