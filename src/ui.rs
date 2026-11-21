mod settings;
mod voice;

use std::sync::{Arc, LazyLock};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, Selectable, Sizable,
        avatar::Avatar,
        button::{Button, ButtonVariants},
        label::Label,
    },
    prelude::FluentBuilder,
    *,
};

use crate::{
    Spk,
    api::{Channel, Connection},
    session::{Command, Participant, Sidebar, VoiceView},
    voice::Status,
};

pub use settings::{Appearance, ShortcutKeys};
pub use voice::VoiceControls;

pub fn logo(size: f32) -> Img {
    static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../assets/logo.svg").to_vec(),
        ))
    });
    img(Arc::clone(&LOGO)).size(px(size)).rounded_md()
}

fn profile_avatar(
    fallback: Avatar,
    photo: Option<&Arc<RenderImage>>,
    diameter: f32,
    speaking: bool,
    cx: &App,
) -> AnyElement {
    let size = px(diameter);
    let theme = cx.theme();
    let Some(photo) = photo else {
        return fallback
            .size(size)
            .when(speaking, |avatar| avatar.border_color(theme.success))
            .into_any_element();
    };

    div()
        .size(size)
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .bg(theme.tokens.secondary)
        .border_1()
        .border_color(if speaking {
            theme.success
        } else {
            theme.border
        })
        .child(
            img(Arc::clone(photo))
                .id(("avatar-photo", photo.id.0))
                .size_full()
                .rounded_full()
                .object_fit(ObjectFit::Cover),
        )
        .into_any_element()
}

fn tool_button(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    label: impl Into<SharedString>,
) -> Button {
    let label = label.into();
    Button::new(id)
        .ghost()
        .small()
        .icon(icon.into())
        .accessibility_label(label.clone())
        .tooltip(label)
        .tab_stop(true)
}

fn themed_icon(icon: IconName, color: Hsla, size: f32) -> Icon {
    Icon::new(icon).size(px(size)).text_color(color)
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

fn call_status(call: &VoiceView) -> &'static str {
    match call.status {
        Status::Joining => "Joining…",
        Status::Connected => "Connected",
        Status::ListenOnly => "Listen only",
        Status::Reconnecting => "Reconnecting voice…",
        Status::Leaving => "Leaving…",
    }
}

fn channel_name(channel: &Channel) -> &str {
    channel.name.as_deref().unwrap_or("Unnamed channel")
}

fn voice_channels(sidebar: &Sidebar) -> impl Iterator<Item = &Channel> {
    sidebar
        .channels
        .iter()
        .flatten()
        .filter(|channel| channel.kind == "voice")
}

impl Spk {
    pub fn workspace(&self, sidebar: &Sidebar, cx: &mut Context<Self>) -> impl IntoElement {
        let room = voice_channels(sidebar)
            .find(|channel| Some(channel.id.as_str()) == self.selected_room.as_deref())
            .or_else(|| voice_channels(sidebar).next());
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
            sidebar.device_notice,
            self.form_error,
        ];
        let rejoin = sidebar.rejoin_channel().map(str::to_owned);

        div()
            .size_full()
            .flex()
            .when(self.sidebar_open, |page| {
                page.child(self.navigation(sidebar, room, cx))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h_12()
                            .flex_shrink_0()
                            .px_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                tool_button(
                                    "toggle-sidebar",
                                    if self.sidebar_open {
                                        IconName::PanelLeftClose
                                    } else {
                                        IconName::PanelLeftOpen
                                    },
                                    if self.sidebar_open {
                                        "Hide sidebar"
                                    } else {
                                        "Show sidebar"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.sidebar_open = !this.sidebar_open;
                                        this.device_picker = None;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(themed_icon(IconName::Volume2, theme.muted_foreground, 16.0))
                            .child(
                                Label::new(room.map_or("Voice channels", channel_name).to_owned())
                                    .font_semibold(),
                            ),
                    )
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
                                                this.join_room(&id, cx)
                                            })),
                                    )
                                }),
                        )
                    })
                    .child(self.room_stage(sidebar, room, cx)),
            )
    }

    fn navigation(
        &self,
        sidebar: &Sidebar,
        room: Option<&Channel>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let selected = room.map(|room| room.id.as_str());
        let people = sidebar.people();
        let has_text = sidebar
            .channels
            .iter()
            .flatten()
            .any(|channel| channel.kind == "text");
        let name = sidebar.profile.name();
        let online = matches!(sidebar.connection, Connection::Online);

        div()
            .flex()
            .flex_col()
            .w(relative(0.20))
            .min_w(px(270.0))
            .max_w(px(378.0))
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .px_2()
            .pb_2()
            .gap_2()
            .child(
                div()
                    .id("channel-navigation")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_3()
                    .child(
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
                    .when(!people.is_empty(), |nav| {
                        nav.child(
                            div()
                                .flex_shrink_0()
                                .child(section_title(
                                    "Direct messages",
                                    IconName::MessageCircle,
                                    cx,
                                ))
                                .children(people.into_iter().map(|(id, name)| {
                                    div()
                                        .id(SharedString::from(format!("person-{id}")))
                                        .flex()
                                        .items_center()
                                        .h_8()
                                        .gap_2()
                                        .px_2()
                                        .child(profile_avatar(
                                            Avatar::new().name(name.to_owned()).small(),
                                            sidebar.avatar(Some(id)),
                                            20.0,
                                            false,
                                            cx,
                                        ))
                                        .child(Label::new(name.to_owned()).truncate())
                                })),
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
                                            div()
                                                .id(SharedString::from(format!(
                                                    "text-{}",
                                                    channel.id
                                                )))
                                                .flex()
                                                .items_center()
                                                .h_8()
                                                .px_2()
                                                .child(
                                                    Label::new(channel_name(channel).to_owned())
                                                        .truncate(),
                                                )
                                        }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(section_title("Voice channels", IconName::Volume2, cx))
                            .children(
                                voice_channels(sidebar).map(|channel| {
                                    self.channel_row(sidebar, channel, selected, cx)
                                }),
                            ),
                    ),
            )
            .children(sidebar.voice.as_ref().map(|call| {
                let room_name = voice_channels(sidebar)
                    .find(|channel| channel.id == call.channel_id)
                    .map_or("Voice", channel_name);
                let id = call.channel_id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .child(
                        tool_button(
                            "current-call",
                            IconName::Volume2,
                            "Show current voice channel",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.select_room(&id, cx))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(room_name.to_owned()).font_semibold().truncate())
                            .child(
                                Label::new(call_status(call))
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .children(self.leave_voice("sidebar-leave", call, false, cx))
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .child(
                        div()
                            .id("footer-avatar")
                            .relative()
                            .flex_shrink_0()
                            .child(profile_avatar(
                                Avatar::new().name(name.to_owned()).small(),
                                sidebar.avatar(sidebar.profile.id.as_deref()),
                                28.0,
                                false,
                                cx,
                            ))
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .size_2()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme.background)
                                    .bg(if online {
                                        theme.success
                                    } else {
                                        theme.muted_foreground
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .ml_1()
                            .child(Label::new(name.to_owned()).font_semibold().truncate())
                            .child(
                                Label::new(if online { "Online" } else { "Offline" })
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(self.voice_controls(sidebar, VoiceControls::Profile, cx))
                    .child(
                        tool_button("settings", IconName::Settings, "Settings").on_click(
                            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                        ),
                    ),
            )
    }

    fn channel_row(
        &self,
        sidebar: &Sidebar,
        channel: &Channel,
        selected: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let call = sidebar
            .voice
            .as_ref()
            .filter(|call| call.channel_id == channel.id);
        let active = selected == Some(channel.id.as_str());
        let id = channel.id.clone();
        let join_id = channel.id.clone();
        let name = channel_name(channel).to_owned();
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
                            .on_click(cx.listener(move |this, _, _, cx| this.select_room(&id, cx))),
                    )
                    .children(call.and_then(|call| self.leave_voice("leave", call, false, cx)))
                    .when(
                        sidebar.voice.is_none() && matches!(sidebar.connection, Connection::Online),
                        |row| {
                            row.child(
                                tool_button("join", IconName::Phone, format!("Join {name}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.join_room(&join_id, cx)
                                    })),
                            )
                        },
                    ),
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
                    .children(participant_status(flags.muted, flags.deafened, cx))
            }))
    }

    fn select_room(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selected_room = Some(id.to_owned());
        self.device_picker = None;
        cx.notify();
    }

    fn join_room(&mut self, id: &str, cx: &mut Context<Self>) {
        self.select_room(id, cx);
        self.send(Command::JoinVoice(id.to_owned()), None, cx);
    }

    fn room_stage(
        &self,
        sidebar: &Sidebar,
        room: Option<&Channel>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let call = room.and_then(|room| {
            sidebar
                .voice
                .as_ref()
                .filter(|call| call.channel_id == room.id)
        });
        let in_voice = call.is_some_and(|call| {
            matches!(
                call.status,
                Status::Connected | Status::ListenOnly | Status::Reconnecting
            )
        });
        let mut participants = room.map_or_else(Vec::new, |room| sidebar.participants(&room.id));
        if let Some(id) = sidebar.profile.id.as_deref().filter(|_| in_voice)
            && !participants.iter().any(|person| person.id == id)
        {
            // The local media can connect before SPK echoes our room presence.
            participants.insert(
                0,
                Participant {
                    id,
                    name: sidebar.profile.name(),
                    flags: Default::default(),
                },
            );
        }
        let message = match (room, sidebar.channels.is_some()) {
            (None, false) if !sidebar.load_failed => Some("Loading voice channels…"),
            (None, true) => Some("No voice channels"),
            (Some(_), _) if participants.is_empty() => Some("Nobody is here yet."),
            _ => None,
        };
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
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .p_4()
                            .my_auto()
                            .children(message.map(|message| {
                                Label::new(message).text_color(theme.muted_foreground)
                            }))
                            .children(
                                participants
                                    .into_iter()
                                    .map(|person| self.voice_tile(person, call, sidebar, cx)),
                            ),
                    ),
            )
            .when_some(room, |stage, room| {
                let id = room.id.clone();
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
                                    bar.bg(theme.secondary).p_2().child(self.voice_controls(
                                        sidebar,
                                        VoiceControls::Room,
                                        cx,
                                    ))
                                })
                                .children(call.and_then(|call| {
                                    self.leave_voice("room-leave", call, true, cx)
                                }))
                                .when_some(
                                    call.filter(|call| {
                                        matches!(
                                            call.status,
                                            Status::Reconnecting | Status::Leaving
                                        )
                                    }),
                                    |bar, call| bar.child(call_status(call)),
                                )
                                .when(
                                    sidebar.voice.is_none()
                                        && matches!(sidebar.connection, Connection::Online),
                                    |bar| {
                                        bar.child(
                                            Button::new("room-join")
                                                .primary()
                                                .small()
                                                .tab_stop(true)
                                                .icon(IconName::Phone)
                                                .label("Join voice")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.join_room(&id, cx)
                                                })),
                                        )
                                    },
                                ),
                        ),
                )
            })
    }

    fn leave_voice(
        &self,
        id: &'static str,
        call: &VoiceView,
        labeled: bool,
        cx: &Context<Self>,
    ) -> Option<Button> {
        let (icon, label) = match call.status {
            Status::Joining => (IconName::X, "Cancel join"),
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
        cx: &App,
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
        div()
            .id(SharedString::from(participant.id.to_owned()))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .role(Role::Group)
            .when(speaking, |tile| tile.aria_description("Speaking"))
            .w(px(320.0))
            .max_w_full()
            .h(px(180.0))
            .border_1()
            .border_color(if speaking {
                theme.success
            } else {
                theme.border
            })
            .rounded_lg()
            .bg(theme.secondary)
            .child(profile_avatar(
                Avatar::new().name(participant.name.to_owned()),
                sidebar.avatar(Some(participant.id)),
                48.0,
                false,
                cx,
            ))
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
                    .children(participant_status(flags.muted, flags.deafened, cx)),
            )
    }
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
