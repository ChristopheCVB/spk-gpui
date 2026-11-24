use super::*;
use crate::{
    api::PollEditOption,
    session::{PollEditOutcome, validate_poll},
};
use futures::future::Either;

const CONNECTION_ERROR: &str = "Could not reach SPK. Try again.";

struct OptionField {
    id: Option<String>,
    input: Entity<InputState>,
}

struct PollComposer {
    channel_id: String,
    message_id: Option<String>,
    commands: async_channel::Sender<Command>,
    opener: FocusHandle,
    question: Entity<InputState>,
    options: Vec<OptionField>,
    scroll: ScrollHandle,
    sending: bool,
    error: Option<&'static str>,
}

impl Spk {
    pub(super) fn open_poll_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = chat(&self.view).filter(|chat| chat.can_send()) else {
            return;
        };
        self.open_poll_form(
            &chat.channel_id,
            None,
            self.chat_input.focus_handle(cx),
            window,
            cx,
        );
    }

    pub(super) fn open_poll_editor(
        &self,
        message: &ChatMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opener = self
            .chat_row_focus
            .borrow()
            .get(&message.id)
            .cloned()
            .unwrap_or_else(|| self.chat_input.focus_handle(cx));
        self.open_poll_form(&message.channel_id, Some(message), opener, window, cx);
    }

    fn open_poll_form(
        &self,
        channel_id: &str,
        message: Option<&ChatMessage>,
        opener: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.new(|cx| {
            let question = PollComposer::input(300, window, cx);
            let mut form = PollComposer {
                channel_id: channel_id.to_owned(),
                message_id: message.map(|message| message.id.clone()),
                commands: self.commands.clone(),
                opener,
                question,
                options: Vec::new(),
                scroll: ScrollHandle::new(),
                sending: false,
                error: None,
            };
            if let Some(message) = message
                && let Some(poll) = &message.poll
            {
                form.question.update(cx, |input, cx| {
                    input.set_value(message.body.clone(), window, cx)
                });
                for option in &poll.options {
                    form.add_option(Some(option.id.clone()), window, cx)
                        .update(cx, |input, cx| {
                            input.set_value(option.label.clone(), window, cx)
                        });
                }
            } else {
                for _ in 0..2 {
                    form.add_option(None, window, cx);
                }
            }
            form
        });
        let title = if message.is_some() {
            "Edit poll"
        } else {
            "Create poll"
        };
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                form.read(cx).question.focus_handle(cx).focus(window, cx);
            }
            dialog
                .centered()
                .width((window.viewport_size().width * 0.9).min(px(440.)))
                .title(title)
                .on_cancel({
                    let form = form.clone();
                    move |_, _, cx| !form.read(cx).sending
                })
                .on_ok(|_, _, _| false)
                .on_close({
                    let form = form.clone();
                    move |_, window, cx| form.read(cx).opener.clone().focus(window, cx)
                })
                .child(form.clone())
        });
    }
}

impl PollComposer {
    fn input(limit: usize, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .validate(move |text, _| text.encode_utf16().count() <= limit)
        });
        cx.subscribe_in(&input, window, |form, _, event, window, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => form.submit(None, window, cx),
            _ => {}
        })
        .detach();
        input
    }

    fn add_option(
        &mut self,
        option_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = Self::input(100, window, cx);
        let id = input.entity_id();
        cx.on_focus(&input.focus_handle(cx), window, move |form, _, _| {
            if let Some(index) = form
                .options
                .iter()
                .position(|option| option.input.entity_id() == id)
            {
                form.scroll.scroll_to_item(index);
            }
        })
        .detach();
        self.options.push(OptionField {
            id: option_id,
            input: input.clone(),
        });
        input
    }

    fn validation(question: &str, options: &[SharedString]) -> Result<(), &'static str> {
        validate_poll(question, options.iter().map(SharedString::as_ref)).map(|_| ())
    }

    fn values(&self, cx: &App) -> (SharedString, Vec<SharedString>) {
        (
            self.question.read(cx).value(),
            self.options
                .iter()
                .map(|option| option.input.read(cx).value())
                .collect(),
        )
    }

    fn submit(&mut self, reset_votes: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let (question, options) = self.values(cx);
        if Self::validation(&question, &options).is_err() {
            return;
        }
        let options = options.into_iter().map(|option| option.to_string());
        let channel_id = self.channel_id.clone();
        let question = question.to_string();
        let (command, result) = if let Some(message_id) = &self.message_id {
            let (completed, result) = tokio::sync::oneshot::channel();
            (
                Command::EditChatPoll {
                    channel_id,
                    message_id: message_id.clone(),
                    question,
                    options: self
                        .options
                        .iter()
                        .zip(options)
                        .map(|(field, label)| PollEditOption {
                            id: field.id.clone(),
                            label,
                        })
                        .collect(),
                    reset_votes,
                    completed,
                },
                Either::Left(async move { result.await.unwrap_or(Err(CONNECTION_ERROR)) }),
            )
        } else {
            let (completed, result) = tokio::sync::oneshot::channel();
            (
                Command::SendChatPoll {
                    channel_id,
                    question,
                    options: options.collect(),
                    completed,
                },
                Either::Right(async move {
                    result
                        .await
                        .unwrap_or(Err(CONNECTION_ERROR))
                        .map(|()| PollEditOutcome::Saved)
                }),
            )
        };
        if self.commands.try_send(command).is_err() {
            self.error = Some(CONNECTION_ERROR);
            cx.notify();
            return;
        }
        self.sending = true;
        self.error = None;
        cx.spawn_in(window, async move |form, cx| {
            let result = result.await;
            let _ = form.update_in(cx, |form, window, cx| match result {
                Ok(PollEditOutcome::Saved) => {
                    window.close_dialog(cx);
                    form.opener.focus(window, cx);
                }
                Ok(PollEditOutcome::ConfirmVoteChange) => {
                    form.sending = false;
                    form.confirm_vote_change(window, cx);
                    cx.notify();
                }
                Err(error) => {
                    form.sending = false;
                    form.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn confirm_vote_change(&self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.entity().downgrade();
        let keep_focus = cx.focus_handle();
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                keep_focus.focus(window, cx);
            }
            let save = |reset| {
                let form = form.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    let _ = form.update(cx, |form, cx| form.submit(Some(reset), window, cx));
                }
            };
            dialog
                .centered()
                .width((window.viewport_size().width * 0.9).min(px(440.)))
                .title("Change poll options")
                .on_ok(|_, _, _| false)
                .child(Label::new(
                    "People have already voted. What should happen to their votes?",
                ))
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            control_button("cancel-poll-edit")
                                .outline()
                                .tab_stop(true)
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            control_button("keep-poll-votes")
                                .outline()
                                .tab_stop(true)
                                .track_focus(&keep_focus)
                                .label("Keep votes")
                                .on_click(save(false)),
                        )
                        .child(
                            control_button("reset-poll-votes")
                                .danger()
                                .tab_stop(true)
                                .label("Reset votes")
                                .on_click(save(true)),
                        ),
                )
        });
    }
}

impl Render for PollComposer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (question, options) = self.values(cx);
        let validation = Self::validation(&question, &options);
        let disabled = self.sending || validation.is_err();
        let error = self.error.or_else(|| {
            validation.err().filter(|_| {
                !question.trim().is_empty()
                    && options.iter().all(|option| !option.trim().is_empty())
            })
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new("Question"))
                    .child(
                        Input::new(&self.question)
                            .aria_label("Question")
                            .disabled(self.sending),
                    ),
            )
            .child(
                div()
                    .id("poll-options")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .max_h(window.viewport_size().height * 0.45)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(self.options.iter().enumerate().map(|(index, option)| {
                        let input = &option.input;
                        let label = format!("Option {}", index + 1);
                        div()
                            .id(("poll-option", input.entity_id()))
                            .flex()
                            .items_end()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .flex_1()
                                    .child(Label::new(label.clone()))
                                    .child(
                                        Input::new(input).aria_label(label).disabled(self.sending),
                                    ),
                            )
                            .child(
                                tool_button(
                                    ("remove-poll-option", index),
                                    IconName::X,
                                    format!("Remove option {}", index + 1),
                                )
                                .tab_stop(true)
                                .disabled(self.options.len() <= 2 || self.sending)
                                .on_click(cx.listener(
                                    move |form, _, window, cx| {
                                        form.options.remove(index);
                                        form.options[index.min(form.options.len() - 1)]
                                            .input
                                            .focus_handle(cx)
                                            .focus(window, cx);
                                        cx.notify();
                                    },
                                )),
                            )
                    })),
            )
            .child(
                div().child(
                    control_button("add-poll-option")
                        .ghost()
                        .tab_stop(true)
                        .label("Add option")
                        .disabled(self.options.len() >= 10 || self.sending)
                        .on_click(cx.listener(|form, _, window, cx| {
                            form.add_option(None, window, cx)
                                .focus_handle(cx)
                                .focus(window, cx);
                            cx.notify();
                        })),
                ),
            )
            .when_some(error, |panel, error| {
                panel.child(Label::new(error).text_color(theme.danger))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        control_button("cancel-poll")
                            .outline()
                            .tab_stop(true)
                            .label("Cancel")
                            .disabled(self.sending)
                            .on_click(cx.listener(|form, _, window, cx| {
                                window.close_dialog(cx);
                                form.opener.focus(window, cx);
                            })),
                    )
                    .child(
                        control_button("send-poll")
                            .primary()
                            .tab_stop(true)
                            .label(if self.message_id.is_some() {
                                "Save"
                            } else {
                                "Send"
                            })
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|form, _, window, cx| form.submit(None, window, cx)),
                            ),
                    ),
            )
    }
}
