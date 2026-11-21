use gpui_kit::component::Disableable;

use super::*;
use crate::session::{ChannelAction, ChannelKind};

pub(super) enum ChannelForm {
    Create(ChannelKind),
    Rename(String),
    Delete(String, String, ChannelKind),
}

impl Settings {
    pub(super) fn sync_channel_form(&mut self, cx: &App) {
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return;
        };
        if !sidebar.is_admin()
            || self.channel_submitted_at.is_some_and(|revision| {
                revision != sidebar.channel_admin.revision
                    && sidebar
                        .channel_admin
                        .result
                        .is_some_and(|result| result.is_ok())
            })
        {
            self.channel_form = None;
            self.channel_submitted_at = None;
        }
    }

    fn edit_channel(
        &mut self,
        form: ChannelForm,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.channel_submitted_at = None;
        let edit_name = !matches!(form, ChannelForm::Delete(..));
        self.channel_form = Some(form);
        if edit_name {
            self.channel_name.update(cx, |input, cx| {
                input.set_value(initial, window, cx);
                input.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn submit_channel_form(&mut self, confirm_delete: bool, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return;
        };
        if !sidebar.is_admin() || sidebar.channel_admin.pending {
            return;
        }
        let name = || self.channel_name.read(cx).value().to_string();
        let action = match &self.channel_form {
            Some(ChannelForm::Create(kind)) => ChannelAction::Create(*kind, name()),
            Some(ChannelForm::Rename(id)) => ChannelAction::Rename(id.clone(), name()),
            Some(ChannelForm::Delete(id, _, _)) if confirm_delete => {
                ChannelAction::Delete(id.clone())
            }
            _ => return,
        };
        self.channel_submitted_at = Some(sidebar.channel_admin.revision);
        self.change_channel(action, cx);
    }

    fn change_channel(&self, action: ChannelAction, cx: &mut Context<Self>) {
        self.spk.update(cx, |app, cx| {
            app.send(Command::ManageChannel(action), None, cx)
        });
    }

    pub(super) fn channel_status(&self, sidebar: &Sidebar, cx: &App) -> Option<impl IntoElement> {
        let theme = cx.theme();
        let status = self
            .spk
            .read(cx)
            .form_error
            .map(Err)
            .or(sidebar.channel_admin.result);
        status.map(|status| {
            let (message, color) = match status {
                Ok(message) => (message, theme.success),
                Err(message) => (message, theme.danger),
            };
            div()
                .id("channel-admin-status")
                .role(Role::Status)
                .child(Label::new(message).text_color(color))
        })
    }

    pub(super) fn channels(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let pending = sidebar.channel_admin.pending;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .when(self.channel_form.is_none(), |body| {
                body.children(self.channel_status(sidebar, cx))
            })
            .children(
                [ChannelKind::Text, ChannelKind::Voice]
                    .into_iter()
                    .map(|kind| {
                        let (title, create) = match kind {
                            ChannelKind::Text => ("Text Channels", "New text channel"),
                            ChannelKind::Voice => ("Voice Channels", "New voice channel"),
                        };
                        let editing = self.channel_form.is_some();
                        let channels = sidebar.channels_of_kind(kind);
                        let count = channels.clone().count();
                        card(
                            title,
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(
                                    Button::new(create)
                                        .outline()
                                        .small()
                                        .tab_stop(true)
                                        .label(create)
                                        .disabled(pending || editing)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.edit_channel(
                                                ChannelForm::Create(kind),
                                                String::new(),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                                .children(channels.enumerate().map(|(index, channel)| {
                                    let id = channel.id.clone();
                                    let name = channel
                                        .name
                                        .clone()
                                        .unwrap_or_else(|| "Unnamed channel".into());
                                    div()
                                        .id(SharedString::from(id.clone()))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .child(Label::new(name.clone()).truncate()),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_1()
                                                .flex_shrink_0()
                                                .child(
                                                    Button::new("rename-channel")
                                                        .small()
                                                        .ghost()
                                                        .tab_stop(true)
                                                        .label("Rename")
                                                        .accessibility_label(format!(
                                                            "Rename {name}"
                                                        ))
                                                        .disabled(pending || editing)
                                                        .on_click(cx.listener({
                                                            let id = id.clone();
                                                            let name = name.clone();
                                                            move |this, _, window, cx| {
                                                                this.edit_channel(
                                                                    ChannelForm::Rename(id.clone()),
                                                                    name.clone(),
                                                                    window,
                                                                    cx,
                                                                )
                                                            }
                                                        })),
                                                )
                                                .children(
                                                    [
                                                        (false, "Move up", index == 0),
                                                        (true, "Move down", index + 1 == count),
                                                    ]
                                                    .into_iter()
                                                    .map(|(down, label, edge)| {
                                                        let id = id.clone();
                                                        Button::new(label)
                                                            .small()
                                                            .ghost()
                                                            .tab_stop(true)
                                                            .label(label)
                                                            .accessibility_label(format!(
                                                                "{label} {name}"
                                                            ))
                                                            .disabled(pending || editing || edge)
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.change_channel(
                                                                        ChannelAction::Move {
                                                                            id: id.clone(),
                                                                            down,
                                                                        },
                                                                        cx,
                                                                    );
                                                                },
                                                            ))
                                                    }),
                                                )
                                                .child(
                                                    Button::new("delete-channel")
                                                        .small()
                                                        .ghost()
                                                        .tab_stop(true)
                                                        .label("Delete")
                                                        .accessibility_label(format!(
                                                            "Delete {name}"
                                                        ))
                                                        .disabled(pending || editing)
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.edit_channel(
                                                                    ChannelForm::Delete(
                                                                        id.clone(),
                                                                        name.clone(),
                                                                        kind,
                                                                    ),
                                                                    String::new(),
                                                                    window,
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                ),
                                        )
                                })),
                            cx,
                        )
                    }),
            )
    }

    pub(super) fn channel_editor(
        &self,
        form: &ChannelForm,
        pending: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (title, submit) = match form {
            ChannelForm::Create(ChannelKind::Text) => ("New text channel", "Create"),
            ChannelForm::Create(ChannelKind::Voice) => ("New voice channel", "Create"),
            ChannelForm::Rename(_) => ("Rename channel", "Save"),
            ChannelForm::Delete(..) => ("Delete channel", "Delete"),
        };
        let delete = matches!(form, ChannelForm::Delete(..));
        let body = match form {
            ChannelForm::Delete(_, name, kind) => {
                let mut message =
                    format!("Delete {name}? Its messages and files will be removed for everyone.");
                if *kind == ChannelKind::Voice {
                    message.push_str(" Anyone in it will be disconnected.");
                }
                Label::new(message).into_any_element()
            }
            _ => Input::new(&self.channel_name)
                .aria_label("Channel name")
                .w_full()
                .disabled(pending)
                .into_any_element(),
        };
        card(
            title,
            div().flex().flex_col().gap_3().child(body).child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-channel-edit")
                            .small()
                            .ghost()
                            .tab_stop(true)
                            .label("Cancel")
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.channel_form = None;
                                this.channel_submitted_at = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("submit-channel-edit")
                            .small()
                            .primary()
                            .tab_stop(true)
                            .when(delete, |button| button.danger())
                            .label(if pending { "Saving…" } else { submit })
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.submit_channel_form(delete, cx)
                            })),
                    ),
            ),
            cx,
        )
    }
}
