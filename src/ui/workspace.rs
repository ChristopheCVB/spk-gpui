use super::*;
use crate::{
    api::{Channel, Connection},
    session::{Participant, VoiceView},
    voice::video::CameraStatus,
};

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
    pub(super) fn workspace(&self, sidebar: &Sidebar, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_channel = voice_channels(sidebar)
            .find(|channel| Some(channel.id.as_str()) == self.selected_voice_channel.as_deref())
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
            .when(self.sidebar_open && !self.video_stage.fullscreen, |page| {
                page.child(self.navigation(sidebar, selected_channel, cx))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(!self.video_stage.fullscreen, |area| {
                        area.child(
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
                                    Label::new(
                                        selected_channel
                                            .map_or("Voice channels", channel_name)
                                            .to_owned(),
                                    )
                                    .font_semibold(),
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
                    .child(self.voice_channel_stage(sidebar, selected_channel, cx)),
            )
    }

    fn navigation(
        &self,
        sidebar: &Sidebar,
        selected_channel: Option<&Channel>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let selected = selected_channel.map(|channel| channel.id.as_str());
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
                let switching = sidebar.switching_to().is_some();
                let id = sidebar
                    .switching_to()
                    .unwrap_or(&call.channel_id)
                    .to_owned();
                let voice_channel_name = voice_channels(sidebar)
                    .find(|channel| channel.id == id)
                    .map_or("Voice", channel_name);
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
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_voice_channel(&id, cx)),
                        ),
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
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .children(self.leave_voice("sidebar-leave", call, switching, false, cx))
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_voice_channel(&id, cx)
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
                            cx.listener(move |this, _, _, cx| {
                                this.join_voice_channel(&join_id, cx)
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
                    .children(participant_status(flags.muted, flags.deafened, cx))
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

    fn select_voice_channel(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selected_voice_channel = Some(id.to_owned());
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
        let camera_status =
            call.and_then(|call| call.video.as_ref())
                .and_then(|report| match report.camera {
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
                            .gap_3()
                            .when(theater.is_none(), |tiles| tiles.p_4().my_auto())
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
            call.and_then(|call| call.video.as_ref())
                .is_some_and(|video| matches!(video.camera, CameraStatus::Live(..)))
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
        let image = self.video_image(&participant, is_me);
        div()
            .id(SharedString::from(participant.id.to_owned()))
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
            .bg(theme.secondary)
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
                    .children(participant_status(flags.muted, flags.deafened, cx))
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
