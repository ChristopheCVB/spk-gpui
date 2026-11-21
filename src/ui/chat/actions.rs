use super::*;

use gpui_kit::component::menu::PopupMenu;

use crate::session::{MessageAction, Pins};

#[derive(Clone, Copy)]
pub(super) enum ActionForm {
    Edit,
    Delete,
    React,
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

    fn begin_message_action(
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
        let (Some(form), Some(view)) = (
            &mut self.message_action_form,
            chat(&self.view).filter(|view| !view.action_pending),
        ) else {
            return;
        };
        let text = self.message_action_input.read(cx).value();
        let action = match form.kind {
            ActionForm::Edit => MessageAction::Edit(text.to_string()),
            ActionForm::Delete => MessageAction::Delete,
            ActionForm::React => {
                let emoji = text.trim();
                if emoji.is_empty() {
                    return;
                }
                let own_id = match &self.view {
                    View::SignedIn(sidebar) => sidebar.profile.id.as_deref(),
                    _ => None,
                };
                let already_reacted = view
                    .messages
                    .iter()
                    .find(|message| message.id == form.message_id)
                    .and_then(|message| {
                        message
                            .reactions
                            .iter()
                            .find(|reaction| reaction.emoji == emoji)
                    })
                    .is_some_and(|reaction| {
                        reaction
                            .user_ids
                            .iter()
                            .any(|id| own_id == Some(id.as_str()))
                    });
                MessageAction::React {
                    emoji: emoji.to_owned(),
                    on: !already_reacted,
                }
            }
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
            ActionForm::React => ("Enter an emoji", "Toggle reaction"),
        };
        let button = Button::new("submit-message-action")
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
                            .size_full()
                            .accessibility_id(title)
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
                            Button::new("cancel-message-action")
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
            .gap_1()
            .children(message.reactions.iter().map(|reaction| {
                let mine = reaction
                    .user_ids
                    .iter()
                    .any(|id| own_id == Some(id.as_str()));
                let names = reaction
                    .user_ids
                    .iter()
                    .map(|id| sidebar.message_author(id))
                    .collect::<Vec<_>>()
                    .join(", ");
                let channel_id = chat.channel_id.clone();
                let message_id = message.id.clone();
                let emoji = reaction.emoji.clone();
                Button::new(SharedString::from(format!("reaction-{emoji}")))
                    .small()
                    .tab_stop(true)
                    .outline()
                    .selected(mine)
                    .disabled(chat.action_pending)
                    .label(format!("{emoji} {}", reaction.user_ids.len()))
                    .tooltip(names.clone())
                    .accessibility_label(format!("{emoji} reactions from {names}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(
                            Command::MutateChatMessage {
                                channel_id: channel_id.clone(),
                                message_id: message_id.clone(),
                                action: MessageAction::React {
                                    emoji: emoji.clone(),
                                    on: !mine,
                                },
                            },
                            None,
                            cx,
                        );
                    }))
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
                                    Button::new("open-preview")
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
                                        Button::new("remove-preview")
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

    pub(super) fn pins(
        &self,
        sidebar: &Sidebar,
        view: &ChatView,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let channel_id = view.channel_id.clone();
        let shown = view.pins.is_some();
        let can_pin = view.can_pin(sidebar.is_admin());
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().flex().child(
                    Button::new("show-pins")
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .label(if shown { "Close Pins" } else { "Pins" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(
                                Command::ShowChatPins {
                                    channel_id: channel_id.clone(),
                                    shown: !shown,
                                },
                                None,
                                cx,
                            );
                        })),
                ),
            )
            .when_some(view.pins.as_ref(), |area, pins| {
                area.child(match pins {
                    Pins::Loading => Label::new("Loading Pins…").into_any_element(),
                    Pins::Failed => {
                        let channel_id = view.channel_id.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Label::new("Could not load Pins."))
                            .child(
                                Button::new("retry-pins")
                                    .small()
                                    .tab_stop(true)
                                    .label("Retry")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send(
                                            Command::ShowChatPins {
                                                channel_id: channel_id.clone(),
                                                shown: true,
                                            },
                                            None,
                                            cx,
                                        );
                                    })),
                            )
                            .into_any_element()
                    }
                    Pins::Ready(pins) if pins.is_empty() => {
                        Label::new("No pinned messages.").into_any_element()
                    }
                    Pins::Ready(pins) => div()
                        .id("pins-list")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .children(pins.iter().map(|message| {
                            let (channel_id, message_id) =
                                (view.channel_id.clone(), message.id.clone());
                            let (unpin_channel, unpin_id) =
                                (channel_id.clone(), message_id.clone());
                            let preview = message_markdown(
                                &message.body,
                                |id| sidebar.message_author(id),
                                |_| false,
                                None,
                            );
                            let preview: String = preview.chars().take(180).collect();
                            div()
                                .id(SharedString::from(message.id.clone()))
                                .py_1()
                                .border_t_1()
                                .border_color(theme.border)
                                .child(
                                    Label::new(sidebar.message_author(&message.user_id).to_owned())
                                        .font_semibold(),
                                )
                                .child(Label::new(preview))
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new("jump-to-pin")
                                                .small()
                                                .tab_stop(true)
                                                .label("Go to message")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.send(
                                                        Command::ShowChatPins {
                                                            channel_id: channel_id.clone(),
                                                            shown: false,
                                                        },
                                                        None,
                                                        cx,
                                                    );
                                                    this.send(
                                                        Command::JumpToChatMessage {
                                                            channel_id: channel_id.clone(),
                                                            message_id: message_id.clone(),
                                                        },
                                                        None,
                                                        cx,
                                                    );
                                                })),
                                        )
                                        .when(can_pin, |row| {
                                            row.child(
                                                Button::new("unpin")
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
                        }))
                        .into_any_element(),
                })
            })
    }
}

pub(super) struct MessageActions {
    pub app: WeakEntity<Spk>,
    pub channel: String,
    pub message: String,
    pub can_edit: bool,
    pub can_delete: bool,
    pub pin_to: Option<bool>,
    pub disabled: bool,
}

impl MessageActions {
    pub fn menu(&self, menu: PopupMenu) -> PopupMenu {
        let menu = menu.item(self.form_item("Add reaction…", ActionForm::React));
        let menu = if self.can_edit {
            menu.item(self.form_item("Edit message", ActionForm::Edit))
        } else {
            menu
        };
        let menu = if self.can_delete {
            menu.item(self.form_item("Delete message…", ActionForm::Delete))
        } else {
            menu
        };
        if let Some(on) = self.pin_to {
            let (app, channel_id, message_id) =
                (self.app.clone(), self.channel.clone(), self.message.clone());
            menu.item(
                PopupMenuItem::new(if on { "Pin message" } else { "Unpin message" })
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
        let (app, channel_id, message_id) =
            (self.app.clone(), self.channel.clone(), self.message.clone());
        PopupMenuItem::new(label)
            .disabled(self.disabled)
            .on_click(move |_, window, cx| {
                let _ = app.update(cx, |this, cx| {
                    this.begin_message_action(&channel_id, &message_id, kind, window, cx)
                });
            })
    }
}
