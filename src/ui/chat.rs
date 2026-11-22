mod actions;
mod attachments;
mod gifs;
mod history;
mod reaction_picker;

pub(super) use actions::{MessageActionForm, PinPanel};
pub(super) use attachments::AttachmentUi;
pub(super) use reaction_picker::ReactionPicker;

use std::{
    cell::RefCell,
    collections::HashSet,
    hash::{DefaultHasher, Hash, Hasher},
};

use chrono::{DateTime, Local};
use gpui_kit::base::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{
    Disableable,
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    separator::Separator,
    text::{MarkdownExtensions, TextView},
    tooltip::Tooltip,
};

use history::{
    HistoryRow, date_label, history_message_at, history_message_index, history_rows,
    message_row_background, relative_time,
};

use super::*;

use crate::{
    api::{ChatMessage, MessageTarget, UnreadState},
    chat_markdown::{image_preview_links, message_markdown, safe_link},
    session::{ChatView, Delivery, Pins},
};

pub(super) const BOLD: (&str, &str) = ("**", "**");
pub(super) const ITALIC: (&str, &str) = ("_", "_");
pub(super) const STRIKETHROUGH: (&str, &str) = ("~~", "~~");
pub(super) const INLINE_CODE: (&str, &str) = ("`", "`");
pub(super) const CODE_BLOCK: (&str, &str) = ("\n```\n", "\n```\n");
pub(super) const QUOTE: (&str, &str) = ("\n> ", "");

fn chat(view: &View) -> Option<&ChatView> {
    match view {
        View::SignedIn(sidebar) => sidebar.chat.as_ref(),
        _ => None,
    }
}

pub(super) struct ChatDestination<'a> {
    pub icon: IconName,
    pub name: &'a str,
    placeholder_prefix: Option<char>,
}

pub(super) fn chat_destination<'a>(sidebar: &'a Sidebar, chat: &ChatView) -> ChatDestination<'a> {
    let (icon, name, fallback, prefix) = if let Some(peer) = &chat.direct_message_peer {
        (
            IconName::MessageCircle,
            (!chat.unavailable()).then(|| sidebar.message_author(peer)),
            "Direct message",
            '@',
        )
    } else {
        (
            IconName::Hash,
            sidebar
                .channels
                .iter()
                .flatten()
                .find(|channel| channel.id == chat.channel_id)
                .map(super::workspace::channel_name),
            "Text channel",
            '#',
        )
    };
    ChatDestination {
        icon,
        name: name.unwrap_or(fallback),
        placeholder_prefix: name.map(|_| prefix),
    }
}

fn composer_destination(view: &View) -> Option<(char, &str)> {
    let View::SignedIn(sidebar) = view else {
        return None;
    };
    let destination = chat_destination(sidebar, sidebar.chat.as_ref()?);
    Some((destination.placeholder_prefix?, destination.name))
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum HistoryPosition {
    Opening,
    Jump(usize),
    Anchor(usize, Pixels),
}

fn chat_history_viewport(
    rows: impl IntoIterator<Item = impl IntoElement>,
    scroll: &ScrollHandle,
    position: &Rc<Cell<Option<HistoryPosition>>>,
    latest_ready: bool,
) -> Div {
    let history = div().when(
        position
            .get()
            .is_some_and(|position| position != HistoryPosition::Opening || latest_ready),
        |history| {
            let position = position.clone();
            let scroll = scroll.clone();
            // The viewport and the target row have their current bounds after layout.
            history.on_children_prepainted(move |_, window, _| {
                let Some(position) = position.take() else {
                    return;
                };
                let bottom = -scroll.max_offset().y;
                let row = match position {
                    HistoryPosition::Opening => None,
                    HistoryPosition::Jump(index) => Some((index, px(0.))),
                    HistoryPosition::Anchor(index, offset) => Some((index, offset)),
                };
                let offset = if let Some((index, offset)) = row {
                    let Some(row) = scroll.bounds_for_item(index) else {
                        return;
                    };
                    (scroll.bounds().top() - row.top() + offset).clamp(bottom, px(0.))
                } else {
                    bottom
                };
                if scroll.offset().y != offset {
                    scroll.set_offset(point(scroll.offset().x, offset));
                    window.request_animation_frame();
                }
            })
        },
    );
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(
            history
                .id("chat-history")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .children(rows),
        )
        .child(Scrollbar::vertical(scroll).mode(ScrollbarMode::Always))
}

fn history_anchor<'a>(chat: &'a ChatView, scroll: &ScrollHandle) -> Option<(&'a str, Pixels)> {
    let (index, message) = history_message_at(chat, scroll.top_item())?;
    let bounds = scroll.bounds_for_item(index)?;
    Some((
        &message.id,
        bounds.top() + scroll.offset().y - scroll.bounds().top(),
    ))
}

fn chat_images(chat: &ChatView) -> impl Iterator<Item = &Arc<RenderImage>> {
    chat.preview_images
        .images()
        .chain(chat.gifs.iter().flat_map(|picker| picker.images()))
        .chain(chat.draft.gif.iter().filter_map(|gif| gif.image.as_ref()))
        .chain(
            chat.message_image
                .iter()
                .filter_map(|preview| preview.image.as_ref()),
        )
        .chain(
            chat.draft
                .uploads
                .iter()
                .filter_map(|upload| upload.preview.as_ref()),
        )
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
    pub(super) fn track_chat_time(window: &mut Window, cx: &mut Context<Self>) {
        let clock = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                clock.timer(Duration::from_secs(60)).await;
                if this
                    .update_in(cx, |this, _, cx| {
                        if chat(&this.view).is_some_and(|chat| !chat.messages.is_empty()) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

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

    pub(super) fn escape_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.close_reaction_picker(window, cx) {
            return true;
        }
        if self.chat_pins_focus.contains_focused(window, cx) && self.close_pins(window, cx) {
            return true;
        }
        let Some(chat) = chat(&self.view) else {
            return false;
        };
        if chat.gifs.is_some() {
            self.gif_button_focus.focus(window, cx);
            self.send(Command::ShowGifs(false), None, cx);
        } else if self.message_action_form.take().is_some() {
            self.chat_input
                .update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        } else if chat.draft.reply.is_some() {
            self.send(
                Command::SetChatReply {
                    channel_id: chat.channel_id.clone(),
                    message_id: None,
                },
                None,
                cx,
            );
        } else if chat.history_ready() {
            self.send(Command::MarkChatRead(chat.channel_id.clone()), None, cx);
        } else {
            return false;
        }
        true
    }

    pub(super) fn edit_last_message(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat_input.read(cx).value().is_empty() || self.message_action_form.is_some() {
            return false;
        }
        let View::SignedIn(sidebar) = &self.view else {
            return false;
        };
        let Some(chat) = sidebar
            .chat
            .as_ref()
            .filter(|chat| chat.can_send() && !chat.action_pending)
        else {
            return false;
        };
        let Some(me) = sidebar.profile.id.as_deref() else {
            return false;
        };
        let Some(message) = chat
            .messages
            .iter()
            .rev()
            .find(|message| message.user_id == me)
        else {
            return false;
        };
        let channel_id = chat.channel_id.clone();
        let message_id = message.id.clone();
        self.begin_message_action(
            &channel_id,
            &message_id,
            actions::ActionForm::Edit,
            window,
            cx,
        );
        true
    }

    pub(super) fn format_chat_input(
        &mut self,
        before: &str,
        after: &str,
        editing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if editing {
            self.message_action_input.update(cx, |input, cx| {
                let selected = input.selected_value();
                let start = input.selected_range().start + before.len();
                input.replace(format!("{before}{selected}{after}"), window, cx);
                input.set_selected_range(start..start + selected.len(), cx);
            });
        } else {
            self.insert_chat_text(before, after, window, cx);
        }
    }

    pub(super) fn update_chat(&mut self, next: &View, window: &mut Window, cx: &mut Context<Self>) {
        let destination = composer_destination(next);
        if destination != composer_destination(&self.view) {
            let placeholder = destination.map_or_else(
                || "Write a message".to_owned(),
                |(prefix, name)| format!("Message {prefix}{name}"),
            );
            self.chat_input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
            });
        }
        let previous = chat(&self.view);
        let signed_in = matches!(next, View::SignedIn(_));
        let next = chat(next);
        if previous
            .map(|chat| (&chat.channel_id, &chat.messages))
            .zip(next.map(|chat| (&chat.channel_id, &chat.messages)))
            .is_none_or(|((previous_id, previous), (next_id, next))| {
                previous_id != next_id || !Arc::ptr_eq(previous, next)
            })
        {
            let retained: HashSet<_> = next
                .into_iter()
                .flat_map(|chat| chat.messages.iter().map(|message| &message.id))
                .collect();
            self.chat_row_focus
                .get_mut()
                .retain(|message_id, _| retained.contains(message_id));
        }
        self.chat_attachments
            .sync(previous, next, signed_in, window);
        let mut retained: HashSet<_> = next
            .into_iter()
            .flat_map(chat_images)
            .map(Arc::as_ptr)
            .collect();
        for image in previous.into_iter().flat_map(chat_images) {
            if retained.insert(Arc::as_ptr(image)) {
                let _ = window.drop_image(image.clone());
            }
        }
        self.update_gif_input(previous, next, window, cx);
        Self::update_message_actions(&mut self.message_action_form, previous, next);
        if self.reaction_picker.as_ref().is_some_and(|picker| {
            !next.is_some_and(|chat| {
                chat.channel_id == picker.channel_id
                    && chat.can_send()
                    && chat
                        .messages
                        .iter()
                        .any(|message| message.id == picker.message_id)
            })
        }) {
            self.reaction_picker = None;
        }
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
        let reset = previous.map(|chat| (&chat.channel_id, chat.unavailable()))
            != next.map(|chat| (&chat.channel_id, chat.unavailable()));
        if reset
            || matches!(next.and_then(|chat| chat.pins.as_ref()), Some(Pins::Ready(pins))
                if pins.is_empty() || pins.len() == 1 && self.chat_pins == Some(PinPanel::All))
        {
            self.chat_pins = None;
        }
        if reset {
            self.chat_input.update(cx, |input, cx| {
                input.set_value(
                    next.map_or("", |chat| chat.draft.body.as_str()).to_owned(),
                    window,
                    cx,
                )
            });
            self.chat_spoilers.clear();
            self.chat_scroll_anchor = None;
            self.chat_history_position
                .set(next.map(|_| HistoryPosition::Opening));
            self.chat_scroll = ScrollHandle::new();
        } else if let (Some(previous), Some(next)) = (previous, next) {
            if previous.has_more_after
                && next.direct_message_peer.is_some()
                && next.loading()
                && next.messages.is_empty()
            {
                self.chat_scroll_anchor = history_anchor(previous, &self.chat_scroll)
                    .map(|(id, offset)| (id.to_owned(), offset));
            }
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
        }
        let previous_jump = previous
            .filter(|_| !reset)
            .and_then(|chat| chat.jump.as_ref());
        let next_jump = next.and_then(|chat| chat.jump.as_ref());
        let jump_pending = matches!(
            self.chat_history_position.get(),
            Some(HistoryPosition::Jump(_))
        );
        if previous_jump != next_jump || jump_pending {
            let row = next
                .zip(next_jump)
                .and_then(|(chat, jump)| history_message_index(chat, &jump.message_id));
            if let Some(row) = row {
                self.chat_history_position
                    .set(Some(HistoryPosition::Jump(row)));
                self.chat_scroll_anchor = None;
                return;
            }
            if jump_pending {
                self.chat_history_position.set(None);
            }
        }
        if self.chat_history_position.get() == Some(HistoryPosition::Opening) {
            if next.is_some_and(|chat| chat.jump_failed) {
                self.chat_history_position.set(None);
            }
            return;
        }
        if let (Some(previous), Some(next)) = (previous, next) {
            if next.history_ready()
                && let Some((anchor, offset)) = self.chat_scroll_anchor.take()
                && (next.has_more_after || next.paging_forward)
                && let Some(index) = history_message_index(next, &anchor)
            {
                self.chat_history_position
                    .set(Some(HistoryPosition::Anchor(index, offset)));
                return;
            }
            if next.history_ready() && next.paging_forward && previous.loading() {
                return;
            }
            if next.history_ready() && previous.has_more_after && !next.has_more_after {
                self.chat_scroll.scroll_to_bottom();
                return;
            }
            if Arc::ptr_eq(&previous.messages, &next.messages) {
                return;
            }
            if previous.messages.first().map(|message| &message.id)
                != next.messages.first().map(|message| &message.id)
                && let Some((anchor, offset)) = history_anchor(previous, &self.chat_scroll)
                && let Some(index) = history_message_index(next, anchor)
            {
                self.chat_history_position
                    .set(Some(HistoryPosition::Anchor(index, offset)));
            } else if self.chat_scroll.offset().y == -self.chat_scroll.max_offset().y
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
        if !chat.draft.can_send(&body) {
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let keyboard_focus = window
            .last_input_was_keyboard()
            .then(|| window.focused(cx))
            .flatten();
        if self
            .chat_history_keyboard_focus
            .replace(keyboard_focus.clone())
            != keyboard_focus
            && let Some(focus) = keyboard_focus
            && self.chat_history_position.get().is_none()
            && let Some((message_id, _)) = self
                .chat_row_focus
                .borrow()
                .iter()
                .find(|(_, row)| row.contains(&focus, window))
            && let Some(index) = history_message_index(chat, message_id)
        {
            self.chat_scroll.scroll_to_item(index);
        }
        let theme = cx.theme();
        let now = Local::now();
        let divider = |label, color| {
            div()
                .px_4()
                .py_1()
                .flex_shrink_0()
                .child(Separator::horizontal().label(label).color(color))
                .into_any_element()
        };
        let rows =
            history_rows(&chat.messages, chat.first_unread_id.as_deref(), &Local).map(|row| {
                match row {
                    HistoryRow::Date(date) => {
                        divider(date_label(date, now.date_naive()), theme.border)
                    }
                    HistoryRow::Unread => divider("New messages".into(), theme.danger),
                    HistoryRow::Message {
                        message,
                        starts_group,
                    } => self
                        .chat_message(
                            sidebar,
                            chat,
                            message,
                            starts_group,
                            now.timestamp_millis(),
                            window,
                            cx,
                        )
                        .into_any_element(),
                }
            });
        let status = if chat.jump_failed {
            Some("Message unavailable.")
        } else if chat.unavailable() {
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
            .when(!chat.unavailable(), |area| {
                area.child(self.pins(sidebar, chat, cx))
            })
            .when_some(
                chat.action_outcome.as_ref().and_then(|result| result.error),
                |area, error| {
                    area.child(
                        div()
                            .px_4()
                            .py_2()
                            .child(Label::new(error).text_color(theme.danger)),
                    )
                },
            )
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
            .child(chat_history_viewport(
                rows,
                &self.chat_scroll,
                &self.chat_history_position,
                chat.latest_ready(),
            ))
            .when(chat.has_more_after && !chat.loading(), |area| {
                area.child(
                    div().px_4().py_2().flex().gap_2().child(
                        Button::new("newer-messages")
                            .small()
                            .tab_stop(true)
                            .label("Load newer messages")
                            .disabled(!chat.history_ready() || chat.messages.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send(Command::LoadNewerMessages, None, cx)
                            })),
                    ).child(
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
                    .id("chat-composer")
                    .px_4()
                    .py(if self.compact() { px(6.0) } else { px(12.0) })
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .border_1()
                    .border_color(theme.background.opacity(0.0))
                    .when(chat.can_send() && chat.outgoing.is_none(), |composer| {
                        let accent = theme.primary;
                        composer
                            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                                style.border_color(accent)
                            })
                            .on_drop(cx.listener(|app, paths: &ExternalPaths, _, cx| {
                                app.add_attachment_paths(paths.paths().to_vec(), cx);
                            }))
                    })
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
                        let channel_id = chat.channel_id.clone();
                        let (status, retry, dismiss) = match &outgoing.status {
                            Delivery::Sending(_) => ("Sending…".into(), false, false),
                            Delivery::Failed(error) => {
                                (format!("Could not send message. {error}"), true, true)
                            }
                            Delivery::Unconfirmed => (
                                "Delivery unconfirmed. Resending may post a duplicate. Check message history."
                                    .into(),
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
                                        .flex_wrap()
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
                                                        "Edit and resend"
                                                    })
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.send(
                                                            Command::DismissChatSend,
                                                            None,
                                                            cx,
                                                        )
                                                    })),
                                            )
                                        })
                                        .when(matches!(outgoing.status, Delivery::Unconfirmed), |bar| {
                                            bar.child(
                                                Button::new("mark-send-delivered")
                                                    .small()
                                                    .tab_stop(true)
                                                    .label("Already sent")
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.send(Command::MarkChatSendDelivered(channel_id.clone()), None, cx)
                                                    })),
                                            )
                                        }),
                                ),
                        )
                    })
                    .child(self.message_composer(sidebar, chat, cx))
            }))
    }

    fn message_composer(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let surface = theme.popover;
        let disabled = chat.outgoing.is_some() || !chat.can_send();
        let can_send = !disabled && chat.draft.can_send(&self.chat_input.read(cx).value());
        let app = cx.weak_entity();
        div()
            .id("message-composer")
            .flex()
            .flex_col()
            .min_h_0()
            .p_1()
            .rounded_lg()
            .bg(surface)
            .border_1()
            .border_color(surface)
            .child(
                div()
                    .id("composer-media")
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.staged_gif(chat, cx))
                    .child(self.attachment_queue(chat, cx)),
            )
            .child(
                Textarea::new(&self.chat_input)
                    .flex_shrink_0()
                    .bg(surface)
                    .border_color(surface)
                    .aria_label("Write a message")
                    .on_paste(move |item, _, cx| {
                        app.update(cx, |app, cx| app.paste_attachments(item, cx))
                            .unwrap_or(false)
                    })
                    .disabled(disabled),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_end()
                    .gap_2()
                    .child(self.composer_tools(sidebar, chat, disabled, cx))
                    .child(
                        Button::new("send-chat")
                            .primary()
                            .small()
                            .icon(IconName::SendHorizontal)
                            .label("Send")
                            .tab_stop(true)
                            .flex_shrink_0()
                            .disabled(!can_send)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_chat(window, cx)),
                            ),
                    ),
            )
    }

    fn chat_message(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
        message: &ChatMessage,
        starts_group: bool,
        now: i64,
        window: &Window,
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
        let images = self.message_images(chat, &message_id, &rendered, cx);
        let share_text = SharedString::from(rendered);
        let target = MessageTarget::new(&message.channel_id, &message.id);
        let text = TextView::markdown("message-body", share_text.clone())
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
                    search::activate_chat_link(&link_app, url, cx);
                }
            });
        let row_focus = self
            .chat_row_focus
            .borrow_mut()
            .entry(message.id.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        let composer_busy = chat.outgoing.is_some() || !chat.can_send();
        let message_actions = Rc::new(actions::MessageActions {
            app: app.clone(),
            channel: channel.clone(),
            message: message_id.clone(),
            row_focus: row_focus.clone(),
            can_edit: sidebar.profile.id.as_deref() == Some(&message.user_id),
            can_delete: sidebar.can_remove_message_content(message),
            pin_to: chat.can_pin(sidebar.is_admin()).then_some(!message.pinned),
            disabled: chat.action_pending || !chat.can_send(),
            composer_busy,
            body,
            share_text,
            target,
            spoiler_key,
            hidden,
        });
        let reply_channel = channel.clone();
        let reply_id = message_id.clone();
        let keyboard_focused = row_focus.is_focused(window) && window.last_input_was_keyboard();
        let row_focused = row_focus.contains_focused(window, cx);
        let author = sidebar.message_author(&message.user_id);
        let time = DateTime::from_timestamp_millis(message.created_at)
            .map(|time| time.with_timezone(&Local));
        let exact_time = time.map(|time| time.format("%b %-d, %Y, %H:%M:%S").to_string());
        let menu_focused = self
            .message_menu_focus
            .as_ref()
            .is_some_and(|(channel, id, focus)| {
                channel == &chat.channel_id && id == &message.id && focus.is_focused(window)
            });
        let toolbar = message_actions.toolbar(
            self,
            sidebar,
            chat,
            message,
            row_focused || menu_focused,
            cx,
        );
        let content = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(starts_group, |header| {
                        header.child(Label::new(author.to_owned()).font_semibold())
                    })
                    .when_some(
                        exact_time.clone().filter(|_| starts_group),
                        |header, exact| {
                            header.child(
                                div()
                                    .id("sent-at")
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(exact.clone()).build(window, cx)
                                    })
                                    .child(
                                        Label::new(relative_time(message.created_at, now))
                                            .text_xs()
                                            .text_color(theme.muted_foreground),
                                    ),
                            )
                        },
                    )
                    .when(message.pinned, |row| {
                        row.child(
                            Label::new("Pinned")
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                    })
                    .when(message.edited_at.is_some(), |row| {
                        row.child(
                            Label::new("(edited)")
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                    }),
            )
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
            .when(!message.body.is_empty(), |row| row.child(text))
            .child(images)
            .when(!message.attachments.is_empty(), |row| {
                row.child(self.attachment_cards(message, cx))
            })
            .children(self.message_action_form(chat, message, cx))
            .when(!message.previews.is_empty(), |row| {
                row.child(self.link_previews(sidebar, chat, message, cx))
            })
            .when(!message.reactions.is_empty(), |row| {
                row.child(self.reactions(sidebar, chat, message, cx))
            });
        div()
            .id(SharedString::from(format!("message-{}", message.id)))
            .relative()
            .group("message-row")
            .track_focus(&row_focus)
            .when_some(exact_time.clone(), |row, time| {
                row.aria_description(format!("{author}, {time}"))
            })
            .border_l_2()
            .border_color(theme.transparent)
            .focus_visible(|row| row.border_color(theme.ring))
            .on_key_down(cx.listener(move |app, event: &KeyDownEvent, window, cx| {
                if !event.is_held
                    && !event.keystroke.modifiers.modified()
                    && event.keystroke.key == "r"
                    && !window.has_focused_input(cx)
                    && !composer_busy
                {
                    app.reply_to_message(reply_channel.clone(), reply_id.clone(), window, cx);
                    cx.stop_propagation();
                    window.prevent_default();
                }
            }))
            .px_4()
            .py(px(2.))
            .when(starts_group, |row| {
                row.pt(if self.compact() { px(4.) } else { px(8.) })
            })
            .flex()
            .items_start()
            .gap_2()
            .flex_shrink_0()
            .when_some(
                message_row_background(
                    message,
                    chat.direct_message_peer.is_some(),
                    sidebar.profile.id.as_deref(),
                    chat.jump.as_ref().map(|jump| jump.message_id.as_str()),
                    theme,
                ),
                |row, background| row.bg(background),
            )
            .when_some(
                exact_time.clone().filter(|_| keyboard_focused),
                |row, time| {
                    row.child(deferred(
                        div()
                            .absolute()
                            .left_4()
                            .bottom_full()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.tokens.popover)
                            .child(Label::new(time).text_xs()),
                    ))
                },
            )
            .child(
                div()
                    .w(px(36.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .when(starts_group, |lane| {
                        lane.child(profile_avatar(
                            Avatar::new().name(author.to_owned()),
                            sidebar.avatar(Some(&message.user_id)),
                            32.,
                            false,
                            cx,
                        ))
                    })
                    .when_some(
                        time.filter(|_| !starts_group).zip(exact_time.clone()),
                        |lane, (time, exact)| {
                            lane.child(
                                div()
                                    .id("sent-at")
                                    .opacity(if row_focused { 1. } else { 0. })
                                    .group_hover("message-row", |clock| clock.opacity(1.))
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(exact.clone()).build(window, cx)
                                    })
                                    .child(
                                        Label::new(time.format("%H:%M").to_string())
                                            .text_xs()
                                            .text_color(theme.muted_foreground),
                                    ),
                            )
                        },
                    ),
            )
            .child(content)
            .child(toolbar)
            .context_menu(move |menu, window, cx| message_actions.menu(menu, window, cx))
    }

    fn reply_to_message(
        &mut self,
        channel_id: String,
        message_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send(
            Command::SetChatReply {
                channel_id,
                message_id: Some(message_id),
            },
            None,
            cx,
        );
        self.chat_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn composer_tools(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
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
            .flex_1()
            .min_w_0()
            .flex_wrap()
            .gap_1()
            .children(
                [
                    (IconName::Bold, "Bold", BOLD),
                    (IconName::Italic, "Italic", ITALIC),
                    (IconName::Strikethrough, "Strikethrough", STRIKETHROUGH),
                    (IconName::Code, "Inline code", INLINE_CODE),
                    (IconName::SquareCode, "Code block", CODE_BLOCK),
                    (IconName::Quote, "Quote", QUOTE),
                    (IconName::EyeOff, "Spoiler", ("||", "||")),
                ]
                .into_iter()
                .map(|(icon, label, (before, after))| {
                    tool_button(label, icon, label)
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.insert_chat_text(before, after, window, cx);
                        }))
                }),
            )
            .child(
                tool_button("mention-menu", IconName::AtSign, "Mention")
                    .disabled(disabled || people.is_empty())
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
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
                tool_button("emoji-menu", IconName::FaceSlightlySmiling, "Emoji")
                    .disabled(disabled)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
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
            .child(self.attachment_picker(chat, cx))
            .child(self.gif_toggle(chat, cx))
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

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a graphical session or Xvfb"]
    fn delayed_history_opens_at_the_latest_message_and_shares_scrollbar_position() {
        use std::{cell::Cell, rc::Rc, time::Duration};

        use gpui_kit::*;

        use super::{HistoryPosition, chat_history_viewport};

        struct ChatLayout {
            rows: usize,
            height: Pixels,
            scroll: ScrollHandle,
            position: Rc<Cell<Option<HistoryPosition>>>,
        }

        impl Render for ChatLayout {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .h(self.height)
                    .flex()
                    .flex_col()
                    .child(chat_history_viewport(
                        (0..self.rows).map(|_| div().h(px(24.)).flex_shrink_0()),
                        &self.scroll,
                        &self.position,
                        self.rows > 1,
                    ))
            }
        }

        let outcome = Rc::new(Cell::new(None));
        let result = outcome.clone();
        application().run(move |cx| {
            gpui_kit::init(cx);
            let scroll = ScrollHandle::new();
            let handle = cx
                .open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|_| ChatLayout {
                        rows: 1,
                        height: px(120.),
                        scroll: scroll.clone(),
                        position: Rc::new(Cell::new(Some(HistoryPosition::Opening))),
                    })
                })
                .unwrap();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                handle
                    .update(cx, |view, _, cx| {
                        view.rows = 20;
                        cx.notify();
                    })
                    .unwrap();
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let aligned = handle
                    .update(cx, |_, window, cx| {
                        let aligned = (scroll.bottom_item(), scroll.logical_scroll_bottom().1);
                        window.dispatch_event(
                            ScrollEvent {
                                position: scroll.bounds().center(),
                                delta: ScrollDelta::Pixels(point(px(0.), px(20.))),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                        aligned
                    })
                    .unwrap();
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let after_wheel = scroll.offset().y;
                handle
                    .update(cx, |view, _, cx| {
                        view.rows = 21;
                        cx.notify();
                    })
                    .unwrap();
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let after_append = handle
                    .update(cx, |_, window, cx| {
                        let offset = scroll.offset().y;
                        let bounds = scroll.bounds();
                        let thumb = point(bounds.right() - px(8.), bounds.bottom() - px(25.));
                        let top = point(thumb.x, bounds.top());
                        for event in [
                            MouseDownEvent {
                                position: thumb,
                                button: MouseButton::Left,
                                click_count: 1,
                                ..Default::default()
                            }
                            .to_platform_input(),
                            MouseMoveEvent {
                                position: top,
                                pressed_button: Some(MouseButton::Left),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            MouseUpEvent {
                                position: top,
                                button: MouseButton::Left,
                                click_count: 1,
                                ..Default::default()
                            }
                            .to_platform_input(),
                        ] {
                            window.dispatch_event(event, cx);
                        }
                        offset
                    })
                    .unwrap();
                let after_drag = scroll.offset().y;
                handle
                    .update(cx, |view, _, cx| {
                        view.height = px(530.);
                        cx.notify();
                    })
                    .unwrap();
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                handle
                    .update(cx, |_, _, cx| {
                        result.set(Some((
                            aligned,
                            after_wheel,
                            after_append,
                            after_drag,
                            scroll.max_offset().y,
                        )));
                        cx.quit();
                    })
                    .unwrap();
            })
            .detach();
        });
        let (aligned, after_wheel, after_append, after_drag, max_after_resize) =
            outcome.take().expect("history must finish layout");
        assert_eq!(aligned, (19, px(0.)));
        assert_eq!(after_wheel, px(-340.));
        assert_eq!(after_append, after_wheel);
        assert_eq!(after_drag, px(0.));
        assert_eq!(max_after_resize, px(0.));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a graphical session or Xvfb"]
    fn chat_jumps_align_grouped_messages_and_messages_after_both_dividers() {
        use std::{cell::Cell, rc::Rc, time::Duration};

        use chrono::{FixedOffset, TimeZone};
        use gpui_kit::*;

        use super::{
            ChatMessage, HistoryPosition, HistoryRow, chat_history_viewport,
            history::message_row_index, history_rows,
        };

        struct ChatLayout {
            scroll: ScrollHandle,
            position: Rc<Cell<Option<HistoryPosition>>>,
            messages: Vec<ChatMessage>,
            timezone: FixedOffset,
        }

        impl Render for ChatLayout {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let rows = history_rows(&self.messages, Some("5"), &self.timezone).map(|row| {
                    let height = match row {
                        HistoryRow::Date(_) => 16.,
                        HistoryRow::Unread => 20.,
                        HistoryRow::Message { starts_group, .. } => {
                            if starts_group {
                                32.
                            } else {
                                24.
                            }
                        }
                    };
                    div().h(px(height)).flex_shrink_0()
                });
                let history = div()
                    .h(px(120.))
                    .flex()
                    .flex_col()
                    .child(chat_history_viewport(
                        rows,
                        &self.scroll,
                        &self.position,
                        true,
                    ));
                div()
                    .flex()
                    .flex_col()
                    .child(div().h(px(48.)))
                    .child(history)
            }
        }

        let timezone = FixedOffset::east_opt(7200).unwrap();
        let midnight = timezone
            .with_ymd_and_hms(2026, 11, 22, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let messages: Vec<ChatMessage> = (0..20)
            .map(|index| {
                serde_json::from_value(serde_json::json!({
                    "id": index.to_string(),
                    "channelId": "general",
                    "userId": "bob",
                    "body": "Message",
                    "createdAt": midnight + (index - 5) * 60_000
                }))
                .unwrap()
            })
            .collect();
        let rows = || history_rows(&messages, Some("5"), &timezone);
        let after_dividers = message_row_index(rows(), "5").unwrap();
        let grouped = message_row_index(rows(), "6").unwrap();
        assert_eq!((after_dividers, grouped), (8, 9));

        let outcome = Rc::new(Cell::new(None));
        let result = outcome.clone();
        application().run(move |cx| {
            gpui_kit::init(cx);
            let scroll = ScrollHandle::new();
            let handle = cx
                .open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|_| ChatLayout {
                        scroll: scroll.clone(),
                        position: Rc::new(Cell::new(Some(HistoryPosition::Jump(after_dividers)))),
                        messages,
                        timezone,
                    })
                })
                .unwrap();
            cx.spawn(async move |cx| {
                let timer = cx.background_executor().clone();
                timer.timer(Duration::from_millis(250)).await;
                let viewport_top = scroll.bounds().top();
                let after_dividers_top =
                    scroll.bounds_for_item(after_dividers).unwrap().top() + scroll.offset().y;
                handle
                    .update(cx, |view, _, cx| {
                        view.position.set(Some(HistoryPosition::Jump(grouped)));
                        cx.notify();
                    })
                    .unwrap();
                timer.timer(Duration::from_millis(250)).await;
                handle
                    .update(cx, |_, _, cx| {
                        let grouped_top =
                            scroll.bounds_for_item(grouped).unwrap().top() + scroll.offset().y;
                        result.set(Some((viewport_top, after_dividers_top, grouped_top)));
                        cx.quit();
                    })
                    .unwrap();
            })
            .detach();
        });
        let (viewport_top, after_dividers_top, grouped_top) =
            outcome.take().expect("the chat must finish layout");
        assert!(
            viewport_top > px(0.),
            "fixture needs a header above the chat"
        );
        assert_eq!(after_dividers_top, viewport_top);
        assert_eq!(grouped_top, viewport_top);
    }
}
