use super::*;

use gpui_kit::component::menu::PopupMenu;

use crate::{
    chat_markdown::{notification_preview, plain_text},
    session::MessageAction,
};

const QUICK_REACTIONS: [(&str, &str); 3] = [
    ("👍", "thumbs up"),
    ("❤️", "red heart"),
    ("😂", "face with tears of joy"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum PinPanel {
    Latest,
    All,
}

#[derive(Clone, Copy)]
pub(super) enum ActionForm {
    Edit,
    Delete,
}

pub(in crate::ui) struct MessageActionForm {
    message_id: String,
    kind: ActionForm,
    submitted: bool,
}

impl Spk {
    pub(super) fn update_message_actions(
        form_state: &mut Option<MessageActionForm>,
        previous: Option<&ChatView>,
        next: Option<&ChatView>,
    ) {
        if let Some(form) = form_state {
            let Some(next) = next.filter(|next| {
                previous.is_some_and(|previous| previous.channel_id == next.channel_id)
                    && !next.unavailable()
                    && next
                        .messages
                        .iter()
                        .any(|message| message.id == form.message_id)
            }) else {
                *form_state = None;
                return;
            };
            if form.submitted
                && previous.is_some_and(|view| view.action_pending)
                && let Some(result) = &next.action_outcome
                && result.message_id == form.message_id
            {
                if result.error.is_none() {
                    *form_state = None;
                } else {
                    form.submitted = false;
                }
            }
        }
    }

    pub(super) fn begin_message_action(
        &mut self,
        channel_id: &str,
        message_id: &str,
        kind: ActionForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = chat(&self.view).filter(|view| view.channel_id == channel_id) else {
            return;
        };
        let Some(message) = view
            .messages
            .iter()
            .find(|message| message.id == message_id)
        else {
            return;
        };
        let initial = match kind {
            ActionForm::Edit => &message.body,
            _ => "",
        };
        self.message_action_form = Some(MessageActionForm {
            message_id: message_id.to_owned(),
            kind,
            submitted: false,
        });
        if !matches!(kind, ActionForm::Delete) {
            self.message_action_input.update(cx, |input, cx| {
                input.set_value(initial.to_owned(), window, cx);
                input.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(in crate::ui) fn submit_message_action(&mut self, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let (Some(form), Some(view)) = (
            &mut self.message_action_form,
            sidebar
                .chat
                .as_ref()
                .filter(|view| !view.action_pending && view.can_send()),
        ) else {
            return;
        };
        let text = self.message_action_input.read(cx).value();
        let action = match form.kind {
            ActionForm::Edit => MessageAction::Edit(text.to_string()),
            ActionForm::Delete => MessageAction::Delete,
        };
        form.submitted = true;
        let command = Command::MutateChatMessage {
            channel_id: view.channel_id.clone(),
            message_id: form.message_id.clone(),
            action,
        };
        self.send(command, None, cx);
    }

    pub(super) fn message_action_form(
        &self,
        chat: &ChatView,
        message: &ChatMessage,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        let form = self
            .message_action_form
            .as_ref()
            .filter(|form| form.message_id == message.id)?;
        let (title, action) = match form.kind {
            ActionForm::Edit => ("Edit message", "Save"),
            ActionForm::Delete => ("Delete this message?", "Delete"),
        };
        let button = control_button("submit-message-action")
            .small()
            .tab_stop(true)
            .label(action)
            .disabled(chat.action_pending)
            .on_click(cx.listener(|this, _, _, cx| this.submit_message_action(cx)));
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Label::new(title).font_semibold())
                .when(!matches!(form.kind, ActionForm::Delete), |panel| {
                    panel.child(
                        Textarea::new(&self.message_action_input)
                            .aria_label(title)
                            .disabled(chat.action_pending),
                    )
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(if matches!(form.kind, ActionForm::Delete) {
                            button.danger()
                        } else {
                            button.primary()
                        })
                        .child(
                            control_button("cancel-message-action")
                                .small()
                                .tab_stop(true)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.message_action_form = None;
                                    this.chat_input
                                        .update(cx, |input, cx| input.focus(window, cx));
                                    cx.notify();
                                })),
                        ),
                ),
        )
    }

    pub(super) fn toggle_reaction(
        &mut self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
        selection: ReactionSelection,
        cx: &mut Context<Self>,
    ) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let Some((_, message)) = reaction_message(sidebar.chat.as_ref(), channel_id, message_id)
            .filter(|(chat, _)| !chat.action_pending)
        else {
            return;
        };
        let action = reaction_action(message, emoji, selection, sidebar.profile.id.as_deref());
        self.send(
            Command::MutateChatMessage {
                channel_id: channel_id.to_owned(),
                message_id: message_id.to_owned(),
                action,
            },
            None,
            cx,
        );
    }

    pub(super) fn reactions(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
        message: &ChatMessage,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let own_id = sidebar.profile.id.as_deref();
        div()
            .flex()
            .flex_wrap()
            .mt_1()
            .gap_1()
            .children(message.reactions.iter().map(|reaction| {
                let emoji = &reaction.emoji;
                let mine = message.reacted_with(emoji, own_id);
                let names = reaction
                    .user_ids
                    .iter()
                    .map(|id| sidebar.message_author(id))
                    .collect::<Vec<_>>()
                    .join(", ");
                let count = reaction.user_ids.len();
                let action = if mine {
                    "Remove your reaction"
                } else {
                    "React"
                };
                reaction_button(chat, message, emoji, mine, ReactionSelection::Chip, cx)
                    .outline()
                    .xsmall()
                    .rounded(px(10.))
                    .child(
                        control_content()
                            .child(reaction_glyph(emoji, ACTION_ICON_SIZE))
                            .child(count.to_string()),
                    )
                    .tooltip(names)
                    .accessibility_label(format!("{emoji}, {count} reactions. {action}"))
            }))
    }

    pub(super) fn link_previews(
        &self,
        sidebar: &Sidebar,
        chat: &ChatView,
        message: &ChatMessage,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let can_remove = sidebar.can_remove_message_content(message);
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(message.previews.iter().filter_map(|preview| {
                let url = preview.link()?;
                let site = preview
                    .site_name
                    .as_deref()
                    .or(url.host_str())
                    .unwrap_or(&preview.url);
                let channel_id = chat.channel_id.clone();
                let message_id = message.id.clone();
                let image = chat.preview_images.get(&message_id, &preview.url);
                let preview_url = preview.url.clone();
                let app = cx.weak_entity();
                Some(
                    div()
                        .id(SharedString::from(format!("link-preview-{preview_url}")))
                        .max_w(px(480.))
                        .border_l_2()
                        .border_color(theme.primary)
                        .rounded_md()
                        .bg(theme.muted)
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .justify_between()
                                .items_center()
                                .child(
                                    control_button("open-preview")
                                        .small()
                                        .ghost()
                                        .tab_stop(true)
                                        .label(site.to_owned())
                                        .accessibility_label(format!("Open link to {site}"))
                                        .on_click(move |_, _, cx| {
                                            search::activate_chat_link(&app, url.clone(), cx)
                                        }),
                                )
                                .when(can_remove, |row| {
                                    row.child(
                                        control_button("remove-preview")
                                            .small()
                                            .ghost()
                                            .tab_stop(true)
                                            .disabled(chat.action_pending)
                                            .label("Remove preview")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.send(
                                                    Command::MutateChatMessage {
                                                        channel_id: channel_id.clone(),
                                                        message_id: message_id.clone(),
                                                        action: MessageAction::RemovePreview(
                                                            preview_url.clone(),
                                                        ),
                                                    },
                                                    None,
                                                    cx,
                                                )
                                            })),
                                    )
                                }),
                        )
                        .when_some(preview.title.clone(), |card, title| {
                            card.child(Label::new(title).font_semibold())
                        })
                        .when_some(preview.description.clone(), |card, description| {
                            card.child(Label::new(description).text_sm())
                        })
                        .when_some(image, |card, image| {
                            card.child(
                                img(image)
                                    .w_full()
                                    .max_h(px(192.))
                                    .object_fit(ObjectFit::Contain),
                            )
                        }),
                )
            }))
    }

    pub(super) fn close_pins(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let opener = match self.chat_pins.take() {
            Some(PinPanel::Latest) => &self.latest_pin_focus,
            Some(PinPanel::All) => &self.all_pins_focus,
            None => return false,
        };
        opener.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn pins(
        &self,
        sidebar: &Sidebar,
        view: &ChatView,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let bar = div()
            .id("chat-pins")
            .track_focus(&self.chat_pins_focus)
            .min_w_0()
            .flex_shrink_0();
        let pins = match &view.pins {
            Some(Pins::Failed) => {
                let channel_id = view.channel_id.clone();
                return bar.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .child(
                            Label::new("Could not load Pins.").text_color(theme.muted_foreground),
                        )
                        .child(
                            control_button("retry-pins")
                                .small()
                                .tab_stop(true)
                                .label("Retry")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(Command::RetryChatPins(channel_id.clone()), None, cx);
                                })),
                        ),
                );
            }
            Some(Pins::Ready(pins)) if !pins.is_empty() => pins,
            _ => return bar,
        };
        let latest = &pins[0];
        let author = sidebar.message_author(&latest.user_id);
        let snippet = pin_snippet(latest, sidebar);
        let latest_open = self.chat_pins == Some(PinPanel::Latest);
        let all_open = self.chat_pins == Some(PinPanel::All);
        let visible = if all_open {
            pins.as_slice()
        } else {
            &pins[..1]
        };
        let can_pin = view.can_pin(sidebar.is_admin());
        bar.border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_1()
                    .child(
                        control_button("latest-pin")
                            .small()
                            .ghost()
                            .selected(latest_open)
                            .toggled(latest_open)
                            .track_focus(&self.latest_pin_focus)
                            .tab_stop(true)
                            .flex_1()
                            .min_w_0()
                            .accessibility_label(format!(
                                "{} latest pinned message from {author}: {snippet}",
                                if latest_open { "Hide" } else { "Show" }
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap(CONTROL_PADDING)
                                    .child(
                                        Icon::new(IconName::Pin)
                                            .size(ACTION_ICON_SIZE)
                                            .flex_shrink_0()
                                            .text_color(theme.primary),
                                    )
                                    .child(Label::new(format!("{author} · {snippet}")).truncate()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.latest_pin_focus.focus(window, cx);
                                this.chat_pins = (!latest_open).then_some(PinPanel::Latest);
                                cx.notify();
                            })),
                    )
                    .when(pins.len() > 1, |row| {
                        row.child(
                            control_button("all-pins")
                                .small()
                                .ghost()
                                .selected(all_open)
                                .toggled(all_open)
                                .track_focus(&self.all_pins_focus)
                                .tab_stop(true)
                                .flex_shrink_0()
                                .label(format!("+{}", pins.len() - 1))
                                .accessibility_label(if all_open {
                                    "Close pinned messages".to_owned()
                                } else {
                                    format!("Show all {} pinned messages", pins.len())
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.all_pins_focus.focus(window, cx);
                                    this.chat_pins = (!all_open).then_some(PinPanel::All);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(self.chat_pins.is_some(), |bar| {
                bar.on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if event.button == MouseButton::Left {
                        this.chat_pins = None;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .id("pins-list")
                        .max_h(px(144.))
                        .overflow_y_scroll()
                        .children(visible.iter().map(|message| {
                            let channel_id = view.channel_id.clone();
                            let message_id = message.id.clone();
                            let unpin_channel = channel_id.clone();
                            let unpin_id = message_id.clone();
                            let preview = if all_open {
                                let author = sidebar.message_author(&message.user_id);
                                let snippet = pin_snippet(message, sidebar);
                                Label::new(format!("{author} · {snippet}"))
                                    .truncate()
                                    .into_any_element()
                            } else {
                                let body = plain_text(&message_markdown(
                                    &message.body,
                                    |id| sidebar.message_author(id),
                                    |_| false,
                                    None,
                                ));
                                let body = if body.trim().is_empty() {
                                    pin_snippet(message, sidebar)
                                } else {
                                    body
                                };
                                div()
                                    .id("preview")
                                    .max_h(px(72.))
                                    .overflow_y_scroll()
                                    .child(Label::new(body))
                                    .into_any_element()
                            };
                            div()
                                .id(SharedString::from(message.id.clone()))
                                .px_4()
                                .py_2()
                                .border_t_1()
                                .border_color(theme.border)
                                .child(preview)
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            control_button("jump-to-pin")
                                                .small()
                                                .tab_stop(true)
                                                .label("Go to message")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.close_pins(window, cx);
                                                        this.send(
                                                            Command::JumpToChatMessage {
                                                                channel_id: channel_id.clone(),
                                                                message_id: message_id.clone(),
                                                            },
                                                            None,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        )
                                        .when(can_pin, |row| {
                                            row.child(
                                                control_button("unpin")
                                                    .small()
                                                    .ghost()
                                                    .tab_stop(true)
                                                    .label("Unpin")
                                                    .disabled(view.action_pending)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.send(
                                                                Command::MutateChatMessage {
                                                                    channel_id: unpin_channel
                                                                        .clone(),
                                                                    message_id: unpin_id.clone(),
                                                                    action: MessageAction::Pin(
                                                                        false,
                                                                    ),
                                                                },
                                                                None,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        }),
                                )
                        })),
                )
            })
    }
}

#[derive(Clone, Copy)]
pub(super) enum ReactionSelection {
    Choice,
    Chip,
}

fn reaction_choice_key<'a>(message: &'a ChatMessage, emoji: &'a str) -> &'a str {
    const EMOJI_PRESENTATION: char = '\u{fe0f}';
    let base = emoji.trim_end_matches(EMOJI_PRESENTATION);
    let Some((canonical, _)) = QUICK_REACTIONS
        .into_iter()
        .find(|(key, _)| key.trim_end_matches(EMOJI_PRESENTATION) == base)
    else {
        return emoji;
    };
    // SPK stores exact string keys. Use the existing variant if there is only one.
    let mut existing = message
        .reactions
        .iter()
        .map(|reaction| reaction.emoji.as_str())
        .filter(|key| key.trim_end_matches(EMOJI_PRESENTATION) == base);
    match (existing.next(), existing.next()) {
        (Some(key), None) => key,
        _ => canonical,
    }
}

fn reaction_action(
    message: &ChatMessage,
    emoji: &str,
    selection: ReactionSelection,
    own_id: Option<&str>,
) -> MessageAction {
    let emoji = match selection {
        ReactionSelection::Choice => reaction_choice_key(message, emoji),
        ReactionSelection::Chip => emoji,
    };
    MessageAction::React {
        emoji: emoji.to_owned(),
        on: !message.reacted_with(emoji, own_id),
    }
}

fn reaction_button(
    chat: &ChatView,
    message: &ChatMessage,
    emoji: &str,
    mine: bool,
    selection: ReactionSelection,
    cx: &Context<Spk>,
) -> Button {
    let channel_id = chat.channel_id.clone();
    let message_id = message.id.clone();
    let emoji = emoji.to_owned();
    control_button(SharedString::from(format!("reaction-{emoji}")))
        .tab_stop(true)
        .selected(mine)
        .toggled(mine)
        .disabled(chat.action_pending || !chat.can_send())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_reaction(&channel_id, &message_id, &emoji, selection, cx);
        }))
}

fn pin_snippet(message: &ChatMessage, sidebar: &Sidebar) -> String {
    notification_preview(&message.body, !message.attachments.is_empty(), |id| {
        sidebar.message_author(id)
    })
}

pub(super) struct MessageActions {
    pub app: WeakEntity<Spk>,
    pub channel: String,
    pub message: String,
    pub row_focus: FocusHandle,
    pub can_edit: bool,
    pub can_delete: bool,
    pub pin_to: Option<bool>,
    pub disabled: bool,
    pub composer_busy: bool,
    pub body: String,
    pub share_text: SharedString,
    pub target: Option<MessageTarget>,
    pub spoiler_key: (String, u64),
    pub hidden: Vec<usize>,
}

impl MessageActions {
    pub fn toolbar(
        self: &Rc<Self>,
        app: &Spk,
        sidebar: &Sidebar,
        chat: &ChatView,
        message: &ChatMessage,
        visible: bool,
        cx: &Context<Spk>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let menu = self.clone();
        let picker_open = app
            .reaction_picker
            .as_ref()
            .is_some_and(|picker| picker.belongs_to(&self.channel, &self.message));
        div()
            .id("message-toolbar")
            .role(Role::Toolbar)
            .aria_label("Message actions")
            .absolute()
            .top_0()
            .right_2()
            .max_w_full()
            .flex()
            .flex_wrap()
            .justify_end()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .shadow_sm()
            .when(!visible && !picker_open, |toolbar| {
                toolbar
                    .invisible()
                    .group_hover("message-row", |style| style.visible())
            })
            .children(QUICK_REACTIONS.into_iter().map(|(emoji, name)| {
                let key = reaction_choice_key(message, emoji);
                let own = message.reacted_with(key, sidebar.profile.id.as_deref());
                let label = format!("{} {name} reaction", if own { "Remove" } else { "Add" });
                reaction_button(chat, message, emoji, own, ReactionSelection::Choice, cx)
                    .xsmall()
                    .w(CONTROL_HEIGHT)
                    .h(px(28.0))
                    .px(px(8.0))
                    .py(CONTROL_PADDING)
                    .self_center()
                    .ghost()
                    .child(reaction_glyph(emoji, px(16.0)))
                    .accessibility_label(label.clone())
                    .tooltip(label)
            }))
            .child(
                div()
                    .relative()
                    .child(
                        tool_button(
                            "add-reaction",
                            IconName::FaceSlightlySmilingPlus,
                            "Add reaction",
                        )
                        .open(picker_open)
                        .disabled(self.disabled)
                        .on_click(self.open_reaction_picker()),
                    )
                    .when(picker_open, |anchor| {
                        anchor.children(app.reaction_picker_anchor(cx))
                    }),
            )
            .child(
                tool_button("message-reply", IconName::Reply, "Reply")
                    .disabled(self.composer_busy)
                    .on_click(self.reply()),
            )
            .when(self.can_edit, |toolbar| {
                toolbar.child(
                    tool_button("message-edit", IconName::Pencil, "Edit message")
                        .disabled(self.disabled)
                        .on_click(self.form_action(ActionForm::Edit)),
                )
            })
            .child(
                tool_button("copy-markdown", IconName::Copy, "Copy Markdown")
                    .on_click(self.copy_markdown()),
            )
            .child(
                tool_button("more-message-actions", IconName::Ellipsis, "More actions")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |popup, window, cx| {
                        menu.menu(popup, window, cx)
                    }),
            )
    }

    pub fn menu(
        &self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let menu_focus = menu.focus_handle(cx);
        let _ = self.app.update(cx, |this, cx| {
            this.message_menu_focus =
                Some((self.channel.clone(), self.message.clone(), menu_focus));
            cx.notify();
        });
        let (app, channel, message) =
            (self.app.clone(), self.channel.clone(), self.message.clone());
        let quote_app = app.clone();
        let quote_channel = channel.clone();
        let quoted = self
            .body
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let copy_text = self.share_text.clone();
        let spoiler_key = self.spoiler_key.clone();
        let hidden = self.hidden.clone();
        let menu = menu
            .max_h(window.viewport_size().height - px(24.))
            .scrollable(true)
            .item(
                PopupMenuItem::new("Reply")
                    .icon(IconName::Reply)
                    .disabled(self.composer_busy)
                    .on_click(self.reply()),
            )
            .item(
                PopupMenuItem::new("Quote")
                    .icon(IconName::Quote)
                    .disabled(self.composer_busy)
                    .on_click(move |_, window, cx| {
                        let _ = quote_app.update(cx, |this, cx| {
                            if chat(&this.view).is_some_and(|chat| {
                                chat.channel_id == quote_channel
                                    && chat.can_send()
                                    && chat.outgoing.is_none()
                            }) {
                                this.chat_input.update(cx, |input, cx| {
                                    let end = input.value().len();
                                    input.set_selected_range(end..end, cx);
                                    input.insert(format!("\n\n{quoted}\n\n"), window, cx);
                                    input.focus(window, cx);
                                });
                                this.update_chat_draft(cx);
                            }
                        });
                    }),
            );
        let mark_app = app.clone();
        self.mutation_items(menu, cx.theme().danger)
            .separator()
            .item(
                PopupMenuItem::new("Mark as unread")
                    .icon(IconName::Mail)
                    .on_click(move |_, _, cx| {
                        let _ = mark_app.update(cx, |this, cx| {
                            this.send(
                                Command::MarkChatUnread {
                                    channel_id: channel.clone(),
                                    message_id: message.clone(),
                                },
                                None,
                                cx,
                            );
                        });
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new("Copy text")
                    .icon(IconName::Copy)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(plain_text(&copy_text)));
                    }),
            )
            .item(
                PopupMenuItem::new("Copy Markdown")
                    .icon(IconName::FileCode)
                    .on_click(self.copy_markdown()),
            )
            .when_some(self.target.clone(), |menu, target| {
                menu.item(
                    PopupMenuItem::new("Copy link")
                        .icon(IconName::Link)
                        .on_click(move |_, _, cx| {
                            search::copy_message_link(&target, cx);
                        }),
                )
            })
            .when(!hidden.is_empty(), |menu| {
                menu.item(
                    PopupMenuItem::new("Reveal spoilers")
                        .icon(IconName::Eye)
                        .on_click(move |_, _, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.preserve_chat_position();
                                this.chat_spoilers
                                    .entry(spoiler_key.clone())
                                    .or_default()
                                    .extend(&hidden);
                                cx.notify();
                            });
                        }),
                )
            })
    }

    fn copy_markdown(&self) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
        let body = self.body.clone();
        move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(body.clone()))
    }

    fn reply(&self) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
        let (app, channel, message) =
            (self.app.clone(), self.channel.clone(), self.message.clone());
        move |_, window, cx| {
            let _ = app.update(cx, |this, cx| {
                this.reply_to_message(channel.clone(), message.clone(), window, cx)
            });
        }
    }

    fn mutation_items(&self, menu: PopupMenu, danger: Hsla) -> PopupMenu {
        let menu = menu.item(
            PopupMenuItem::new("Add reaction…")
                .icon(IconName::FaceSlightlySmilingPlus)
                .disabled(self.disabled)
                .on_click(self.open_reaction_picker()),
        );
        let menu = if self.can_edit {
            menu.item(
                self.form_item("Edit message", ActionForm::Edit)
                    .icon(IconName::Pencil),
            )
        } else {
            menu
        };
        let menu = if self.can_delete {
            menu.item(
                self.form_item("Delete message…", ActionForm::Delete)
                    .icon(Icon::new(IconName::Trash).text_color(danger)),
            )
        } else {
            menu
        };
        if let Some(on) = self.pin_to {
            let (app, channel_id, message_id) =
                (self.app.clone(), self.channel.clone(), self.message.clone());
            menu.item(
                PopupMenuItem::new(if on { "Pin message" } else { "Unpin message" })
                    .icon(if on { IconName::Pin } else { IconName::PinOff })
                    .disabled(self.disabled)
                    .on_click(move |_, _, cx| {
                        let _ = app.update(cx, |this, cx| {
                            this.send(
                                Command::MutateChatMessage {
                                    channel_id: channel_id.clone(),
                                    message_id: message_id.clone(),
                                    action: MessageAction::Pin(on),
                                },
                                None,
                                cx,
                            );
                        });
                    }),
            )
        } else {
            menu
        }
    }

    fn form_item(&self, label: &'static str, kind: ActionForm) -> PopupMenuItem {
        PopupMenuItem::new(label)
            .disabled(self.disabled)
            .on_click(self.form_action(kind))
    }

    fn open_reaction_picker(&self) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
        let (app, channel, message, focus) = (
            self.app.clone(),
            self.channel.clone(),
            self.message.clone(),
            self.row_focus.clone(),
        );
        move |_, window, cx| {
            let (app, channel, message, focus) =
                (app.clone(), channel.clone(), message.clone(), focus.clone());
            // An invoking menu needs to release its focus before handing it to the picker.
            window.defer(cx, move |window, cx| {
                let _ = app.update(cx, |this, cx| {
                    this.open_reaction_picker(&channel, &message, focus, window, cx);
                });
            });
        }
    }

    fn form_action(&self, kind: ActionForm) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
        let (app, channel, message) =
            (self.app.clone(), self.channel.clone(), self.message.clone());
        move |_, window, cx| {
            let _ = app.update(cx, |this, cx| {
                this.begin_message_action(&channel, &message, kind, window, cx)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatMessage, MessageAction, ReactionSelection, reaction_action};

    #[test]
    fn quick_and_picker_reactions_share_keys_and_chips_keep_legacy_targets() {
        const PICKER_THUMBS_UP: &str = "👍\u{fe0f}";
        let mut message: ChatMessage = serde_json::from_value(serde_json::json!({
            "id":"m1","channelId":"general","userId":"bob","body":"","createdAt":1,
            "reactions":[{"emoji":PICKER_THUMBS_UP,"userIds":["alice"]},{"emoji":"❤","userIds":["bob"]}]
        }))
        .unwrap();
        let toggle = |message: &ChatMessage, emoji, selection| {
            let MessageAction::React { emoji, on } =
                reaction_action(message, emoji, selection, Some("alice"))
            else {
                panic!("expected reaction mutation");
            };
            (emoji, on)
        };

        for emoji in ["👍", PICKER_THUMBS_UP] {
            assert_eq!(
                toggle(&message, emoji, ReactionSelection::Choice),
                (PICKER_THUMBS_UP.into(), false)
            );
        }
        assert_eq!(
            toggle(&message, "❤️", ReactionSelection::Choice),
            ("❤".into(), true)
        );

        message.reactions.push(
            serde_json::from_value(serde_json::json!({
                "emoji":"👍","userIds":["bob"]
            }))
            .unwrap(),
        );
        assert_eq!(
            toggle(&message, PICKER_THUMBS_UP, ReactionSelection::Choice),
            ("👍".into(), true)
        );
        assert_eq!(
            toggle(&message, PICKER_THUMBS_UP, ReactionSelection::Chip),
            (PICKER_THUMBS_UP.into(), false)
        );
        assert_eq!(
            toggle(&message, "👍", ReactionSelection::Chip),
            ("👍".into(), true)
        );

        message.reactions.clear();
        assert_eq!(
            toggle(&message, PICKER_THUMBS_UP, ReactionSelection::Choice),
            ("👍".into(), true)
        );
    }
}
