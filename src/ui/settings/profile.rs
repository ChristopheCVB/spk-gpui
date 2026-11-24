use crate::session::ProfileEdit;
use crate::ui::profile::user_status_picker;
use gpui_kit::component::Disableable;

use super::*;

impl Settings {
    pub(super) fn save_nickname(&mut self, cx: &mut Context<Self>) {
        self.avatar_picker_error = None;
        let name = self.nickname.read(cx).value().to_string();
        self.spk.update(cx, |app, cx| {
            app.send(Command::SaveNickname(name), None, cx)
        });
    }

    fn choose_avatar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.avatar_picker_open = true;
        self.avatar_picker_error = None;
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose avatar".into()),
        });
        cx.spawn_in(window, async move |settings, cx| {
            let result = selection.await;
            let _ = settings.update_in(cx, |settings, _, cx| {
                settings.avatar_picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            settings.spk.update(cx, |app, cx| {
                                app.send(Command::UploadAvatar(path), None, cx)
                            });
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => settings.avatar_picker_error = Some("Could not open the file picker."),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn profile(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let profile = &sidebar.profile;
        let theme = cx.theme();
        let spk = self.spk.downgrade();
        let edit = sidebar.profile_edit;
        let disabled = edit.busy() || self.avatar_picker_open;
        let result = self
            .avatar_picker_error
            .or(self.spk.read(cx).form_error)
            .map(|error| (error, theme.danger))
            .or_else(|| match edit {
                ProfileEdit::Failed(error) => Some((error, theme.danger)),
                ProfileEdit::Saved => Some(("Nickname saved.", theme.success)),
                ProfileEdit::Uploaded => Some(("Avatar updated.", theme.success)),
                _ => None,
            });
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_5()
            .child(
                card(
                    "Profile",
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(profile_avatar(
                                    Avatar::new().name(profile.name().to_owned()),
                                    sidebar.avatar(profile.id.as_deref()),
                                    40.0,
                                    false,
                                    cx,
                                ))
                                .child(
                                    icon_label_button(
                                        control_button("choose-avatar").outline(),
                                        IconName::Upload,
                                        if edit == ProfileEdit::Uploading {
                                            "Uploading…"
                                        } else {
                                            "Choose avatar"
                                        },
                                    )
                                    .tab_stop(true)
                                    .disabled(disabled)
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.choose_avatar(window, cx);
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(Label::new("Nickname").text_color(theme.muted_foreground))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            surface_input(&self.nickname, theme.popover, cx)
                                                .w_full()
                                                .aria_label("Nickname")
                                                .disabled(disabled),
                                        )
                                        .child(
                                            control_button("save-nickname")
                                                .primary()
                                                .tab_stop(true)
                                                .label(if edit == ProfileEdit::Saving {
                                                    "Saving…"
                                                } else {
                                                    "Save"
                                                })
                                                .disabled(disabled)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.save_nickname(cx);
                                                })),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Label::new("Status").text_color(theme.muted_foreground))
                                .child(user_status_picker(
                                    "profile-status",
                                    sidebar.user_status,
                                    spk.clone(),
                                )),
                        )
                        .children(sidebar.user_status_notice.map(|notice| {
                            div()
                                .id("user-status-notice")
                                .role(Role::Status)
                                .child(Label::new(notice).text_color(theme.danger))
                        }))
                        .children(result.map(|(message, color)| {
                            div()
                                .id("profile-result")
                                .role(Role::Status)
                                .child(Label::new(message).text_color(color))
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(Label::new("Email").text_color(theme.muted_foreground))
                                .child(Label::new(profile.email.clone())),
                        ),
                    cx,
                )
                .w_full(),
            )
            .child(
                icon_label_button(control_button("sign-out"), IconName::LogOut, "Sign out")
                    .tab_stop(true)
                    .on_click(move |_, window, cx| {
                        let _ = spk.update(cx, |app, cx| app.send(Command::SignOut, None, cx));
                        window.close_dialog(cx);
                    }),
            )
    }
}
