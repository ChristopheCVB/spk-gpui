mod markdown;

use std::{
    cell::RefCell,
    hash::{DefaultHasher, Hash, Hasher},
};

use gpui_kit::component::{
    Disableable,
    menu::{DropdownMenu, PopupMenuItem},
    text::{MarkdownExtensions, TextView},
};

use markdown::{message_markdown, safe_link};

use super::*;

use crate::{
    api::{ChatMessage, UnreadState},
    session::{ChatView, Delivery},
};

fn chat(view: &View) -> Option<&ChatView> {
    match view {
        View::SignedIn(sidebar) => sidebar.chat.as_ref(),
        _ => None,
    }
}

pub(super) fn unread_badges(counts: UnreadState, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .when(counts.mentions > 0, |badges| {
            badges.child(
                Label::new(format!("@{}", counts.mentions))
                    .text_xs()
                    .font_semibold()
                    .px_1()
                    .rounded_md()
                    .bg(theme.primary.opacity(0.16))
                    .text_color(theme.primary),
            )
        })
        .when(counts.unread > 0, |badges| {
            badges.child(
                Label::new(counts.unread.to_string())
                    .text_xs()
                    .px_1()
                    .rounded_md()
                    .bg(theme.secondary)
                    .text_color(theme.foreground),
            )
        })
}

pub(super) fn unread_label(name: &str, counts: UnreadState) -> String {
    if counts.unread == 0 {
        return name.to_owned();
    }
    format!(
        "{name}, {} unread, {} pings",
        counts.unread, counts.mentions
    )
}

pub(super) fn mark_read_item(channel_id: String, app: WeakEntity<Spk>) -> PopupMenuItem {
    PopupMenuItem::new("Mark read").on_click(move |_, _, cx| {
        let _ = app.update(cx, |this, cx| {
            this.send(Command::MarkChatRead(channel_id.clone()), None, cx);
        });
    })
}

impl Spk {
    pub(super) fn read_visible_chat(&self, window: &Window) {
        if let Some(chat) = chat(&self.view).filter(|chat| chat.latest_ready())
            && window.is_window_active()
            && !self.video_stage.fullscreen
        {
            let _ = self
                .commands
                .try_send(Command::ReadVisibleChat(chat.channel_id.clone()));
        }
    }

    pub(super) fn dismiss_unread_marker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) || self.video_stage.fullscreen {
            return;
        }
        if let Some(chat) = chat(&self.view).filter(|chat| chat.first_unread_id.is_some()) {
            self.send(Command::MarkChatRead(chat.channel_id.clone()), None, cx);
        }
    }

    pub(super) fn update_chat(&mut self, next: &View, window: &mut Window, cx: &mut Context<Self>) {
        let previous = chat(&self.view);
        let next = chat(next);
        if let Some(next) = next.filter(|chat| chat.latest_ready())
            && previous.is_none_or(|previous| {
                previous.channel_id != next.channel_id
                    || !previous.latest_ready()
                    || previous.messages.last().map(|message| &message.id)
                        != next.messages.last().map(|message| &message.id)
            })
            && window.is_window_active()
            && !self.video_stage.fullscreen
        {
            let _ = self
                .commands
                .try_send(Command::ReadVisibleChat(next.channel_id.clone()));
        }
        if previous.map(|chat| (&chat.channel_id, chat.unavailable()))
            != next.map(|chat| (&chat.channel_id, chat.unavailable()))
        {
            self.chat_input.update(cx, |input, cx| {
                input.set_value(
                    next.map_or("", |chat| chat.draft.body.as_str()).to_owned(),
                    window,
                    cx,
                )
            });
            self.chat_spoilers.clear();
            self.chat_scroll = ScrollHandle::new();
            self.chat_scroll.scroll_to_bottom();
        } else if let (Some(previous), Some(next)) = (previous, next) {
            let acknowledged = previous.outgoing.as_ref().is_some_and(|pending| {
                next.outgoing.is_none()
                    && !next.loading()
                    && next.draft.body.is_empty()
                    && self.chat_input.read(cx).value().as_ref() == pending.body
            });
            let restored = previous.loading()
                && !next.loading()
                && !next.draft.body.is_empty()
                && self.chat_input.read(cx).value().is_empty();
            if acknowledged || restored {
                self.chat_input.update(cx, |input, cx| {
                    input.set_value(next.draft.body.clone(), window, cx)
                });
            }
            if previous.jump != next.jump
                && let Some((id, _)) = &next.jump
                && let Some(index) = next.messages.iter().position(|message| message.id == *id)
            {
                self.chat_scroll.scroll_to_top_of_item(index);
                return;
            }
            if previous.has_more_after && !next.has_more_after {
                self.chat_scroll.scroll_to_bottom();
                return;
            }
            if Arc::ptr_eq(&previous.messages, &next.messages) {
                return;
            }
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

    pub(super) fn update_chat_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = chat(&self.view) {
            self.send(
                Command::UpdateChatDraft {
                    channel_id: chat.channel_id.clone(),
                    body: self.chat_input.read(cx).value().to_string(),
                },
                None,
                cx,
            );
        }
    }

    pub(super) fn submit_chat(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = chat(&self.view).filter(|chat| chat.can_send() && chat.outgoing.is_none())
        else {
            return;
        };
        let body = self.chat_input.read(cx).value().to_string();
        if body.trim().is_empty() {
            return;
        }
        let reading_older = chat.has_more_after;
        self.send(
            Command::SendChatMessage {
                channel_id: chat.channel_id.clone(),
                body,
            },
            None,
            cx,
        );
        if self.form_error.is_none() && !reading_older {
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
        } else if chat.jump_failed {
            Some("Original message unavailable.")
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
            .when_some(
                chat.first_unread_id.as_ref().filter(|id| {
                    (chat.has_more_before || chat.has_more_after)
                        && !chat.loading()
                        && !chat.jump_failed
                        && !chat.messages.iter().any(|message| &message.id == *id)
                }),
                |area, message_id| {
                    let channel_id = chat.channel_id.clone();
                    let message_id = message_id.clone();
                    area.child(
                        div().px_4().py_1().child(
                            Button::new("jump-to-unread")
                                .ghost()
                                .small()
                                .tab_stop(true)
                                .label("Jump to new messages")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(
                                        Command::JumpToChatMessage {
                                            channel_id: channel_id.clone(),
                                            message_id: message_id.clone(),
                                        },
                                        None,
                                        cx,
                                    );
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
                    .children(
                        chat.messages
                            .iter()
                            .map(|message| self.chat_message(sidebar, chat, message, cx)),
                    ),
            )
            .when(chat.has_more_after && !chat.loading(), |area| {
                area.child(
                    div().px_4().py_2().child(
                        Button::new("latest-messages")
                            .small()
                            .tab_stop(true)
                            .label("Jump to latest messages")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send(Command::LoadLatestMessages, None, cx)
                            })),
                    ),
                )
            })
            .children((!chat.unavailable()).then(|| {
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .flex_shrink_0()
                    .when(!chat.typing.is_empty(), |composer| {
                        let names: Vec<_> = chat
                            .typing
                            .iter()
                            .map(|id| sidebar.message_author(id))
                            .collect();
                        let text = match names.as_slice() {
                            [one] => format!("{one} is typing…"),
                            [one, two] => format!("{one} and {two} are typing…"),
                            _ => "Several people are typing…".into(),
                        };
                        composer.child(Label::new(text).text_color(theme.muted_foreground))
                    })
                    .when_some(chat.draft.reply.as_ref(), |composer, reply| {
                        let channel_id = chat.channel_id.clone();
                        composer.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .pb_2()
                                .child(
                                    Label::new(format!(
                                        "Replying to {}",
                                        sidebar.message_author(&reply.user_id)
                                    ))
                                    .font_semibold(),
                                )
                                .child(
                                    Label::new(
                                        message_markdown(
                                            &reply.preview,
                                            |id| sidebar.message_author(id),
                                            |_| false,
                                            None,
                                        )
                                        .into_owned(),
                                    )
                                    .truncate(),
                                )
                                .child(
                                    Button::new("cancel-reply")
                                        .small()
                                        .tab_stop(true)
                                        .label("Cancel reply")
                                        .disabled(chat.outgoing.is_some() || !chat.can_send())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.send(
                                                Command::SetChatReply {
                                                    channel_id: channel_id.clone(),
                                                    message_id: None,
                                                },
                                                None,
                                                cx,
                                            );
                                        })),
                                ),
                        )
                    })
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
                                .child(
                                    Label::new(
                                        message_markdown(
                                            &outgoing.body,
                                            |id| sidebar.message_author(id),
                                            |_| false,
                                            None,
                                        )
                                        .into_owned(),
                                    )
                                    .truncate(),
                                )
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
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.send(
                                                            Command::DismissChatSend,
                                                            None,
                                                            cx,
                                                        )
                                                    })),
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
                    .child(self.composer_tools(
                        sidebar,
                        chat.outgoing.is_some() || !chat.can_send(),
                        cx,
                    ))
            }))
    }

    fn chat_message(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
        message: &ChatMessage,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let channel = chat.channel_id.clone();
        let message_id = message.id.clone();
        let body = message.body.clone();
        let mut hasher = DefaultHasher::new();
        body.hash(&mut hasher);
        let spoiler_key = (message_id.clone(), hasher.finish());
        let reveals = self.chat_spoilers.get(&spoiler_key);
        let hidden = RefCell::new(Vec::new());
        let rendered = message_markdown(
            &body,
            |id| sidebar.message_author(id),
            |offset| {
                let revealed = reveals.is_some_and(|offsets| offsets.contains(&offset));
                if !revealed {
                    hidden.borrow_mut().push(offset);
                }
                revealed
            },
            Some(&self.chat_spoiler_link),
        )
        .into_owned();
        let hidden = hidden.into_inner();
        let link_key = spoiler_key.clone();
        let app = cx.weak_entity();
        let link_app = app.clone();
        let link_filter = self.chat_spoiler_link.clone();
        let link_prefix = self.chat_spoiler_link.clone();
        let text = TextView::markdown("message-body", rendered)
            .markdown_extensions(
                MarkdownExtensions::default()
                    .parser_revision(1)
                    .raw_html(false),
            )
            .link(move |link, _| {
                (link.url.starts_with(&link_filter) || safe_link(&link.url).is_some())
                    .then(|| link.clone())
            })
            .on_link_click(move |href, _, _, cx| {
                if let Some(offset) = href.strip_prefix(&link_prefix) {
                    if let Ok(offset) = offset.parse::<usize>() {
                        let _ = link_app.update(cx, |this, cx| {
                            this.chat_spoilers
                                .entry(link_key.clone())
                                .or_default()
                                .insert(offset);
                            cx.notify();
                        });
                    }
                } else if let Some(url) = safe_link(href) {
                    cx.open_url(url.as_str());
                }
            });
        let composer_busy = chat.outgoing.is_some() || !chat.can_send();
        let pinged = chat.direct_message_peer.is_none()
            && sidebar
                .profile
                .id
                .as_deref()
                .is_some_and(|id| message.pings(id));
        div()
            .id(SharedString::from(format!("message-{}", &message.id)))
            .px_4()
            .py_2()
            .flex_shrink_0()
            .when(
                pinged || chat.jump.as_ref().is_some_and(|(id, _)| id == &message.id),
                |row| row.bg(theme.primary.opacity(0.1)),
            )
            .when(chat.first_unread_id.as_ref() == Some(&message.id), |row| {
                row.child(
                    Label::new("New messages")
                        .text_xs()
                        .text_color(theme.primary)
                        .font_semibold()
                        .mb_1()
                        .border_t_1()
                        .border_color(theme.primary)
                        .w_full(),
                )
            })
            .when_some(message.reply_to.as_ref(), |row, reply| {
                row.child(if reply.deleted {
                    Label::new("Original message deleted")
                        .text_color(theme.muted_foreground)
                        .into_any_element()
                } else {
                    let channel_id = chat.channel_id.clone();
                    let message_id = reply.id.clone();
                    let preview = message_markdown(
                        &reply.preview,
                        |id| sidebar.message_author(id),
                        |_| false,
                        None,
                    );
                    Button::new("jump-reply")
                        .ghost()
                        .small()
                        .tab_stop(true)
                        .label(format!(
                            "{} · {preview}",
                            sidebar.message_author(&reply.user_id)
                        ))
                        .accessibility_label("Jump to the original message")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(
                                Command::JumpToChatMessage {
                                    channel_id: channel_id.clone(),
                                    message_id: message_id.clone(),
                                },
                                None,
                                cx,
                            );
                        }))
                        .into_any_element()
                })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Label::new(sidebar.message_author(&message.user_id).to_owned())
                                    .font_semibold(),
                            )
                            .when(pinged, |author| {
                                author.child(Label::new("Ping").text_xs().text_color(theme.primary))
                            }),
                    )
                    .child(
                        Button::new("message-actions")
                            .ghost()
                            .small()
                            .label("Actions")
                            .tab_stop(true)
                            .dropdown_menu(move |menu, _, _| {
                                let channel_id = channel.clone();
                                let quote_channel = channel.clone();
                                let message_id = message_id.clone();
                                let mark_channel = channel.clone();
                                let mark_message = message_id.clone();
                                let reply_app = app.clone();
                                let quote_app = app.clone();
                                let mark_app = app.clone();
                                let quoted = body
                                    .lines()
                                    .map(|line| format!("> {line}"))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let menu = menu
                                    .item(
                                        PopupMenuItem::new("Reply")
                                            .disabled(composer_busy)
                                            .on_click(move |_, window, cx| {
                                                let _ = reply_app.update(cx, |this, cx| {
                                                    this.send(
                                                        Command::SetChatReply {
                                                            channel_id: channel_id.clone(),
                                                            message_id: Some(message_id.clone()),
                                                        },
                                                        None,
                                                        cx,
                                                    );
                                                    this.chat_input.update(cx, |input, cx| {
                                                        input.focus(window, cx)
                                                    });
                                                });
                                            }),
                                    )
                                    .item(
                                        PopupMenuItem::new("Quote")
                                            .disabled(composer_busy)
                                            .on_click(move |_, window, cx| {
                                                let _ = quote_app.update(cx, |this, cx| {
                                                    if self::chat(&this.view).is_some_and(|chat| {
                                                        chat.channel_id == quote_channel
                                                            && chat.can_send()
                                                            && chat.outgoing.is_none()
                                                    }) {
                                                        this.chat_input.update(cx, |input, cx| {
                                                            let end = input.value().len();
                                                            input.set_selected_range(end..end, cx);
                                                            input.insert(
                                                                format!("\n\n{quoted}\n\n"),
                                                                window,
                                                                cx,
                                                            );
                                                            input.focus(window, cx);
                                                        });
                                                        this.update_chat_draft(cx);
                                                    }
                                                });
                                            }),
                                    )
                                    .item(PopupMenuItem::new("Mark as unread").on_click(
                                        move |_, _, cx| {
                                            let _ = mark_app.update(cx, |this, cx| {
                                                this.send(
                                                    Command::MarkChatUnread {
                                                        channel_id: mark_channel.clone(),
                                                        message_id: mark_message.clone(),
                                                    },
                                                    None,
                                                    cx,
                                                );
                                            });
                                        },
                                    ));
                                if hidden.is_empty() {
                                    menu
                                } else {
                                    let app = app.clone();
                                    let key = spoiler_key.clone();
                                    let offsets = hidden.clone();
                                    menu.item(PopupMenuItem::new("Reveal spoilers").on_click(
                                        move |_, _, cx| {
                                            let _ = app.update(cx, |this, cx| {
                                                this.chat_spoilers
                                                    .entry(key.clone())
                                                    .or_default()
                                                    .extend(&offsets);
                                                cx.notify();
                                            });
                                        },
                                    ))
                                }
                            }),
                    ),
            )
            .child(text)
    }

    fn composer_tools(
        &self,
        sidebar: &Sidebar,
        disabled: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let app = cx.weak_entity();
        let mention_app = app.clone();
        let people: Vec<_> = sidebar
            .people()
            .into_iter()
            .map(|(id, name)| (name.to_owned(), id.to_owned()))
            .collect();
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .pt_2()
            .children(
                [
                    ("Bold", "**", "**"),
                    ("Italic", "_", "_"),
                    ("Strikethrough", "~~", "~~"),
                    ("Inline code", "`", "`"),
                    ("Code block", "\n```\n", "\n```\n"),
                    ("Spoiler", "||", "||"),
                ]
                .into_iter()
                .map(|(label, before, after)| {
                    Button::new(label)
                        .ghost()
                        .small()
                        .tab_stop(true)
                        .label(label)
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.insert_chat_text(before, after, window, cx);
                        }))
                }),
            )
            .child(
                Button::new("mention-menu")
                    .ghost()
                    .small()
                    .label("Mention")
                    .tab_stop(true)
                    .disabled(disabled || people.is_empty())
                    .dropdown_menu(move |menu, _, _| {
                        people.iter().fold(menu, |menu, (name, id)| {
                            let app = mention_app.clone();
                            let mention = format!("<@{id}> ");
                            menu.item(PopupMenuItem::new(name.clone()).on_click(
                                move |_, window, cx| {
                                    let _ = app.update(cx, |this, cx| {
                                        this.insert_chat_text("", &mention, window, cx)
                                    });
                                },
                            ))
                        })
                    }),
            )
            .child(
                Button::new("emoji-menu")
                    .ghost()
                    .small()
                    .label("Emoji")
                    .tab_stop(true)
                    .disabled(disabled)
                    .dropdown_menu(move |menu, _, _| {
                        [
                            ("👍", "Thumbs up"),
                            ("❤️", "Heart"),
                            ("😂", "Laugh"),
                            ("😊", "Smile"),
                            ("👋", "Wave"),
                            ("🎉", "Celebrate"),
                        ]
                        .into_iter()
                        .fold(menu, |menu, (emoji, name)| {
                            let app = app.clone();
                            menu.item(PopupMenuItem::new(format!("{emoji} {name}")).on_click(
                                move |_, window, cx| {
                                    let _ = app.update(cx, |this, cx| {
                                        this.insert_chat_text("", emoji, window, cx)
                                    });
                                },
                            ))
                        })
                    }),
            )
    }

    fn insert_chat_text(
        &mut self,
        before: &str,
        after: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !chat(&self.view).is_some_and(|chat| chat.can_send() && chat.outgoing.is_none()) {
            return;
        }
        self.chat_input.update(cx, |input, cx| {
            let selected = input.selected_value();
            let start = input.selected_range().start + before.len();
            input.replace(format!("{before}{selected}{after}"), window, cx);
            if !before.is_empty() {
                input.set_selected_range(start..start + selected.len(), cx);
            }
            input.focus(window, cx);
        });
        self.update_chat_draft(cx);
    }
}
