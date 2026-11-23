mod channel_admin;

use gpui_kit::component::{
    Disableable, Placement,
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    resizable::{HandleEdge, resize_handle},
    scroll::ScrollableElement,
    window_paddings,
};

use super::{
    chat::{chat_destination, mark_read_item, unread_badges, unread_label},
    video::{TileActions, TileControls},
    *,
};
use crate::{
    api::{Channel, Connection, VoiceModeration},
    session::{ChannelKind, Participant, VoiceView},
    voice::video::CameraStatus,
};

pub(super) const SIDEBAR_WIDTH: Pixels = px(288.0);
const WIDE_VOICE_SESSION_WIDTH: Pixels = px(320.0);
const VOICE_TILE_BORDER_WIDTH: Pixels = px(2.0);

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

fn navigation_card(title: &'static str, cx: &App) -> Stateful<Div> {
    div()
        .id(title)
        .role(Role::Group)
        .aria_label(title)
        .flex_shrink_0()
        .rounded_lg()
        .p_1()
        .bg(cx.theme().background)
}

pub(super) fn navigation_target(
    id: SharedString,
    scroll_on_focus: Option<&ScrollHandle>,
    window: &mut Window,
    cx: &mut App,
) -> (Stateful<Div>, bool) {
    let focus = window.with_element_namespace(id.clone(), |window| {
        window
            .use_keyed_state("navigation-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(false)
            })
            .read(cx)
            .clone()
    });
    let focused = focus.contains_focused(window, cx);
    let target = div().id(id).track_focus(&focus).when_some(
        scroll_on_focus.cloned().filter(|_| focused),
        |row, scroll| {
            row.relative()
                .child(reveal_in_vertical_scroll(scroll, px(0.)))
        },
    );
    (target, focused)
}

fn vertical_reveal_delta(
    bounds: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    outset: Pixels,
) -> Pixels {
    let bounds = bounds.dilate(outset);
    if bounds.top() < viewport.top() || bounds.size.height > viewport.size.height {
        viewport.top() - bounds.top()
    } else {
        (viewport.bottom() - bounds.bottom()).min(px(0.))
    }
}

fn reveal_in_vertical_scroll(scroll: ScrollHandle, outset: Pixels) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let delta = vertical_reveal_delta(bounds, scroll.bounds(), outset);
            if delta != px(0.) {
                let offset = scroll.offset() + point(px(0.), delta);
                window.on_next_frame(move |window, _| {
                    scroll.set_offset(offset);
                    window.refresh();
                });
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

#[cfg(test)]
mod tests {
    use super::{SPEAKING_RING_WIDTH, VOICE_TILE_BORDER_WIDTH, vertical_reveal_delta};
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn tile_scroll_reveals_the_speaking_ring_at_both_edges() {
        let viewport = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(200.)));
        let outset = VOICE_TILE_BORDER_WIDTH + SPEAKING_RING_WIDTH;
        for (top, delta) in [(2., 2.), (98., -2.), (4., 0.), (96., 0.)] {
            let bounds = Bounds::new(point(px(2.), px(top)), size(px(196.), px(100.)));
            assert_eq!(vertical_reveal_delta(bounds, viewport, outset), px(delta));
        }
    }
}

enum TileLayout<'a> {
    Theater,
    Strip(Pixels),
    Grid(Pixels, Option<&'a ScrollHandle>),
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
            window.open_sheet_at(Placement::Left, cx, move |sheet, window, cx| {
                let drawer = app.update(cx, |this, cx| {
                    let View::SignedIn(sidebar) = &this.view else {
                        return None;
                    };
                    Some((
                        this.navigation(
                            sidebar,
                            this.selected_voice_destination(sidebar),
                            window,
                            cx,
                        )
                        .into_any_element(),
                        this.navigation_footer(sidebar, SIDEBAR_WIDTH, cx)
                            .into_any_element(),
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
                        // Fill the sheet body so the shared navigation owns the scroll position.
                        sheet
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .flex_col()
                                    .child(navigation),
                            )
                            .footer(footer)
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
        let dialog_focus = if window.has_active_dialog(cx) {
            window.focused(cx)
        } else {
            None
        };
        window.close_sheet(cx);
        if let Some(focus) = dialog_focus {
            focus.focus(window, cx);
        }
        cx.notify();
        true
    }

    pub(super) fn selected_voice_destination<'a>(
        &self,
        sidebar: &'a Sidebar,
    ) -> Option<&'a Channel> {
        voice_channels(sidebar)
            .find(|channel| Some(channel.id.as_str()) == self.selected_voice_channel.as_deref())
            .or_else(|| voice_channels(sidebar).next())
    }

    pub(super) fn workspace(
        &self,
        sidebar: &Sidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected_channel = self.selected_voice_destination(sidebar);
        let wide = wide_workspace(window);
        let inline_sidebar = wide && self.sidebar_open && !self.video_stage.fullscreen;
        let chat = sidebar
            .chat
            .as_ref()
            .filter(|_| !self.video_stage.fullscreen);
        let stage_has_controls = chat.is_none()
            && sidebar.voice.as_ref().is_some_and(|call| {
                call.is_participating()
                    && selected_channel.is_some_and(|channel| channel.id == call.channel_id)
            });
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
        let navigation =
            inline_sidebar.then(|| self.navigation(sidebar, selected_channel, window, cx));
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
        let rejoin = sidebar
            .rejoin_channel()
            .filter(|id| chat.is_some() || selected_channel.is_none_or(|channel| channel.id != *id))
            .map(str::to_owned);

        div()
            .id("workspace")
            .size_full()
            .relative()
            .flex()
            .when_some(navigation, |page, navigation| {
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
                        .child(navigation)
                        .child(div().px_2().pb_2().child(self.navigation_footer(
                            sidebar,
                            self.sidebar_width,
                            cx,
                        )))
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
                                .when(stage_has_controls, |header| {
                                    header.child(super::voice::connection_diagnostics(cx))
                                })
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
                                .child(control_button("refresh").small().label("Retry").on_click(
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
                                        control_button("voice-rejoin")
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
                        Some(chat) => self
                            .chat_stage(sidebar, chat, window, cx)
                            .into_any_element(),
                        None => self
                            .voice_channel_stage(sidebar, selected_channel, window, cx)
                            .into_any_element(),
                    })
                    .when(
                        !inline_sidebar
                            && !self.sidebar_drawer_open
                            && !self.video_stage.fullscreen
                            && !stage_has_controls,
                        |area| {
                            area.when_some(sidebar.voice.as_ref(), |area, call| {
                                let (label, action) = super::video::screen_share_action(call);
                                area.children(self.screen_capture_status(call, sidebar).map(
                                    |panel| {
                                        let attempt = match action {
                                            Command::StopScreenShare(attempt) => Some(attempt),
                                            _ => None,
                                        };
                                        panel.when_some(attempt, |panel, attempt| {
                                            panel.child(
                                                control_button("dismiss-screen-capture")
                                                    .small()
                                                    .tab_stop(true)
                                                    .label(label)
                                                    .on_click(cx.listener(move |app, _, _, cx| {
                                                        app.send(
                                                            Command::StopScreenShare(attempt),
                                                            None,
                                                            cx,
                                                        );
                                                    })),
                                            )
                                        })
                                    },
                                ))
                                .child(self.persistent_voice_session(sidebar, call, None, cx))
                            })
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let focused = window.focused(cx);
        let (scroll, focus_changed) = window
            .use_keyed_state("navigation-scroll", cx, |_, _| {
                (ScrollHandle::new(), None::<FocusHandle>)
            })
            .update(cx, |(scroll, previous_focus), _| {
                let changed = *previous_focus != focused;
                *previous_focus = focused;
                (scroll.clone(), changed)
            });
        let scroll_on_focus =
            (focus_changed && window.last_input_was_keyboard()).then_some(&scroll);
        let selected = if sidebar.chat.is_none() {
            selected_channel.map(|channel| channel.id.as_str())
        } else {
            None
        };
        let people = sidebar.people();
        let text = sidebar.channels_of_kind(ChannelKind::Text);
        let voice = sidebar.channels_of_kind(ChannelKind::Voice);
        let text_count = text.clone().count();
        let voice_count = voice.clone().count();
        let voice_card = navigation_card("Voice channels", cx)
            .child(
                navigation_target("voice-heading".into(), scroll_on_focus, window, cx)
                    .0
                    .child(self.channel_section_title(sidebar, ChannelKind::Voice, cx)),
            )
            .children(voice.enumerate().map(|(index, channel)| {
                let actions = sidebar.is_admin().then(|| {
                    self.channel_actions(
                        channel,
                        ChannelKind::Voice,
                        index > 0,
                        index + 1 < voice_count,
                        cx,
                    )
                    .into_any_element()
                });
                let target = navigation_target(
                    format!("voice-{}", channel.id).into(),
                    scroll_on_focus,
                    window,
                    cx,
                );
                self.channel_row(sidebar, channel, selected, actions, target, cx)
            }));
        let primary = cx.theme().primary;

        let destinations = div()
            .id("channel-navigation")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .px_2()
            .pb_2()
            .gap(if self.compact() { px(4.0) } else { px(8.0) })
            .when(!self.sidebar_drawer_open, |nav| {
                nav.child(
                    div()
                        .flex()
                        .h_10()
                        .items_center()
                        .justify_center()
                        .flex_shrink_0()
                        .gap_1()
                        .text_2xl()
                        .font_bold()
                        .text_color(primary)
                        .child(logo(26.0))
                        .child("spk"),
                )
            })
            .child(
                navigation_card("Direct messages", cx)
                    .child(section_title(
                        "Direct messages",
                        IconName::MessageCircle,
                        cx,
                    ))
                    .children(people.into_iter().map(|(id, name)| {
                        navigation_target(format!("dm-{id}").into(), scroll_on_focus, window, cx)
                            .0
                            .child(self.person_row(sidebar, id, name, cx))
                    })),
            )
            .child(
                navigation_card("Text channels", cx)
                    .child(
                        navigation_target("text-heading".into(), scroll_on_focus, window, cx)
                            .0
                            .child(self.channel_section_title(sidebar, ChannelKind::Text, cx)),
                    )
                    .children(text.enumerate().map(|(index, channel)| {
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
                        navigation_target(format!("text-{id}").into(), scroll_on_focus, window, cx)
                            .0
                            .flex()
                            .items_center()
                            .pr_1()
                            .child(
                                control_button("select")
                                    .ghost()
                                    .small()
                                    .tab_stop(true)
                                    .selected(active)
                                    .accessibility_label(unread_label(&name, counts))
                                    .flex_1()
                                    .min_w_0()
                                    .h_8()
                                    .when(active || counts.mentions > 0, |button| {
                                        button.bg(primary.opacity(0.10))
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
                                                    .when(active, |label| label.text_color(primary))
                                                    .when(counts.unread > 0, |label| {
                                                        label.font_bold()
                                                    }),
                                            )
                                            .child(unread_badges(counts, cx)),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.close_sidebar_drawer(window, cx);
                                        this.device_picker = None;
                                        this.send(Command::OpenTextChannel(id.clone()), None, cx);
                                    })),
                            )
                            .children(sidebar.is_admin().then(|| {
                                self.channel_actions(
                                    channel,
                                    ChannelKind::Text,
                                    index > 0,
                                    index + 1 < text_count,
                                    cx,
                                )
                            }))
                            .context_menu(move |menu, _, _| {
                                menu.item(mark_read_item(menu_id.clone(), app.clone()))
                                    .item(notifications::mute_item(
                                        menu_id.clone(),
                                        muted,
                                        app.clone(),
                                    ))
                            })
                    })),
            )
            .child(voice_card)
            .when(sidebar.is_admin(), |nav| {
                nav.children(channel_admin::channel_status(
                    sidebar.channel_admin.result,
                    cx,
                ))
            });
        div()
            .id("navigation-viewport")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(destinations)
            .vertical_scrollbar(&scroll)
    }

    fn navigation_footer(
        &self,
        sidebar: &Sidebar,
        width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement {
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
                    .map(|call| self.persistent_voice_session(sidebar, call, Some(width), cx)),
            )
            .child(
                div()
                    .id("profile-card")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .child(profile::profile_status_avatar(sidebar, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_start()
                            .flex_1()
                            .min_w_0()
                            .child(
                                Label::new(name.to_owned())
                                    .w_full()
                                    .font_semibold()
                                    .truncate(),
                            )
                            .child(
                                Label::new(sidebar.user_status.label())
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(self.voice_controls(sidebar, VoiceControls::Profile, cx))
                    .child(
                        tool_button("settings", IconName::Settings, "Settings")
                            .track_focus(&self.settings_button_focus)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.settings_button_focus.focus(window, cx);
                                this.open_settings(window, cx);
                            })),
                    ),
            )
    }

    fn persistent_voice_session(
        &self,
        sidebar: &Sidebar,
        call: &VoiceView,
        sidebar_width: Option<Pixels>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let location = if sidebar_width.is_some() {
            VoiceControls::Session
        } else {
            VoiceControls::BottomBar
        };
        let bottom_bar = location == VoiceControls::BottomBar;
        let stacked = sidebar_width.is_some_and(|width| width < WIDE_VOICE_SESSION_WIDTH);
        let switching = sidebar.switching_to().is_some();
        let status = call_status(call, switching);
        let id = sidebar
            .switching_to()
            .unwrap_or(&call.channel_id)
            .to_owned();
        let voice_channel_name = voice_channels(sidebar)
            .find(|channel| channel.id == id)
            .map_or("Voice", channel_name)
            .to_owned();
        let show_capture = !bottom_bar
            && (self.sidebar_drawer_open
                || sidebar.chat.is_some()
                || self
                    .selected_voice_destination(sidebar)
                    .is_none_or(|channel| channel.id != call.channel_id));
        let capture_status = show_capture
            .then(|| self.screen_capture_status(call, sidebar))
            .flatten();
        let channel_color = if matches!(call.status, Status::Connected | Status::ListenOnly) {
            theme.success
        } else {
            theme.muted_foreground
        };
        let row = div()
            .id("voice-session")
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .p_2()
            .bg(theme.secondary)
            .when(bottom_bar, |bar| {
                bar.border_t_1().border_color(theme.border)
            })
            .when(!bottom_bar, |card| card.rounded_lg())
            .when(stacked, |card| card.flex_col())
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .when(stacked, |details| {
                        details.w_full().items_center().justify_between().gap_1()
                    })
                    .when(!stacked, |details| {
                        details.flex_1().flex_col().items_start()
                    })
                    .child(
                        control_button("current-call")
                            .ghost()
                            .small()
                            .tab_stop(true)
                            .accessibility_label(format!("Show {voice_channel_name} voice channel"))
                            .min_w_0()
                            .max_w_full()
                            .flex_shrink_1()
                            .justify_start()
                            .child(
                                control_content()
                                    .child(
                                        Icon::new(IconName::Volume2)
                                            .size(ACTION_ICON_SIZE)
                                            .text_color(channel_color),
                                    )
                                    .child(
                                        Label::new(voice_channel_name)
                                            .min_w_0()
                                            .font_semibold()
                                            .truncate(),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_sidebar_drawer(window, cx);
                                this.select_voice_channel(&id, cx);
                            })),
                    )
                    .child(super::voice::connection_diagnostics_with_status(status, cx)),
            )
            .when(
                call.is_participating() || call.status == Status::Joining || switching,
                |card| {
                    card.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .flex_wrap()
                            .when(call.is_participating(), |actions| {
                                actions
                                    .when(bottom_bar, |bar| {
                                        bar.child(self.voice_controls(sidebar, location, cx))
                                    })
                                    .when(!bottom_bar, |card| {
                                        card.child(self.camera_controls(sidebar, location, cx))
                                            .child(self.screen_controls(call, location, cx))
                                    })
                                    .children(self.voice_media_buttons(sidebar, location, cx))
                            })
                            .children(self.leave_voice(
                                "persistent-voice-leave",
                                call,
                                switching,
                                bottom_bar,
                                cx,
                            )),
                    )
                },
            );
        div().flex().flex_col().children(capture_status).child(row)
    }

    fn channel_row(
        &self,
        sidebar: &Sidebar,
        channel: &Channel,
        selected: Option<&str>,
        actions: Option<impl IntoElement>,
        (row, focused): (Stateful<Div>, bool),
        cx: &Context<Self>,
    ) -> Stateful<Div> {
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
        let participants = sidebar.participants(&id);
        let participant_count = participants.len();
        let join_label = if sidebar.voice.is_some() {
            format!("Switch to {name}")
        } else {
            format!("Join {name}")
        };
        div()
            .id(SharedString::from(format!("voice-{id}")))
            .child(
                row.group("voice-channel")
                    .flex()
                    .items_center()
                    .h_8()
                    .rounded_md()
                    .pr_1()
                    .when(active, |row| row.bg(theme.primary.opacity(0.10)))
                    .when(!active && focused, |row| row.bg(theme.accent))
                    .when(!active, |row| row.hover(|row| row.bg(theme.accent)))
                    .child(
                        control_button("select")
                            .ghost()
                            .small()
                            .tab_stop(true)
                            .selected(active)
                            .accessibility_label(name.clone())
                            .flex_1()
                            .min_w_0()
                            .h_8()
                            .bg(theme.transparent)
                            .child(Label::new(name).w_full().truncate().text_color(if active {
                                theme.primary
                            } else {
                                theme.foreground
                            }))
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
                        row.child(
                            div()
                                .id("join-action")
                                .flex_shrink_0()
                                .when(!focused, |action| {
                                    action
                                        .opacity(0.)
                                        .group_hover("voice-channel", |style| style.opacity(1.))
                                })
                                .child(tool_button("join", IconName::Phone, join_label).on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.close_sidebar_drawer(window, cx);
                                        this.join_voice_channel(&join_id, cx);
                                    }),
                                )),
                        )
                    })
                    .children(actions),
            )
            .children(participants.into_iter().enumerate().map(|(index, person)| {
                let flags = person.flags;
                let speaking = flags.speaking && !flags.muted && !flags.deafened;
                let presence = sidebar.user_presence(person.id);
                let presence_color = super::profile::user_presence_color(presence, cx);
                let last = index + 1 == participant_count;
                div()
                    .id(SharedString::from(format!("member-{}", person.id)))
                    .role(Role::Group)
                    .aria_label(person.name.to_owned())
                    .when(speaking, |row| row.aria_description("Speaking"))
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_7()
                    .pr_1()
                    .h(CONTROL_HEIGHT)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_4()
                            .w_2()
                            .h_4()
                            .rounded_bl_sm()
                            .border_l_1()
                            .border_b_1()
                            .border_color(theme.muted_foreground.opacity(0.35)),
                    )
                    .when(!last, |row| {
                        row.child(
                            div()
                                .absolute()
                                .top_4()
                                .bottom_0()
                                .left_4()
                                .border_l_1()
                                .border_color(theme.muted_foreground.opacity(0.35)),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .child(profile_avatar(
                                Avatar::new().name(person.name.to_owned()).xsmall(),
                                sidebar.avatar(Some(person.id)),
                                16.0,
                                speaking,
                                cx,
                            ))
                            .child(
                                div()
                                    .id("user-presence")
                                    .role(Role::Image)
                                    .aria_label(presence.label())
                                    .absolute()
                                    .top(px(-1.0))
                                    .right_0()
                                    .size_1p5()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme.background)
                                    .bg(presence_color),
                            ),
                    )
                    .child(
                        Label::new(person.name.to_owned())
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground),
                    )
                    .children(participant_status(flags.muted, flags.deafened, cx))
                    .when(flags.radio_listening == Some(true), |row| {
                        row.child(radio_badge(cx))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_shrink_0()
                            .gap_1()
                            .when(
                                sidebar.profile.id.as_deref() != Some(person.id)
                                    && call.is_some_and(|call| {
                                        call.channel_id == channel.id && call.is_participating()
                                    }),
                                |actions| {
                                    actions
                                        .child(self.participant_volume_button(&person, sidebar, cx))
                                },
                            )
                            .children(voice_moderation_menu(&person, sidebar, cx)),
                    )
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
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let call = sidebar
            .voice
            .as_ref()
            .filter(|call| selected_channel.is_some_and(|channel| call.channel_id == channel.id));
        let joined = call.filter(|call| call.is_participating());
        let switching = selected_channel
            .is_some_and(|channel| sidebar.switching_to() == Some(channel.id.as_str()));
        let departing = if switching {
            sidebar.voice.as_ref()
        } else {
            call
        };
        let participants = joined.map_or_else(
            || selected_channel.map_or_else(Vec::new, |channel| sidebar.participants(&channel.id)),
            |call| super::video::joined_participants(sidebar, call),
        );
        let message = match (selected_channel, sidebar.channels.is_some()) {
            (None, false) if !sidebar.load_failed => Some("Loading voice channels…"),
            (None, true) => Some("No voice channels"),
            (Some(_), _) if participants.is_empty() => Some("Nobody is here yet."),
            _ => None,
        };
        let stage = &self.video_stage;
        let focused = window.focused(cx);
        let focus_changed = stage.grid_focus.replace(focused.clone()) != focused;
        let scroll_on_focus =
            (focus_changed && window.last_input_was_keyboard()).then_some(&stage.grid_scroll);
        let camera_status = joined.and_then(|_| match sidebar.camera_status() {
            CameraStatus::Starting => Some("Starting Camera…"),
            CameraStatus::Failed(message) => Some(message),
            _ => None,
        });
        let fullscreen = self.video_stage.fullscreen;
        let gap = if self.compact() { px(6.0) } else { px(12.0) };
        let theater = self.video_stage.theater.as_deref().and_then(|id| {
            joined.and_then(|call| {
                let focused = participants.iter().find(|person| person.id == id)?;
                Some((focused, call))
            })
        });
        let show_strip = !fullscreen && !self.video_stage.participants_hidden;
        let strip_width = if window.viewport_size().height < px(500.0) {
            px(144.0)
        } else {
            px(192.0)
        };
        let tile_width = if window.viewport_size().width < px(640.0) {
            px(244.0)
        } else {
            px(320.0)
        };

        div()
            .id("voice-stage")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when_some(
                departing.filter(|call| !fullscreen && call.status != Status::Connected),
                |stage, call| {
                    stage.child(
                        div()
                            .id("stage-connection-status")
                            .role(Role::Status)
                            .text_center()
                            .flex_shrink_0()
                            .px_3()
                            .py_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(call_status(call, switching)),
                    )
                },
            )
            .child(if let Some((focused, call)) = theater {
                let others = || participants.iter().filter(|person| person.id != focused.id);
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .when(!fullscreen, |tiles| tiles.p_2().gap(gap))
                    .when(fullscreen, |tiles| tiles.p(SPEAKING_RING_WIDTH))
                    .child(div().flex_1().min_h_0().child(self.voice_tile(
                        focused,
                        call,
                        sidebar,
                        TileLayout::Theater,
                        window,
                        cx,
                    )))
                    .when(!fullscreen && participants.len() > 1, |stage| {
                        let label = if show_strip {
                            "Hide participants"
                        } else {
                            "Show participants"
                        };
                        stage.child(
                            tool_button_with_label(
                                "voice-strip-toggle",
                                if show_strip {
                                    IconName::PanelBottom
                                } else {
                                    IconName::PanelBottomOpen
                                },
                                label,
                                true,
                            )
                            .self_center()
                            .flex_shrink_0()
                            .track_focus(&self.voice_strip_toggle_focus)
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.voice_strip_toggle_focus.focus(window, cx);
                                    this.video_stage.participants_hidden =
                                        !this.video_stage.participants_hidden;
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(show_strip && participants.len() > 1, |stage| {
                        self.scroll_to_focused_participant(
                            others().map(|person| person.id),
                            window,
                            cx,
                        );
                        stage.child(
                            div()
                                .id("voice-participant-strip")
                                .role(Role::Group)
                                .aria_label("Voice participants")
                                .flex()
                                .flex_shrink_0()
                                .gap(gap - SPEAKING_RING_WIDTH * 2.)
                                .px(px(4.) - SPEAKING_RING_WIDTH)
                                .py_1()
                                .overflow_x_scroll()
                                .track_scroll(&self.video_stage.participant_scroll)
                                .children(others().enumerate().map(|(index, person)| {
                                    div()
                                        .flex()
                                        .flex_shrink_0()
                                        .max_w_full()
                                        .px(SPEAKING_RING_WIDTH)
                                        .when(index == 0, |slot| slot.ml_auto())
                                        .when(index + 2 == participants.len(), |slot| {
                                            slot.mr_auto()
                                        })
                                        .child(self.voice_tile(
                                            person,
                                            call,
                                            sidebar,
                                            TileLayout::Strip(strip_width),
                                            window,
                                            cx,
                                        ))
                                })),
                        )
                    })
                    .into_any_element()
            } else {
                div()
                    .id("voice-tiles")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&stage.grid_scroll)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_center()
                            .my_auto()
                            .flex_shrink_0()
                            .gap(gap)
                            .p(if self.compact() { px(8.0) } else { px(16.0) })
                            .children(message.map(|message| {
                                Label::new(message).text_color(theme.muted_foreground)
                            }))
                            .children(participants.iter().map(|person| {
                                if let Some(call) = joined {
                                    self.voice_tile(
                                        person,
                                        call,
                                        sidebar,
                                        TileLayout::Grid(tile_width, scroll_on_focus),
                                        window,
                                        cx,
                                    )
                                    .into_any_element()
                                } else {
                                    self.voice_preview(person, sidebar, cx).into_any_element()
                                }
                            })),
                    )
                    .into_any_element()
            })
            .when_some(camera_status.filter(|_| !fullscreen), |stage, message| {
                stage.child(
                    Label::new(message)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .px_4()
                        .pb_2(),
                )
            })
            .children(
                joined
                    .filter(|_| !fullscreen && !self.sidebar_drawer_open)
                    .and_then(|call| self.screen_capture_status(call, sidebar)),
            )
            .when(!fullscreen, |stage| {
                let footer = if let Some(call) = joined {
                    Some(
                        div()
                            .id("voice-stage-controls")
                            .role(Role::Toolbar)
                            .aria_label("Voice controls")
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .rounded_lg()
                            .bg(theme.sidebar)
                            .p_2()
                            .child(self.voice_controls(sidebar, VoiceControls::Channel, cx))
                            .child(self.camera_controls(sidebar, VoiceControls::Channel, cx))
                            .child(self.screen_controls(call, VoiceControls::Channel, cx))
                            .children(self.voice_media_buttons(sidebar, VoiceControls::Channel, cx))
                            .children(self.leave_voice(
                                "voice-channel-leave",
                                call,
                                false,
                                true,
                                cx,
                            ))
                            .into_any_element(),
                    )
                } else {
                    selected_channel.and_then(|channel| {
                        let label = if sidebar.voice.is_some() {
                            "Switch voice channel"
                        } else if sidebar.rejoin_channel() == Some(channel.id.as_str()) {
                            "Rejoin voice"
                        } else {
                            "Join voice"
                        };
                        let id = channel.id.clone();
                        if sidebar.can_join_voice(&id) {
                            Some(
                                icon_label_button(
                                    control_button("voice-channel-join").primary(),
                                    IconName::Phone,
                                    label,
                                )
                                .tab_stop(true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.join_voice_channel(&id, cx)
                                }))
                                .into_any_element(),
                            )
                        } else {
                            departing
                                .and_then(|call| {
                                    self.leave_voice(
                                        "voice-channel-leave",
                                        call,
                                        switching,
                                        true,
                                        cx,
                                    )
                                })
                                .map(IntoElement::into_any_element)
                        }
                    })
                };
                stage.when_some(footer, |stage, footer| {
                    stage.child(
                        div()
                            .flex()
                            .justify_center()
                            .flex_shrink_0()
                            .px_2()
                            .pb_2()
                            .child(footer),
                    )
                })
            })
    }

    fn voice_preview(
        &self,
        participant: &Participant<'_>,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let flags = participant.flags;
        let speaking = flags.speaking && !flags.muted && !flags.deafened;
        div()
            .id(SharedString::from(participant.id.to_owned()))
            .role(Role::Group)
            .aria_label(participant.name.to_owned())
            .when(speaking, |preview| preview.aria_description("Speaking"))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .w(px(144.0))
            .max_w_full()
            .h(px(110.0))
            .p_3()
            .rounded_lg()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .when(speaking, |preview| {
                preview.shadow(speaking_ring(theme.success))
            })
            .child(profile_avatar(
                Avatar::new().name(participant.name.to_owned()).large(),
                sidebar.avatar(Some(participant.id)),
                48.0,
                false,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .max_w_full()
                    .child(
                        Label::new(participant.name.to_owned())
                            .text_sm()
                            .min_w_0()
                            .truncate(),
                    )
                    .children(participant_status(flags.muted, flags.deafened, cx))
                    .when(flags.radio_listening == Some(true), |row| {
                        row.child(radio_badge(cx))
                    }),
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
            tool_button_with_label(
                id,
                Icon::new(icon).text_color(cx.theme().danger),
                label,
                labeled,
            )
            .when(labeled, |button| button.text_color(cx.theme().danger))
            .on_click(cx.listener(|this, _, _, cx| {
                this.device_picker = None;
                this.send(Command::LeaveVoice, None, cx);
            })),
        )
    }

    fn voice_tile_menu(
        &self,
        participant: &Participant<'_>,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        if sidebar.profile.id.as_deref() == Some(participant.id) {
            return None;
        }

        let destinations = voice_moderation_destinations(sidebar, participant.id);
        let enabled = !sidebar.voice_moderation_pending;
        let id = participant.id.to_owned();
        let name = participant.name.to_owned();
        let menu_id = id.clone();
        let app = cx.weak_entity();
        Some(
            tool_button(
                "tile-menu",
                IconName::Ellipsis,
                format!("Actions for {name}"),
            )
            .secondary()
            .dropdown_menu_with_anchor(Anchor::TopRight, {
                let app = app.clone();
                move |menu, window, cx| {
                    let menu = menu.item(PopupMenuItem::new("Volume…").on_click({
                        let app = app.clone();
                        let id = id.clone();
                        let name = name.clone();
                        move |_, window, cx| {
                            let app = app.clone();
                            let id = id.clone();
                            let name = name.clone();
                            window.defer(cx, move |window, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    this.open_participant_volume(&id, &name, window, cx);
                                });
                            });
                        }
                    }));
                    if let Some(destinations) = &destinations {
                        add_voice_moderation_items(
                            menu.separator(),
                            window,
                            cx,
                            &app,
                            &id,
                            destinations,
                            enabled,
                        )
                    } else {
                        menu
                    }
                }
            })
            .on_open_change(move |open, _, cx| {
                let _ = app.update(cx, |this, cx| {
                    if *open {
                        this.video_stage.menu_for = Some(menu_id.clone());
                    } else if this.video_stage.menu_for.as_deref() == Some(&menu_id) {
                        this.video_stage.menu_for = None;
                    }
                    cx.notify();
                });
            }),
        )
    }

    fn voice_tile(
        &self,
        participant: &Participant<'_>,
        call: &VoiceView,
        sidebar: &Sidebar,
        layout: TileLayout<'_>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        const COMPACT_SCREEN_WIDTH: Pixels = px(244.0);
        let actions = TileActions::for_participant(self, participant, call, sidebar);
        let (tile_width, strip_height, scroll_on_focus) = match layout {
            TileLayout::Theater => (None, None, None),
            TileLayout::Strip(width) if actions.watched_screen => {
                (Some(COMPACT_SCREEN_WIDTH), Some(width * 9.0 / 16.0), None)
            }
            TileLayout::Strip(width) => (Some(width), None, None),
            TileLayout::Grid(width, scroll) => (Some(width), None, scroll),
        };
        let theme = cx.theme();
        let is_me = sidebar.profile.id.as_deref() == Some(participant.id);
        let media = call.media.as_ref();
        let flags = if is_me {
            crate::api::VoiceFlags {
                muted: sidebar.voice_intent.mute.effective_muted()
                    || matches!(call.status, Status::ListenOnly | Status::Reconnecting),
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
        let (image, camera_inset) = self.video_images(participant, is_me);
        let showing_video = image.is_some();
        let hover_id = participant.id.to_owned();
        let key_id = hover_id.clone();
        let tile_focus = &self.video_stage.tile_focus[participant.id];
        let menu = self.voice_tile_menu(participant, sidebar, cx);
        let tile_focused = tile_focus.contains_focused(window, cx);
        let reveal_actions = (tile_focused
            && (!actions.watched_screen || self.video_stage.keyboard_focus))
            || self.video_stage.menu_for.as_deref() == Some(participant.id)
            || self.video_stage.screen_volume_for.as_deref() == Some(participant.id)
            || (actions.watched_screen
                && self.video_stage.pointer_over_window(window)
                && self.video_stage.hovered.as_deref() == Some(participant.id));
        let scroll_on_focus = scroll_on_focus.filter(|_| tile_focused).cloned();
        let focus_on_tile = tile_focus.is_focused(window);
        let controls = self
            .video_tile_controls(
                participant,
                call,
                sidebar,
                TileControls {
                    actions,
                    revealed: reveal_actions,
                    compact: tile_width.is_some_and(|width| width <= COMPACT_SCREEN_WIDTH),
                },
                menu,
                cx,
            )
            .when_some(
                scroll_on_focus.clone().filter(|_| !focus_on_tile),
                |controls, scroll| controls.child(reveal_in_vertical_scroll(scroll, px(0.))),
            );
        let name = if is_me { "You" } else { participant.name };
        div()
            .id(SharedString::from(participant.id.to_owned()))
            .group("voice-tile")
            .track_focus(tile_focus)
            .when(actions.watched_screen, |tile| {
                tile.hover_listener_mode(HoverListenerMode::InputModalityIndependent)
            })
            .focus_visible(|tile| tile.border_color(theme.ring))
            .on_hover(cx.listener(move |app, hovered: &bool, _, cx| {
                if *hovered {
                    app.video_stage.hovered = Some(hover_id.clone());
                } else if app.video_stage.hovered.as_deref() == Some(&hover_id) {
                    app.video_stage.hovered = None;
                }
                cx.notify();
            }))
            .on_key_down(cx.listener(move |app, event, window, cx| {
                app.video_tile_key(&key_id, event, window, cx)
            }))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .role(Role::Group)
            .aria_label(name.to_owned())
            .when(speaking, |tile| tile.aria_description("Speaking"))
            .when_some(tile_width, |tile, width| {
                tile.w(width)
                    .max_w_full()
                    .when_some(strip_height, |tile, height| tile.h(height))
                    .when(strip_height.is_none(), |tile| tile.aspect_ratio(16.0 / 9.0))
                    .flex_shrink_0()
            })
            .when(tile_width.is_none(), |tile| tile.size_full())
            .overflow_hidden()
            .border(VOICE_TILE_BORDER_WIDTH)
            .border_color(theme.sidebar)
            .rounded_lg()
            .bg(theme.sidebar)
            .when(speaking, |tile| tile.shadow(speaking_ring(theme.success)))
            .when(!showing_video, |tile| {
                tile.child(profile_avatar(
                    Avatar::new().name(participant.name.to_owned()),
                    sidebar.avatar(Some(participant.id)),
                    if tile_width.is_some_and(|width| width <= px(192.0)) {
                        32.0
                    } else {
                        48.0
                    },
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
                    .max_w(relative(0.9))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded_md()
                    .when(showing_video, |row| row.bg(theme.background))
                    .child(Label::new(name.to_owned()).text_xs().min_w_0().truncate())
                    .children(participant_status(flags.muted, flags.deafened, cx))
                    .when(flags.radio_listening == Some(true), |row| {
                        row.child(radio_badge(cx))
                    })
                    .when_some(actions.screen_quality, |row, quality| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .border_l_1()
                                .border_color(theme.border)
                                .pl_1()
                                .text_size(px(10.0))
                                .child(format!("{}p · {} fps", quality.height, quality.fps)),
                        )
                    }),
            )
            .child(controls)
            .when_some(scroll_on_focus.filter(|_| focus_on_tile), |tile, scroll| {
                // The absolute canvas starts inside the tile's border.
                tile.child(reveal_in_vertical_scroll(
                    scroll,
                    VOICE_TILE_BORDER_WIDTH + SPEAKING_RING_WIDTH,
                ))
            })
    }
}

fn voice_moderation_destinations(
    sidebar: &Sidebar,
    user_id: &str,
) -> Option<Vec<(String, String)>> {
    Some(
        sidebar
            .voice_moderation_channels(user_id)?
            .into_iter()
            .map(|channel| (channel.id.clone(), channel_name(channel).to_owned()))
            .collect(),
    )
}

fn voice_moderation_menu(
    participant: &Participant<'_>,
    sidebar: &Sidebar,
    cx: &Context<Spk>,
) -> Option<impl IntoElement> {
    let destinations = voice_moderation_destinations(sidebar, participant.id)?;
    let user_id = participant.id.to_owned();
    let app = cx.weak_entity();
    Some(
        tool_button(
            "voice-moderation",
            IconName::Ellipsis,
            format!("Moderate {}", participant.name),
        )
        .disabled(sidebar.voice_moderation_pending)
        .dropdown_menu(move |menu, window, cx| {
            add_voice_moderation_items(menu, window, cx, &app, &user_id, &destinations, true)
        }),
    )
}

fn add_voice_moderation_items(
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
    app: &WeakEntity<Spk>,
    user_id: &str,
    destinations: &[(String, String)],
    enabled: bool,
) -> PopupMenu {
    if enabled && !destinations.is_empty() {
        let app = app.clone();
        let user_id = user_id.to_owned();
        let destinations = destinations.to_vec();
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
    menu.item(
        voice_moderation_item(
            "Disconnect from voice",
            app.clone(),
            user_id.to_owned(),
            VoiceModeration::Disconnect,
        )
        .disabled(!enabled),
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
