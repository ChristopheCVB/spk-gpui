use std::cell::Cell;

use super::*;
use crate::session::ChannelAction;

#[derive(Clone)]
enum ChannelForm {
    Create(ChannelKind),
    Rename(String, String),
    Delete(String, String, ChannelKind),
}

impl ChannelForm {
    fn create_title(kind: ChannelKind) -> &'static str {
        match kind {
            ChannelKind::Text => "New text channel",
            ChannelKind::Voice => "New voice channel",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Create(kind) => Self::create_title(*kind),
            Self::Rename(..) => "Rename channel",
            Self::Delete(..) => "Delete channel",
        }
    }
}

impl Spk {
    pub(super) fn channel_section_title(
        &self,
        sidebar: &Sidebar,
        kind: ChannelKind,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (title, icon, focus) = match kind {
            ChannelKind::Text => (
                "Text channels",
                IconName::Hash,
                &self.new_text_channel_focus,
            ),
            ChannelKind::Voice => (
                "Voice channels",
                IconName::Volume2,
                &self.new_voice_channel_focus,
            ),
        };
        let focus = focus.clone();
        let add = ChannelForm::create_title(kind);
        div()
            .id(title)
            .flex()
            .items_center()
            .justify_between()
            .child(section_title(title, icon, cx))
            .when(sidebar.is_admin(), |heading| {
                heading.child(
                    tool_button(add, IconName::Plus, add)
                        .track_focus(&focus)
                        .disabled(sidebar.channel_admin.pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            focus.focus(window, cx);
                            this.edit_channel(ChannelForm::Create(kind), window, cx);
                        })),
                )
            })
    }

    pub(super) fn channel_actions(
        &self,
        channel: &Channel,
        kind: ChannelKind,
        can_move_up: bool,
        can_move_down: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        ChannelActions {
            id: channel.id.clone(),
            name: channel_name(channel).to_owned(),
            kind,
            can_move_up,
            can_move_down,
            app: cx.weak_entity(),
        }
    }

    fn edit_channel(&mut self, form: ChannelForm, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(&self.view, View::SignedIn(sidebar)
            if sidebar.is_admin() && !sidebar.channel_admin.pending)
        {
            return;
        }

        let app = cx.entity();
        let opener = window.focused(cx);
        let title = form.title();
        let initial = match &form {
            ChannelForm::Rename(_, name) => name.clone(),
            _ => String::new(),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Channel name")
                .default_value(initial)
        });
        let initial_focus = Cell::new(!matches!(form, ChannelForm::Delete(..)));
        let input = name.clone();
        let editor = cx.new(|cx: &mut Context<ChannelEditor>| {
            cx.subscribe_in(&name, window, |editor, _, event, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    editor.submit(false, cx);
                }
            })
            .detach();
            cx.observe_in(&app, window, |editor, app, window, cx| {
                if !matches!(&app.read(cx).view, View::SignedIn(sidebar) if sidebar.is_admin())
                    || editor.result(cx).is_some_and(|result| result.is_ok())
                {
                    window.close_dialog(cx);
                    editor.restore_focus(window, cx);
                }
                cx.notify();
            })
            .detach();
            ChannelEditor {
                app,
                form,
                name,
                opener,
                submitted_at: None,
            }
        });
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                input.focus_handle(cx).focus(window, cx);
            }
            dialog
                .centered()
                .width(px(440.0))
                .title(title)
                .on_ok(|_, _, _| false)
                .on_close({
                    let editor = editor.clone();
                    move |_, window, cx| {
                        editor.update(cx, |editor, cx| editor.restore_focus(window, cx))
                    }
                })
                .child(editor.clone())
        });
    }
}

#[derive(IntoElement)]
struct ChannelActions {
    id: String,
    name: String,
    kind: ChannelKind,
    can_move_up: bool,
    can_move_down: bool,
    app: WeakEntity<Spk>,
}

impl RenderOnce for ChannelActions {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            name,
            kind,
            can_move_up,
            can_move_down,
            app,
        } = self;
        let focus = window
            .use_keyed_state("channel-actions-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        tool_button(
            "channel-actions",
            IconName::Ellipsis,
            format!("Actions for {name}"),
        )
        .track_focus(&focus)
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
            let editable = app.upgrade().is_some_and(|app| {
                matches!(&app.read(cx).view, View::SignedIn(sidebar)
                    if sidebar.is_admin() && !sidebar.channel_admin.pending)
            });
            let mut menu = menu.item(channel_form_item(
                "Rename…",
                IconName::Pencil,
                ChannelForm::Rename(id.clone(), name.clone()),
                editable,
                app.clone(),
                focus.clone(),
            ));
            for (down, label, icon, enabled) in [
                (false, "Move up", IconName::ArrowUp, can_move_up),
                (true, "Move down", IconName::ArrowDown, can_move_down),
            ] {
                let id = id.clone();
                let app = app.clone();
                let focus = focus.clone();
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .icon(icon)
                        .disabled(!editable || !enabled)
                        .on_click(move |_, window, cx| {
                            focus.focus(window, cx);
                            let _ = app.update(cx, |this, cx| {
                                this.send(
                                    Command::ManageChannel(ChannelAction::Move {
                                        id: id.clone(),
                                        down,
                                    }),
                                    None,
                                    cx,
                                );
                            });
                        }),
                );
            }
            menu.separator().item(channel_form_item(
                "Delete…",
                IconName::Trash,
                ChannelForm::Delete(id.clone(), name.clone(), kind),
                editable,
                app.clone(),
                focus.clone(),
            ))
        })
    }
}

fn channel_form_item(
    label: &'static str,
    icon: IconName,
    form: ChannelForm,
    editable: bool,
    app: WeakEntity<Spk>,
    focus: FocusHandle,
) -> PopupMenuItem {
    PopupMenuItem::new(label)
        .icon(icon)
        .disabled(!editable)
        .on_click(move |_, window, cx| {
            let app = app.clone();
            let form = form.clone();
            let focus = focus.clone();
            // The menu dismisses before the dialog takes focus.
            window.defer(cx, move |window, cx| {
                focus.focus(window, cx);
                let _ = app.update(cx, |this, cx| this.edit_channel(form, window, cx));
            });
        })
}

pub(super) fn channel_status(
    status: Option<Result<&'static str, &'static str>>,
    cx: &App,
) -> Option<impl IntoElement> {
    let theme = cx.theme();
    status.map(|status| {
        let (message, color) = match status {
            Ok(message) => (message, theme.success),
            Err(message) => (message, theme.danger),
        };
        div()
            .id("channel-admin-status")
            .role(Role::Status)
            .child(Label::new(message).text_color(color).text_sm())
    })
}

struct ChannelEditor {
    app: Entity<Spk>,
    form: ChannelForm,
    name: Entity<InputState>,
    opener: Option<FocusHandle>,
    submitted_at: Option<u64>,
}

impl ChannelEditor {
    fn restore_focus(&mut self, window: &mut Window, cx: &mut App) {
        self.submitted_at = None;
        let app = self.app.read(cx);
        if matches!(app.view, View::SignedIn(_)) && !app.sidebar_drawer_open {
            let focus = self
                .opener
                .as_ref()
                .filter(|opener| app.workspace_focus.contains(opener, window))
                .unwrap_or(&app.sidebar_toggle_focus)
                .clone();
            focus.focus(window, cx);
        }
    }

    fn result(&self, cx: &App) -> Option<Result<&'static str, &'static str>> {
        let app = self.app.read(cx);
        let View::SignedIn(sidebar) = &app.view else {
            return None;
        };
        let revision = self.submitted_at?;
        if revision == sidebar.channel_admin.revision {
            app.form_error.map(Err)
        } else {
            sidebar.channel_admin.result
        }
    }

    fn pending(&self, sidebar: &Sidebar, cx: &App) -> bool {
        sidebar.channel_admin.pending
            || (self.submitted_at == Some(sidebar.channel_admin.revision)
                && self.result(cx).is_none())
    }

    fn submit(&mut self, confirm_delete: bool, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.app.read(cx).view else {
            return;
        };
        if !sidebar.is_admin() || self.pending(sidebar, cx) {
            return;
        }
        let name = || self.name.read(cx).value().to_string();
        let action = match &self.form {
            ChannelForm::Create(kind) => ChannelAction::Create(*kind, name()),
            ChannelForm::Rename(id, _) => ChannelAction::Rename(id.clone(), name()),
            ChannelForm::Delete(id, _, _) if confirm_delete => ChannelAction::Delete(id.clone()),
            _ => return,
        };
        self.submitted_at = Some(sidebar.channel_admin.revision);
        self.app.update(cx, |app, cx| {
            app.send(Command::ManageChannel(action), None, cx)
        });
    }
}

impl Render for ChannelEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = matches!(&self.app.read(cx).view,
            View::SignedIn(sidebar) if self.pending(sidebar, cx));
        let (body, submit, delete) = match &self.form {
            ChannelForm::Delete(_, name, kind) => {
                let mut message =
                    format!("Delete {name}? Its messages and files will be removed for everyone.");
                if *kind == ChannelKind::Voice {
                    message.push_str(" Anyone in it will be disconnected.");
                }
                (Label::new(message).into_any_element(), "Delete", true)
            }
            form => (
                Input::new(&self.name)
                    .aria_label("Channel name")
                    .w_full()
                    .disabled(pending)
                    .into_any_element(),
                if matches!(form, ChannelForm::Create(_)) {
                    "Create"
                } else {
                    "Save"
                },
                false,
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(body)
            .children(channel_status(self.result(cx), cx))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        control_button("cancel-channel-edit")
                            .small()
                            .ghost()
                            .tab_stop(true)
                            .label("Cancel")
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                this.restore_focus(window, cx);
                            })),
                    )
                    .child(
                        control_button("submit-channel-edit")
                            .small()
                            .primary()
                            .tab_stop(true)
                            .when(delete, |button| button.danger())
                            .label(if pending { "Saving…" } else { submit })
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| this.submit(delete, cx))),
                    ),
            )
    }
}
