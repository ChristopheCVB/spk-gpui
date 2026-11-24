use super::*;
use crate::session::validate_poll;

struct PollComposer {
    channel_id: String,
    commands: async_channel::Sender<Command>,
    opener: FocusHandle,
    question: Entity<InputState>,
    options: Vec<Entity<InputState>>,
    scroll: ScrollHandle,
    sending: bool,
    error: Option<&'static str>,
}

impl Spk {
    pub(super) fn open_poll_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = chat(&self.view).filter(|chat| chat.can_send()) else {
            return;
        };
        let channel_id = chat.channel_id.clone();
        let commands = self.commands.clone();
        let opener = self.chat_input.focus_handle(cx);
        let form = cx.new(|cx| {
            let question = PollComposer::input(300, window, cx);
            let mut form = PollComposer {
                channel_id,
                commands,
                opener,
                question,
                options: Vec::new(),
                scroll: ScrollHandle::new(),
                sending: false,
                error: None,
            };
            for _ in 0..2 {
                form.add_option(window, cx);
            }
            form
        });
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                form.read(cx).question.focus_handle(cx).focus(window, cx);
            }
            dialog
                .centered()
                .width((window.viewport_size().width * 0.9).min(px(440.)))
                .title("Create poll")
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
            InputEvent::PressEnter { .. } => form.submit(window, cx),
            _ => {}
        })
        .detach();
        input
    }

    fn add_option(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = Self::input(100, window, cx);
        let id = input.entity_id();
        cx.on_focus(&input.focus_handle(cx), window, move |form, _, _| {
            if let Some(index) = form
                .options
                .iter()
                .position(|input| input.entity_id() == id)
            {
                form.scroll.scroll_to_item(index);
            }
        })
        .detach();
        self.options.push(input.clone());
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
                .map(|option| option.read(cx).value())
                .collect(),
        )
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let (question, options) = self.values(cx);
        if Self::validation(&question, &options).is_err() {
            return;
        }
        let (completed, result) = tokio::sync::oneshot::channel();
        const CONNECTION_ERROR: &str = "Could not reach SPK. Try again.";
        if self
            .commands
            .try_send(Command::SendChatPoll {
                channel_id: self.channel_id.clone(),
                question: question.to_string(),
                options: options
                    .into_iter()
                    .map(|option| option.to_string())
                    .collect(),
                completed,
            })
            .is_err()
        {
            self.error = Some(CONNECTION_ERROR);
            cx.notify();
            return;
        }
        self.sending = true;
        self.error = None;
        cx.spawn_in(window, async move |form, cx| {
            let result = result.await.unwrap_or(Err(CONNECTION_ERROR));
            let _ = form.update_in(cx, |form, window, cx| match result {
                Ok(()) => {
                    window.close_dialog(cx);
                    form.opener.focus(window, cx);
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
                    .child(Input::new(&self.question).aria_label("Question")),
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
                    .children(self.options.iter().enumerate().map(|(index, input)| {
                        let label = format!("Option {}", index + 1);
                        div()
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
                                    .child(Input::new(input).aria_label(label)),
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
                            form.add_option(window, cx)
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
                            .label("Send")
                            .disabled(disabled)
                            .on_click(cx.listener(|form, _, window, cx| form.submit(window, cx))),
                    ),
            )
    }
}
