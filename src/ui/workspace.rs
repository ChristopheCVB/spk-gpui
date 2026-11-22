use gpui_kit::component::{
    Disableable, Placement,
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    resizable::{HandleEdge, resize_handle},
    window_paddings,
};

use super::{
    chat::{chat_destination, mark_read_item, unread_badges, unread_label},
    *,
};
use crate::{
    api::{Channel, Connection, VoiceModeration},
    session::{Participant, VoiceView},
    voice::video::CameraStatus,
};

pub(super) const SIDEBAR_WIDTH: Pixels = px(288.0);

pub(super) fn wide_workspace(window: &Window) -> bool {
    let padding = window_paddings(window);
    window.viewport_size().width - padding.left - padding.right >= px(1024.0)
}

struct SidebarResize;

impl Render for SidebarResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn section_title(title: &'static str, icon: IconName, cx: &App) -> Div {
    let color = cx.theme().muted_foreground;
    div()
        .flex()
        .items_center()
        .gap_2()
        .h_8()
        .px_2()
        .text_color(color)
        .font_semibold()
        .child(themed_icon(icon, color, 16.0))
        .child(title)
}

fn call_status(call: &VoiceView, switching: bool) -> &'static str {
    match call.status {
        Status::Joining => "Joining…",
        Status::Connected => "Connected",
        Status::ListenOnly => "Listen only",
        Status::Leaving if switching => "Switching…",
        Status::Reconnecting => "Reconnecting voice…",
        Status::Leaving => "Leaving…",
    }
}

pub(super) fn channel_name(channel: &Channel) -> &str {
    channel.name.as_deref().unwrap_or("Unnamed channel")
}

pub(super) fn voice_channels(sidebar: &Sidebar) -> impl Iterator<Item = &Channel> {
    sidebar
        .channels
        .iter()
        .flatten()
        .filter(|channel| channel.kind == "voice")
}

impl Spk {
    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_sidebar_drawer(window, cx) || self.video_stage.fullscreen {
            return;
        }
        self.device_picker = None;
        if wide_workspace(window) {
            self.sidebar_open = !self.sidebar_open;
        } else {
            self.close_message_search(window, cx);
            self.sidebar_drawer_open = true;
            let app = cx.weak_entity();
            window.open_sheet_at(Placement::Left, cx, move |sheet, _, cx| {
                let drawer = app.update(cx, |this, cx| {
                    let View::SignedIn(sidebar) = &this.view else {
                        return None;
                    };
                    Some((
                        this.navigation(sidebar, this.selected_voice_destination(sidebar), cx)
                            .into_any_element(),
                        this.navigation_footer(sidebar, cx).into_any_element(),
                    ))
                });
                let app = app.clone();
                sheet
                    .title("Channels and direct messages")
                    .size(SIDEBAR_WIDTH)
                    .resizable(false)
                    .bg(cx.theme().sidebar)
                    .text_color(cx.theme().sidebar_foreground)
                    .px_0()
                    .when_some(drawer.ok().flatten(), |sheet, (navigation, footer)| {
                        sheet.child(navigation).footer(footer)
                    })
                    .on_close(move |_, _, cx| {
                        let _ = app.update(cx, |this, cx| {
                            this.sidebar_drawer_open = false;
                            this.device_picker = None;
                            cx.notify();
                        });
                    })
            });
        }
        cx.notify();
    }

    pub(super) fn close_sidebar_drawer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !std::mem::take(&mut self.sidebar_drawer_open) {
            return false;
        }
        self.device_picker = None;
        window.close_sheet(cx);
        cx.notify();
        true
    }

    fn selected_voice_destination<'a>(&self, sidebar: &'a Sidebar) -> Option<&'a Channel> {
        voice_channels(sidebar)
            .find(|channel| Some(channel.id.as_str()) == self.selected_voice_channel.as_deref())
            .or_else(|| voice_channels(sidebar).next())
    }

    pub(super) fn workspace(
        &self,
        sidebar: &Sidebar,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected_channel = self.selected_voice_destination(sidebar);
        let wide = wide_workspace(window);
        let inline_sidebar = wide && self.sidebar_open && !self.video_stage.fullscreen;
        let chat = sidebar
            .chat
            .as_ref()
            .filter(|_| !self.video_stage.fullscreen);
        let (stage_icon, stage_name) = match chat {
            Some(chat) => {
                let destination = chat_destination(sidebar, chat);
                (destination.icon, destination.name)
            }
            None => (
                IconName::Volume2,
                selected_channel.map_or("Voice channels", channel_name),
            ),
        };
        let theme = cx.theme();
        let status = match sidebar.connection {
            Connection::Connecting => Some("Connecting to live updates…"),
            Connection::Online => None,
            Connection::Reconnecting => Some("Reconnecting to live updates…"),
            Connection::Forbidden => Some("SPK denied the live connection."),
        };
        let notices = [
            status,
            sidebar.notice,
            sidebar.chat_notice,
            sidebar.read_notice,
            sidebar.device_notice,
            sidebar.voice_moderation_notice,
            self.form_error,
        ];
        let rejoin = sidebar.rejoin_channel().map(str::to_owned);

        div()
            .id("workspace")
            .size_full()
            .relative()
            .flex()
            .when(inline_sidebar, |page| {
                page.on_drag_move(cx.listener(
                    |this, drag: &DragMoveEvent<SidebarResize>, _, cx| {
                        this.sidebar_width = (drag.event.position.x - drag.bounds.left())
                            .clamp(px(248.0), px(368.0));
                        cx.notify();
                    },
                ))
                .child(
                    div()
                        .relative()
                        .w(self.sidebar_width)
                        .h_full()
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .bg(theme.sidebar)
                        .text_color(theme.sidebar_foreground)
                        .child(self.navigation(sidebar, selected_channel, cx))
                        .child(
                            div()
                                .px_2()
                                .pb_2()
                                .child(self.navigation_footer(sidebar, cx)),
                        )
                        .child(
                            resize_handle("sidebar-edge", Axis::Horizontal)
                                .inside(HandleEdge::Trailing)
                                .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| SidebarResize)),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .bg(theme.background)
                    .when(!self.video_stage.fullscreen, |area| {
                        area.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .h_12()
                                .flex_shrink_0()
                                .px_3()
                                .bg(theme.sidebar)
                                .border_b_1()
                                .border_color(theme.sidebar_border)
                                .child(
                                    tool_button(
                                        "toggle-sidebar",
                                        if inline_sidebar {
                                            IconName::PanelLeftClose
                                        } else {
                                            IconName::PanelLeftOpen
                                        },
                                        if inline_sidebar {
                                            "Hide sidebar"
                                        } else {
                                            "Open sidebar"
                                        },
                                    )
                                    .flex_shrink_0()
                                    .track_focus(&self.sidebar_toggle_focus)
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.sidebar_toggle_focus.focus(window, cx);
                                            this.toggle_sidebar(window, cx);
                                        },
                                    )),
                                )
                                .child(themed_icon(stage_icon, theme.muted_foreground, 16.0))
                                .child(
                                    Label::new(stage_name.to_owned())
                                        .font_semibold()
                                        .min_w_0()
                                        .truncate(),
                                )
                                .child(
                                    tool_button(
                                        "message-search",
                                        IconName::Search,
                                        "Search messages",
                                    )
                                    .ml_auto()
                                    .flex_shrink_0()
                                    .track_focus(&self.search_button_focus)
                                    .selected(self.search_panel.is_some())
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.search_button_focus.focus(window, cx);
                                            this.open_message_search(window, cx);
                                        },
                                    )),
                                ),
                        )
                    })
                    .when(sidebar.load_failed, |area| {
                        area.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_4()
                                .py_1()
                                .child("Could not load all channel data.")
                                .child(Button::new("refresh").small().label("Retry").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.send(Command::Refresh, None, cx)
                                    }),
                                )),
                        )
                    })
                    .children(notices.into_iter().flatten().map(|notice| {
                        div()
                            .px_4()
                            .py_1()
                            .text_color(theme.muted_foreground)
                            .child(notice)
                    }))
                    .when_some(sidebar.voice_notice, |area, notice| {
                        area.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_4()
                                .py_1()
                                .text_color(theme.muted_foreground)
                                .child(notice)
                                .when_some(rejoin, |row, id| {
                                    row.child(
                                        Button::new("voice-rejoin")
                                            .small()
                                            .tab_stop(true)
                                            .label("Rejoin voice")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.join_voice_channel(&id, cx)
                                            })),
                                    )
                                }),
                        )
                    })
                    .child(match chat {
                        Some(chat) => self.chat_stage(sidebar, chat, cx).into_any_element(),
                        None => self
                            .voice_channel_stage(sidebar, selected_channel, cx)
                            .into_any_element(),
                    })
                    .when(
                        !inline_sidebar
                            && !self.sidebar_drawer_open
                            && !self.video_stage.fullscreen,
                        |area| {
                            area.children(
                                sidebar.voice.as_ref().map(|call| {
                                    self.persistent_voice_session(sidebar, call, true, cx)
                                }),
                            )
                        },
                    ),
            )
            .when(
                !self.video_stage.fullscreen && self.search_panel.is_some(),
                |page| page.child(self.search_surface(wide, window, cx)),
            )
    }

    fn navigation(
        &self,
        sidebar: &Sidebar,
        selected_channel: Option<&Channel>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let selected = if sidebar.chat.is_none() {
            selected_channel.map(|channel| channel.id.as_str())
        } else {
            None
        };
        let people = sidebar.people();
        let has_text = sidebar
            .channels
            .iter()
            .flatten()
            .any(|channel| channel.kind == "text");

        div()
            .id("channel-navigation")
            .flex()
            .flex_col()
            .px_2()
            .gap(if self.compact() { px(4.0) } else { px(12.0) })
            .when(!self.sidebar_drawer_open, |nav| {
                nav.flex_1().min_h_0().overflow_y_scroll().child(
                    div()
                        .flex()
                        .h_16()
                        .items_center()
                        .justify_center()
                        .flex_shrink_0()
                        .gap_1()
                        .text_2xl()
                        .font_bold()
                        .text_color(theme.primary)
                        .child(logo(26.0))
                        .child("spk"),
                )
            })
            .when(!people.is_empty(), |nav| {
                nav.child(
                    div()
                        .flex_shrink_0()
                        .child(section_title(
                            "Direct messages",
                            IconName::MessageCircle,
                            cx,
                        ))
                        .children(
                            people
                                .into_iter()
                                .map(|(id, name)| self.person_row(sidebar, id, name, cx)),
                        ),
                )
            })
            .when(has_text, |nav| {
                nav.child(
                    div()
                        .flex_shrink_0()
                        .child(section_title("Text channels", IconName::Hash, cx))
                        .children(
                            sidebar
                                .channels
                                .iter()
                                .flatten()
                                .filter(|channel| channel.kind == "text")
                                .map(|channel| {
                                    let id = channel.id.clone();
                                    let name = channel_name(channel).to_owned();
                                    let active = sidebar
                                        .chat
                                        .as_ref()
                                        .is_some_and(|chat| chat.channel_id == id);
                                    let counts = sidebar.unread(&id);
                                    let muted = self.notification_preferences.muted.contains(&id);
                                    let menu_id = id.clone();
                                    let app = cx.weak_entity();
                                    div()
                                        .id(SharedString::from(format!("text-{id}")))
                                        .child(
                                            Button::new("select")
                                                .ghost()
                                                .small()
                                                .tab_stop(true)
                                                .selected(active)
                                                .accessibility_label(unread_label(&name, counts))
                                                .w_full()
                                                .h_8()
                                                .when(active || counts.mentions > 0, |button| {
                                                    button.bg(theme.primary.opacity(0.10))
                                                })
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .w_full()
                                                        .min_w_0()
                                                        .child(
                                                            Label::new(name)
                                                                .flex_1()
                                                                .truncate()
                                                                .when(counts.unread > 0, |label| {
                                                                    label.font_bold()
                                                                }),
                                                        )
                                                        .child(unread_badges(counts, cx)),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.close_sidebar_drawer(window, cx);
                                                        this.device_picker = None;
                                                        this.send(
                                                            Command::OpenTextChannel(id.clone()),
                                                            None,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        )
                                        .context_menu(move |menu, _, _| {
                                            menu.item(mark_read_item(menu_id.clone(), app.clone()))
                                                .item(notifications::mute_item(
                                                    menu_id.clone(),
                                                    muted,
                                                    app.clone(),
                                                ))
                                        })
                                }),
                        ),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .child(section_title("Voice channels", IconName::Volume2, cx))
                    .children(
                        voice_channels(sidebar)
                            .map(|channel| self.channel_row(sidebar, channel, selected, cx)),
                    ),
            )
    }

    fn navigation_footer(&self, sidebar: &Sidebar, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let name = sidebar.profile.name();
        div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(if self.compact() { px(4.0) } else { px(8.0) })
            .children(
                sidebar
                    .voice
                    .as_ref()
                    .map(|call| self.persistent_voice_session(sidebar, call, false, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .child(profile_avatar(
                        Avatar::new().name(name.to_owned()).small(),
                        sidebar.avatar(sidebar.profile.id.as_deref()),
                        28.0,
                        false,
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_start()
                            .flex_1()
                            .min_w(px(48.0))
                            .ml_1()
                            .child(Label::new(name.to_owned()).font_semibold().truncate())
                            .child(profile::user_status_picker(
                                "sidebar-status",
                                sidebar.user_status,
                                cx.weak_entity(),
                            )),
                    )
                    .child(
                        tool_button("settings", IconName::Settings, "Settings")
                            .track_focus(&self.settings_button_focus)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.settings_button_focus.focus(window, cx);
                                this.open_settings(window, cx);
                            })),
                    )
                    .child(self.voice_controls(sidebar, VoiceControls::Profile, cx)),
            )
    }

    fn persistent_voice_session(
        &self,
        sidebar: &Sidebar,
        call: &VoiceView,
        bottom_bar: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let switching = sidebar.switching_to().is_some();
        let id = sidebar
            .switching_to()
            .unwrap_or(&call.channel_id)
            .to_owned();
        let voice_channel_name = voice_channels(sidebar)
            .find(|channel| channel.id == id)
            .map_or("Voice", channel_name);
        div()
            .id("voice-session")
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .p_2()
            .bg(theme.secondary)
            .when(bottom_bar, |bar| {
                bar.border_t_1().border_color(theme.border)
            })
            .when(!bottom_bar, |card| card.rounded_lg())
            .child(
                tool_button(
                    "current-call",
                    IconName::Volume2,
                    "Show current voice channel",
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_sidebar_drawer(window, cx);
                    this.select_voice_channel(&id, cx);
                })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Label::new(voice_channel_name.to_owned())
                            .font_semibold()
                            .truncate(),
                    )
                    .child(
                        Label::new(call_status(call, switching))
                            .text_xs()
                            .truncate()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .when(call.is_participating(), |card| {
                card.children(self.voice_media_buttons(false, cx))
            })
            .when(bottom_bar && call.is_participating(), |bar| {
                bar.child(self.voice_controls(sidebar, VoiceControls::BottomBar, cx))
            })
            .children(self.leave_voice("persistent-voice-leave", call, switching, bottom_bar, cx))
    }

    fn channel_row(
        &self,
        sidebar: &Sidebar,
        channel: &Channel,
        selected: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let switching_here = sidebar.switching_to() == Some(channel.id.as_str());
        let call = sidebar
            .voice
            .as_ref()
            .filter(|call| switching_here || call.channel_id == channel.id);
        let active = selected == Some(channel.id.as_str());
        let id = channel.id.clone();
        let join_id = channel.id.clone();
        let name = channel_name(channel).to_owned();
        let can_join = sidebar.can_join_voice(&id);
        let join_label = if sidebar.voice.is_some() {
            format!("Switch to {name}")
        } else {
            format!("Join {name}")
        };
        div()
            .id(SharedString::from(format!("voice-{id}")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_8()
                    .rounded_md()
                    .pr_1()
                    .when(active, |row| row.bg(theme.primary.opacity(0.10)))
                    .child(
                        Button::new("select")
                            .ghost()
                            .small()
                            .tab_stop(true)
                            .selected(active)
                            .accessibility_label(name.clone())
                            .flex_1()
                            .min_w_0()
                            .h_8()
                            .when(active, |button| button.bg(theme.transparent))
                            .child(Label::new(name.clone()).w_full().truncate().text_color(
                                if active {
                                    theme.primary
                                } else {
                                    theme.foreground
                                },
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_sidebar_drawer(window, cx);
                                this.select_voice_channel(&id, cx);
                            })),
                    )
                    .children(sidebar.channel_radio.get(&channel.id).map(|station| {
                        div()
                            .id("radio-station")
                            .role(Role::Image)
                            .aria_label(format!("Radio playing {station}"))
                            .px_1()
                            .child(themed_icon(IconName::Radio, theme.primary, 14.0))
                    }))
                    .children(call.and_then(|call| {
                        self.leave_voice("leave", call, switching_here, false, cx)
                    }))
                    .when(can_join, |row| {
                        row.child(tool_button("join", IconName::Phone, join_label).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.close_sidebar_drawer(window, cx);
                                this.join_voice_channel(&join_id, cx);
                            }),
                        ))
                    }),
            )
            .children(sidebar.participants(&channel.id).into_iter().map(|person| {
                let flags = person.flags;
                div()
                    .id(SharedString::from(format!("member-{}", person.id)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_6()
                    .pr_2()
                    .h_6()
                    .child(profile_avatar(
                        Avatar::new().name(person.name.to_owned()).xsmall(),
                        sidebar.avatar(Some(person.id)),
                        16.0,
                        flags.speaking && !flags.muted && !flags.deafened,
                        cx,
                    ))
                    .child(
                        Label::new(person.name.to_owned())
                            .truncate()
                            .text_color(theme.muted_foreground),
                    )
                    .when(flags.speaking && !flags.muted && !flags.deafened, |row| {
                        row.child(speaking_badge(cx))
                    })
                    .children(participant_status(flags.muted, flags.deafened, cx))
                    .children(voice_moderation_menu(&person, sidebar, cx))
                    .when(
                        sidebar.profile.id.as_deref() != Some(person.id)
                            && call.is_some_and(|call| {
                                call.channel_id == channel.id && call.is_participating()
                            }),
                        |row| row.child(self.participant_volume_button(&person, sidebar, cx)),
                    )
                    .when(flags.radio_listening == Some(true), |row| {
                        row.child(radio_badge(cx))
                    })
            }))
    }

    pub(super) fn select_voice_channel(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selected_voice_channel = Some(id.to_owned());
        self.send(Command::CloseChat, None, cx);
        self.device_picker = None;
        cx.notify();
    }

    fn join_voice_channel(&mut self, id: &str, cx: &mut Context<Self>) {
        self.select_voice_channel(id, cx);
        self.send(Command::JoinVoice(id.to_owned()), None, cx);
    }

    fn voice_channel_stage(
        &self,
        sidebar: &Sidebar,
        selected_channel: Option<&Channel>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let call = selected_channel.and_then(|channel| {
            sidebar
                .voice
                .as_ref()
                .filter(|call| call.channel_id == channel.id)
        });
        let in_voice = call.is_some_and(VoiceView::is_participating);
        let mut participants =
            selected_channel.map_or_else(Vec::new, |channel| sidebar.participants(&channel.id));
        if let Some(id) = sidebar.profile.id.as_deref().filter(|_| in_voice)
            && !participants.iter().any(|person| person.id == id)
        {
            // The local media can connect before SPK echoes our voice presence.
            participants.insert(
                0,
                Participant {
                    id,
                    name: sidebar.profile.name(),
                    flags: Default::default(),
                    video: None,
                },
            );
        }
        let message = match (selected_channel, sidebar.channels.is_some()) {
            (None, false) if !sidebar.load_failed => Some("Loading voice channels…"),
            (None, true) => Some("No voice channels"),
            (Some(_), _) if participants.is_empty() => Some("Nobody is here yet."),
            _ => None,
        };
        let theater =
            self.video_stage.theater.as_deref().filter(|id| {
                in_voice && participants.iter().any(|participant| participant.id == *id)
            });
        let camera_status = call.and_then(|_| match sidebar.camera_status() {
            CameraStatus::Starting => Some("Starting Camera…"),
            CameraStatus::Failed(message) => Some(message),
            _ => None,
        });
        div()
            .id("voice-stage")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("voice-tiles")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .when(theater.is_none(), |tiles| tiles.overflow_y_scroll())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap(if self.compact() { px(6.0) } else { px(12.0) })
                            .when(theater.is_none(), |tiles| {
                                tiles
                                    .p(if self.compact() { px(8.0) } else { px(16.0) })
                                    .my_auto()
                            })
                            .when(theater.is_some(), |tiles| {
                                tiles
                                    .flex_1()
                                    .min_h_0()
                                    .when(!self.video_stage.fullscreen, |tiles| tiles.p_3())
                            })
                            .children(message.map(|message| {
                                Label::new(message).text_color(theme.muted_foreground)
                            }))
                            .children(
                                participants
                                    .into_iter()
                                    .filter(|person| theater.is_none_or(|id| person.id == id))
                                    .map(|person| self.voice_tile(person, call, sidebar, cx)),
                            ),
                    ),
            )
            .when_some(
                camera_status.filter(|_| !self.video_stage.fullscreen),
                |stage, message| {
                    stage.child(
                        Label::new(message)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_4()
                            .pb_2(),
                    )
                },
            )
            .children(
                call.filter(|_| in_voice && !self.video_stage.fullscreen)
                    .and_then(|call| self.screen_options(call, sidebar, cx)),
            )
            .when_some(
                selected_channel.filter(|_| !self.video_stage.fullscreen),
                |stage, channel| {
                    let id = channel.id.clone();
                    let switching_here = sidebar.switching_to() == Some(id.as_str());
                    let departing = if switching_here {
                        sidebar.voice.as_ref()
                    } else {
                        call
                    };
                    stage.child(
                        div()
                            .relative()
                            .flex()
                            .justify_center()
                            .px_2()
                            .pb_2()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_center()
                                    .gap_1()
                                    .rounded_lg()
                                    .when(in_voice, |bar| {
                                        bar.bg(theme.secondary)
                                            .p_2()
                                            .child(self.voice_controls(
                                                sidebar,
                                                VoiceControls::Channel,
                                                cx,
                                            ))
                                            .children(self.voice_media_buttons(true, cx))
                                            .child(self.camera_controls(sidebar, cx))
                                            .child(self.screen_button(call, cx))
                                            .child(super::voice::connection_diagnostics(cx))
                                    })
                                    .children(departing.and_then(|call| {
                                        self.leave_voice(
                                            "voice-channel-leave",
                                            call,
                                            switching_here,
                                            true,
                                            cx,
                                        )
                                    }))
                                    .children(
                                        departing
                                            .filter(|call| {
                                                matches!(
                                                    call.status,
                                                    Status::Reconnecting | Status::Leaving
                                                )
                                            })
                                            .map(|call| call_status(call, switching_here)),
                                    )
                                    .when(sidebar.can_join_voice(&id), |bar| {
                                        bar.child(
                                            Button::new("voice-channel-join")
                                                .primary()
                                                .small()
                                                .tab_stop(true)
                                                .icon(IconName::Phone)
                                                .label(if sidebar.voice.is_some() {
                                                    "Switch voice channel"
                                                } else {
                                                    "Join voice"
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.join_voice_channel(&id, cx)
                                                })),
                                        )
                                    }),
                            ),
                    )
                },
            )
    }

    fn leave_voice(
        &self,
        id: &'static str,
        call: &VoiceView,
        switching: bool,
        labeled: bool,
        cx: &Context<Self>,
    ) -> Option<Button> {
        let (icon, label) = match call.status {
            Status::Joining => (IconName::X, "Cancel join"),
            Status::Leaving if switching => (IconName::X, "Cancel switch"),
            Status::Leaving => return None,
            _ => (IconName::PhoneOff, "Leave voice"),
        };
        Some(
            tool_button(id, themed_icon(icon, cx.theme().danger, 16.0), label)
                .when(labeled, |button| {
                    button.label(label).text_color(cx.theme().danger)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.device_picker = None;
                    this.send(Command::LeaveVoice, None, cx);
                })),
        )
    }

    fn voice_tile(
        &self,
        participant: Participant<'_>,
        call: Option<&VoiceView>,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let is_me = sidebar.profile.id.as_deref() == Some(participant.id);
        let media = call.and_then(|call| call.media.as_ref());
        let flags = if is_me && call.is_some() {
            crate::api::VoiceFlags {
                muted: sidebar.voice_intent.mute.effective_muted()
                    || call.is_some_and(|call| {
                        matches!(call.status, Status::ListenOnly | Status::Reconnecting)
                    }),
                deafened: sidebar.voice_intent.mute.deafened,
                radio_listening: sidebar.voice_intent.radio_listening,
                ..participant.flags
            }
        } else {
            participant.flags
        };
        let speaking = !flags.muted
            && !flags.deafened
            && media.map_or(flags.speaking, |media| {
                media.speaking.contains(participant.id)
            });
        let theater = self.video_stage.theater.as_deref() == Some(participant.id)
            && call.is_some_and(VoiceView::is_participating);
        let camera_live = if is_me {
            matches!(sidebar.camera_status(), CameraStatus::Live(..))
        } else {
            call.and_then(|call| call.video.as_ref())
                .and_then(|video| video.participants.get(participant.id))
                .map_or_else(
                    || {
                        participant
                            .video
                            .is_some_and(|video| video.cam || video.screen)
                    },
                    |video| !video.live.is_empty(),
                )
        };
        let (image, camera_inset) = self.video_images(&participant, is_me);
        let hover_id = participant.id.to_owned();
        let key_id = hover_id.clone();
        div()
            .id(SharedString::from(participant.id.to_owned()))
            .focusable()
            .tab_index(0)
            .focus_visible(|tile| tile.border_color(theme.ring))
            .on_hover(cx.listener(move |app, hovered: &bool, _, _| {
                if *hovered {
                    app.video_stage.hovered = Some(hover_id.clone());
                } else if app.video_stage.hovered.as_deref() == Some(&hover_id) {
                    app.video_stage.hovered = None;
                }
            }))
            .on_key_down(cx.listener(move |app, event, window, cx| {
                app.video_tile_key(&key_id, event, window, cx)
            }))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .role(Role::Group)
            .when(speaking, |tile| tile.aria_description("Speaking"))
            .when(theater, |tile| tile.size_full())
            .when(!theater, |tile| tile.w(px(320.0)).max_w_full().h(px(180.0)))
            .overflow_hidden()
            .border_1()
            .border_color(if speaking {
                theme.success
            } else {
                theme.border
            })
            .rounded_lg()
            .bg(theme.popover)
            .when(image.is_none(), |tile| {
                tile.child(profile_avatar(
                    Avatar::new().name(participant.name.to_owned()),
                    sidebar.avatar(Some(participant.id)),
                    48.0,
                    false,
                    cx,
                ))
            })
            .children(image)
            .when_some(camera_inset, |tile, camera| {
                tile.child(
                    div()
                        .absolute()
                        .top_10()
                        .right_2()
                        .w(relative(0.26))
                        .max_w(px(200.0))
                        .aspect_ratio(16.0 / 9.0)
                        .overflow_hidden()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.secondary)
                        .child(camera),
                )
            })
            .child(
                div()
                    .absolute()
                    .top_2()
                    .left_2()
                    .right_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .max_w(relative(0.84))
                            .rounded_md()
                            .px_2()
                            .py_0p5()
                            .bg(theme.background)
                            .child(
                                Label::new(if is_me { "You" } else { participant.name }.to_owned())
                                    .text_xs()
                                    .truncate(),
                            ),
                    )
                    .when(camera_live, |row| {
                        row.child(
                            Label::new("Live")
                                .text_xs()
                                .px_2()
                                .rounded_md()
                                .bg(theme.background),
                        )
                    })
                    .when(speaking, |row| row.child(speaking_badge(cx)))
                    .children(participant_status(flags.muted, flags.deafened, cx))
                    .children(voice_moderation_menu(&participant, sidebar, cx))
                    .when(
                        !is_me && call.is_some_and(VoiceView::is_participating),
                        |row| row.child(self.participant_volume_button(&participant, sidebar, cx)),
                    )
                    .when(flags.radio_listening == Some(true), |row| {
                        row.child(radio_badge(cx))
                    }),
            )
            .children(
                call.filter(|call| call.is_participating())
                    .map(|call| self.video_tile_controls(&participant, call, sidebar, cx)),
            )
    }
}

fn voice_moderation_menu(
    participant: &Participant<'_>,
    sidebar: &Sidebar,
    cx: &Context<Spk>,
) -> Option<impl IntoElement> {
    let destinations = sidebar
        .voice_moderation_channels(participant.id)?
        .into_iter()
        .map(|channel| (channel.id.clone(), channel_name(channel).to_owned()))
        .collect::<Vec<_>>();
    let user_id = participant.id.to_owned();
    let app = cx.weak_entity();
    Some(
        tool_button(
            "voice-moderation",
            IconName::Ellipsis,
            format!("Moderate {}", participant.name),
        )
        .disabled(sidebar.voice_moderation_pending)
        .dropdown_menu(move |mut menu, window, cx| {
            if !destinations.is_empty() {
                let app = app.clone();
                let user_id = user_id.clone();
                let destinations = destinations.clone();
                menu = menu.submenu("Move to", window, cx, move |mut submenu, _, _| {
                    for (id, name) in &destinations {
                        submenu = submenu.item(voice_moderation_item(
                            name.clone(),
                            app.clone(),
                            user_id.clone(),
                            VoiceModeration::Move(id.clone()),
                        ));
                    }
                    submenu
                });
            }
            menu.item(voice_moderation_item(
                "Disconnect from voice",
                app.clone(),
                user_id.clone(),
                VoiceModeration::Disconnect,
            ))
        }),
    )
}

fn voice_moderation_item(
    label: impl Into<SharedString>,
    app: WeakEntity<Spk>,
    user_id: String,
    action: VoiceModeration,
) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        let _ = app.update(cx, |app, cx| {
            app.send(
                Command::ModerateVoice {
                    user_id: user_id.clone(),
                    action: action.clone(),
                },
                None,
                cx,
            );
        });
    })
}

fn speaking_badge(cx: &App) -> impl IntoElement {
    div()
        .id("speaking")
        .role(Role::Image)
        .aria_label("Speaking")
        .px_1()
        .rounded_sm()
        .bg(cx.theme().popover)
        .child(themed_icon(IconName::Volume2, cx.theme().success, 14.0))
}

fn radio_badge(cx: &App) -> impl IntoElement {
    div()
        .id("radio-listening")
        .role(Role::Image)
        .aria_label("Listening to Radio")
        .child(themed_icon(IconName::Radio, cx.theme().primary, 14.0))
}

fn participant_status(muted: bool, deafened: bool, cx: &App) -> Option<impl IntoElement> {
    let (icon, label) = if deafened {
        (IconName::HeadphoneOff, "Deafened")
    } else if muted {
        (IconName::MicOff, "Muted")
    } else {
        return None;
    };
    Some(
        div()
            .id("voice-state")
            .role(Role::Image)
            .aria_label(label)
            .child(themed_icon(icon, cx.theme().danger, 14.0)),
    )
}
