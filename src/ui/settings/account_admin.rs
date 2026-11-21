use gpui_kit::component::{
    Disableable,
    menu::{DropdownMenu, PopupMenuItem},
};

use super::*;
use crate::{
    api::{AccountRole, AdminUser, Invite, InviteStatus},
    session::{AdminAccountAction, AdminPage},
};

impl Tab {
    pub(super) fn admin_page(self) -> Option<AdminPage> {
        match self {
            Self::Invites => Some(AdminPage::Invites),
            Self::Users => Some(AdminPage::Users),
            _ => None,
        }
    }
}

impl Settings {
    pub(super) fn sync_account_admin(&mut self, cx: &Context<Self>) {
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return;
        };
        if !sidebar.is_admin()
            || self.ban_target.as_ref().is_some_and(|id| {
                sidebar
                    .admin_accounts
                    .users
                    .as_ref()
                    .is_some_and(|directory| {
                        !directory
                            .users
                            .iter()
                            .any(|entry| &entry.user.id == id && !entry.user.banned)
                    })
            })
        {
            self.ban_target = None;
        }
    }

    pub(super) fn account_action(&self, action: AdminAccountAction, cx: &mut Context<Self>) {
        self.spk.update(cx, |app, cx| {
            app.send(Command::AdminAccounts(action), None, cx);
        });
    }

    pub(super) fn create_invite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.invite_note.read(cx).value().to_string();
        self.account_action(AdminAccountAction::CreateInvite(note), cx);
        self.invite_note
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    pub(super) fn manage_accounts(
        &self,
        page: AdminPage,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let view = &sidebar.admin_accounts;
        if view.page != Some(page) {
            return div();
        }
        let theme = cx.theme();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                Label::new(match page {
                    AdminPage::Invites => "Invite Links",
                    AdminPage::Users => "Users",
                })
                .text_lg()
                .font_semibold(),
            )
            .child(
                admin_button(
                    "refresh-admin",
                    if view.loading {
                        "Refreshing…"
                    } else {
                        "Refresh"
                    },
                )
                .disabled(view.loading)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.account_action(AdminAccountAction::Show(Some(page)), cx);
                })),
            );
        let app = self.spk.downgrade();
        let link = view.link.as_ref().map(|link| {
            let title = match &link.user {
                Some(user) => format!("Reset link for {user} · shown only once, valid 24 hours, one use"),
                None => "Invite Link · shown only once, valid 5 minutes".to_owned(),
            };
            let url = link.url.clone();
            card(
                "Link",
                div().flex().flex_col().gap_2().min_w_0()
                    .child(Label::new(title).font_semibold())
                    .child(Label::new(url.clone()).truncate())
                    .child(div().child(admin_button("copy-account-link", "Copy")
                        .on_click(move |_, _, cx| {
                            if app.upgrade().is_some_and(|app| matches!(
                                &app.read(cx).view,
                                View::SignedIn(sidebar) if sidebar.is_admin()
                                    && sidebar.admin_accounts.link.as_ref().is_some_and(|link| link.url == url)
                            )) {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                            }
                        }))),
                cx,
            )
        });

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header)
            .children(
                view.error
                    .map(|text| Label::new(text).text_color(theme.danger)),
            )
            .children(
                view.notice
                    .map(|text| Label::new(text).text_color(theme.success)),
            )
            .children(link)
            .child(match page {
                AdminPage::Invites => self.invites(sidebar, cx).into_any_element(),
                AdminPage::Users => self.admin_users(sidebar, cx).into_any_element(),
            })
    }

    fn invites(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let view = &sidebar.admin_accounts;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(card(
                "Create invite",
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Label::new("Note (optional)"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Input::new(&self.invite_note)
                                    .w_full()
                                    .aria_label("Invite note")
                                    .disabled(view.busy),
                            )
                            .child(
                                Button::new("create-invite")
                                    .primary()
                                    .tab_stop(true)
                                    .label("Create")
                                    .disabled(view.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create_invite(window, cx)
                                    })),
                            ),
                    ),
                cx,
            ))
            .children(view.invites.as_ref().map(|rows| {
                card(
                    "Invites",
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(rows.is_empty(), |list| {
                            list.child(Label::new("No invites yet."))
                        })
                        .children(rows.iter().map(|row| self.invite_row(row, view.busy, cx))),
                    cx,
                )
            }))
    }

    fn invite_row(&self, row: &Invite, busy: bool, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (status, color) = match row.status {
            InviteStatus::Active => ("Active", theme.success),
            InviteStatus::Used => ("Used", theme.muted_foreground),
            InviteStatus::Expired => ("Expired", theme.warning),
            InviteStatus::Revoked => ("Revoked", theme.danger),
        };
        let expires = chrono::DateTime::from_timestamp_millis(row.expires_at).map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%x %X")
                .to_string()
        });
        let id = row.id.clone();
        div()
            .id(SharedString::from(format!("invite-{id}")))
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(Label::new(status).text_color(color).font_semibold())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Label::new(row.note.clone().unwrap_or_else(|| "No note".into())).truncate(),
                    )
                    .children(expires.map(|expires| {
                        Label::new(format!("Expires {expires}"))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                    }))
                    .children(row.used_by.as_ref().map(|user| {
                        Label::new(format!("Used by {}", user.name))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                    })),
            )
            .children(row.note.as_ref().map(|note| {
                let note = note.clone();
                admin_button("copy-invite-note", "Copy note").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(note.clone()));
                })
            }))
            .children(row.used_by.as_ref().map(|user| {
                let id = user.id.clone();
                admin_button("view-invite-user", "View user").on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.open_admin_profile(id.clone(), window, cx);
                    },
                ))
            }))
            .when(row.status == InviteStatus::Active, |row| {
                row.child(
                    admin_button("revoke-invite", "Revoke")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.account_action(AdminAccountAction::RevokeInvite(id.clone()), cx);
                        })),
                )
            })
    }

    fn admin_users(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let view = &sidebar.admin_accounts;
        let theme = cx.theme();
        let Some(directory) = &view.users else {
            return div();
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(card(
                "Storage",
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Label::new(format!(
                        "Attachments and Soundboard clips · {}",
                        size(directory.total_bytes)
                    )))
                    .child(
                        Label::new("Avatars and link thumbnails are excluded.")
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    ),
                cx,
            ))
            .child(card(
                "Accounts",
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(directory.users.is_empty(), |body| {
                        body.child(Label::new("No users."))
                    })
                    .children(directory.users.iter().map(|row| {
                        let confirm =
                            !row.user.banned && self.ban_target.as_ref() == Some(&row.user.id);
                        div()
                            .child(self.admin_user_row(row, sidebar, cx))
                            .when(confirm, |content| {
                                content.child(self.confirm_ban(row, view.busy, cx))
                            })
                    })),
                cx,
            ))
    }

    fn admin_user_row(
        &self,
        row: &AdminUser,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let user = &row.user;
        let id = user.id.clone();
        let me = sidebar.profile.id.as_ref() == Some(&id);
        let banned = user.banned;
        let admin = user.role == "admin";
        let name = user.name().unwrap_or("No nickname yet");
        let account_name = user.name().unwrap_or(&row.email).to_owned();
        let settings = cx.weak_entity();
        div()
            .id(SharedString::from(format!("admin-user-{id}")))
            .flex()
            .items_center()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(profile_avatar(
                Avatar::new().name(name.to_owned()).small(),
                sidebar.avatar(Some(&id)),
                20.0,
                false,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(name.to_owned()).font_semibold().truncate())
                    .child(
                        Label::new(format!("{} · {}", row.email, size(row.bytes)))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate(),
                    ),
            )
            .child(
                Label::new(if admin { "Admin" } else { "User" }).text_color(if admin {
                    theme.primary
                } else {
                    theme.muted_foreground
                }),
            )
            .when(banned, |row| {
                row.child(Label::new("Banned").text_color(theme.danger))
            })
            .child(
                tool_button(
                    "admin-user-actions",
                    IconName::Ellipsis,
                    format!("Actions for {account_name}"),
                )
                .disabled(sidebar.admin_accounts.busy)
                .dropdown_menu(move |menu, _, _| {
                    let profile = settings.clone();
                    let profile_id = id.clone();
                    let reset_id = id.clone();
                    let reset_name = account_name.clone();
                    let ban = settings.clone();
                    let ban_id = id.clone();
                    menu.item(
                        PopupMenuItem::new("Profile").on_click(move |_, window, cx| {
                            let _ = profile.update(cx, |settings, cx| {
                                settings.open_admin_profile(profile_id.clone(), window, cx)
                            });
                        }),
                    )
                    .item(account_menu_action(
                        if admin { "Make user" } else { "Make admin" },
                        settings.clone(),
                        {
                            let id = id.clone();
                            move || {
                                AdminAccountAction::SetRole(
                                    id.clone(),
                                    if admin {
                                        AccountRole::User
                                    } else {
                                        AccountRole::Admin
                                    },
                                )
                            }
                        },
                    ))
                    .item(account_menu_action(
                        "Reset password",
                        settings.clone(),
                        move || AdminAccountAction::ResetPassword {
                            id: reset_id.clone(),
                            name: reset_name.clone(),
                        },
                    ))
                    .when(!me || banned, |menu| {
                        if banned {
                            menu.item(account_menu_action("Unban", ban, move || {
                                AdminAccountAction::Unban(ban_id.clone())
                            }))
                        } else {
                            menu.item(PopupMenuItem::new("Ban…").on_click(move |_, window, cx| {
                                let _ = ban.update(cx, |settings, cx| {
                                    settings.ban_target = Some(ban_id.clone());
                                    settings
                                        .ban_reason
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                    cx.notify();
                                });
                            }))
                        }
                    })
                }),
            )
    }

    fn confirm_ban(&self, user: &AdminUser, busy: bool, cx: &Context<Self>) -> impl IntoElement {
        let id = user.user.id.clone();
        let name = user.user.name().unwrap_or(&user.email);
        card(
            "Ban user",
            div().flex().flex_col().gap_2()
                .child(Label::new(format!("Ban {name}? They’ll be signed out and disconnected from voice. Their messages stay.")).whitespace_normal())
                .child(Label::new("Reason (optional)"))
                .child(Input::new(&self.ban_reason).aria_label("Ban reason").disabled(busy))
                .child(div().flex().gap_2()
                    .child(admin_button("cancel-ban", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.ban_target = None;
                        cx.notify();
                    })))
                    .child(Button::new("confirm-ban").danger().tab_stop(true).label("Ban").disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let reason = this.ban_reason.read(cx).value().to_string();
                            this.account_action(AdminAccountAction::Ban { id: id.clone(), reason }, cx);
                        })))),
            cx,
        )
    }

    fn open_admin_profile(&self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.spk.update(cx, |app, cx| {
            if matches!(&app.view, View::SignedIn(sidebar) if sidebar.is_admin()) {
                app.open_person_profile(id, window, cx);
            }
        });
    }
}

fn admin_button(id: &'static str, label: &'static str) -> Button {
    Button::new(id)
        .small()
        .outline()
        .tab_stop(true)
        .label(label)
}

fn account_menu_action(
    label: &'static str,
    settings: WeakEntity<Settings>,
    action: impl Fn() -> AdminAccountAction + 'static,
) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        let _ = settings.update(cx, |settings, cx| settings.account_action(action(), cx));
    })
}

fn size(bytes: u64) -> String {
    let mut count = bytes as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if count < 1024.0 || unit == "GB" {
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{count:.1} {unit}")
            };
        }
        count /= 1024.0;
    }
    unreachable!()
}
