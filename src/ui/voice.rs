use super::*;
use crate::{
    session::View,
    shortcuts::Action,
    voice::{Device, DeviceKind, DeviceList},
};
use gpui_kit::component::popover::Popover;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum VoiceControls {
    Profile,
    Channel,
}

fn device_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Microphone => "Microphone",
        DeviceKind::Speakers => "Speakers",
        DeviceKind::MediaOutput => "Media Output",
    }
}

fn mic_needs_retry(sidebar: &Sidebar) -> bool {
    !sidebar.voice_intent.mute.effective_muted()
        && sidebar
            .voice
            .as_ref()
            .is_some_and(|call| call.status == Status::ListenOnly)
}

impl Spk {
    pub(crate) fn voice_action(&mut self, action: Action, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let command = match action {
            Action::Mute if mic_needs_retry(sidebar) => Command::RetryMicrophone,
            Action::Mute => Command::ToggleMute,
            Action::Deafen => Command::ToggleDeafen,
        };
        self.send(command, None, cx);
    }

    pub(super) fn voice_controls(
        &self,
        sidebar: &Sidebar,
        location: VoiceControls,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let mute = sidebar.voice_intent.mute;
        let listen_only = sidebar
            .voice
            .as_ref()
            .is_some_and(|call| call.status == Status::ListenOnly);
        let mic_label = if mute.effective_muted() {
            "Unmute"
        } else if listen_only {
            "Enable microphone"
        } else {
            "Mute"
        };
        let compact = location == VoiceControls::Profile;
        let theme = cx.theme();

        div()
            .id(if compact {
                "profile-voice-controls"
            } else {
                "room-voice-controls"
            })
            .flex()
            .items_center()
            .gap_1()
            .children(
                [
                    (
                        DeviceKind::Microphone,
                        if mute.effective_muted() || listen_only {
                            IconName::MicOff
                        } else {
                            IconName::Mic
                        },
                        mic_label,
                        Action::Mute,
                        mute.effective_muted() || listen_only,
                    ),
                    (
                        DeviceKind::Speakers,
                        if mute.deafened {
                            IconName::HeadphoneOff
                        } else {
                            IconName::Headphones
                        },
                        if mute.deafened { "Undeafen" } else { "Deafen" },
                        Action::Deafen,
                        mute.deafened,
                    ),
                ]
                .into_iter()
                .map(|(kind, icon, label, action, off)| {
                    let picker_label = format!("Choose {}", device_label(kind).to_lowercase());
                    let open = self.device_picker == Some((location, kind));
                    div()
                        .id(device_label(kind))
                        .flex()
                        .items_center()
                        .child(
                            tool_button(
                                "toggle",
                                themed_icon(
                                    icon,
                                    if off { theme.danger } else { theme.foreground },
                                    16.0,
                                ),
                                label,
                            )
                            .when(!compact, |button| button.label(label))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.voice_action(action, cx)),
                            ),
                        )
                        .child({
                            let app = cx.weak_entity();
                            let popup_app = app.clone();
                            Popover::new("device-picker")
                                .anchor(Anchor::BottomRight)
                                .w(px(280.0))
                                .open(open)
                                .trigger(
                                    tool_button(
                                        "devices",
                                        if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronUp
                                        },
                                        picker_label,
                                    )
                                    .w(px(18.0))
                                    .px_0(),
                                )
                                .on_open_change(move |open, _, cx| {
                                    let _ = app.update(cx, |app, cx| {
                                        if *open {
                                            app.device_picker = Some((location, kind));
                                            app.send(Command::RefreshDevices, None, cx);
                                        } else if app.device_picker == Some((location, kind)) {
                                            app.device_picker = None;
                                            cx.notify();
                                        }
                                    });
                                })
                                .content(move |_, _, cx| {
                                    let Some(app) = popup_app.upgrade() else {
                                        return div();
                                    };
                                    let View::SignedIn(sidebar) = &app.read(cx).view else {
                                        return div();
                                    };
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(Label::new(device_label(kind)).font_semibold())
                                        .child(device_options(sidebar, kind, &app, cx))
                                })
                        })
                }),
            )
    }

    pub(super) fn voice_media_buttons(&self, labeled: bool, cx: &Context<Self>) -> [Button; 2] {
        [
            tool_button("radio", IconName::Radio, "Channel Radio")
                .when(labeled, |button| button.label("Radio"))
                .on_click(cx.listener(|this, _, window, cx| this.open_radio(window, cx))),
            tool_button("soundboard", IconName::Megaphone, "Soundboard")
                .when(labeled, |button| button.label("Soundboard"))
                .on_click(cx.listener(|this, _, window, cx| this.open_soundboard(window, cx))),
        ]
    }
}

pub(super) fn device_options(
    sidebar: &Sidebar,
    kind: DeviceKind,
    entity: &Entity<Spk>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (list, active, preferred) = devices(sidebar, kind);
    let selected = list
        .preferred(preferred)
        .filter(|device| Some(*device) == active);
    let entity = entity.downgrade();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .id(device_label(kind))
        .child(
            Label::new(device_status(active, preferred, list))
                .text_xs()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .id("choices")
                .flex()
                .flex_col()
                .gap_1()
                .max_h(px(140.0))
                .overflow_y_scroll()
                .children(
                    std::iter::once(None)
                        .chain(list.choices.iter().map(Some))
                        .enumerate()
                        .map(|(index, device)| {
                            let chosen = device == selected;
                            let name = device
                                .map_or("System default", |device| device.name.as_str())
                                .to_owned();
                            let device = device.cloned();
                            let entity = entity.clone();
                            Button::new(index)
                                .ghost()
                                .small()
                                .tab_stop(true)
                                .selected(chosen)
                                .accessibility_label(name.clone())
                                .when(chosen, |button| button.bg(theme.accent))
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(Label::new(name).truncate())
                                        .when(chosen, |row| {
                                            row.child(themed_icon(
                                                IconName::Check,
                                                theme.primary,
                                                14.0,
                                            ))
                                        }),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |app, cx| {
                                        app.device_picker = None;
                                        app.send(
                                            Command::ChooseDevice(kind, device.clone()),
                                            None,
                                            cx,
                                        );
                                    });
                                })
                        }),
                ),
        )
}

fn devices(sidebar: &Sidebar, kind: DeviceKind) -> (&DeviceList, Option<&Device>, Option<&Device>) {
    let media = sidebar.voice.as_ref().and_then(|call| call.media.as_ref());
    let preferred = &sidebar.voice_intent.preferred;
    match kind {
        DeviceKind::Microphone => {
            let list = &sidebar.devices.microphones;
            let preferred = preferred.microphone.as_ref();
            let active = match media {
                Some(media) if !sidebar.voice_intent.mute.effective_muted() => {
                    media.microphone.as_ref().or_else(|| {
                        if media.status == Status::ListenOnly {
                            None
                        } else {
                            list.resolve(preferred)
                        }
                    })
                }
                _ => list.resolve(preferred),
            };
            (list, active, preferred)
        }
        DeviceKind::Speakers => {
            let list = &sidebar.devices.speakers;
            let preferred = preferred.speakers.as_ref();
            let active =
                media.map_or_else(|| list.resolve(preferred), |media| media.speakers.as_ref());
            (list, active, preferred)
        }
        DeviceKind::MediaOutput => {
            let list = &sidebar.devices.media_outputs;
            let preferred = preferred.media_output.as_ref();
            (list, list.resolve(preferred), preferred)
        }
    }
}

fn device_status(active: Option<&Device>, preferred: Option<&Device>, list: &DeviceList) -> String {
    let Some(active) = active else {
        return preferred.map_or_else(
            || "Unavailable".into(),
            |device| format!("{} unavailable", device.name),
        );
    };
    if list.preferred(preferred) == Some(active) {
        return active.name.clone();
    }
    let label = if list.default.as_ref() == Some(active) {
        "System default"
    } else {
        &active.name
    };
    if let Some(preferred) = preferred {
        format!("{label} ({} unavailable)", preferred.name)
    } else {
        label.to_owned()
    }
}

pub(super) fn connection_diagnostics(cx: &Context<Spk>) -> Popover {
    let app = cx.weak_entity();
    Popover::new("voice-connection")
        .anchor(Anchor::BottomRight)
        .w(px(300.0))
        .trigger(tool_button(
            "voice-connection-diagnostics",
            IconName::Activity,
            "Voice connection diagnostics",
        ))
        .content(move |_, _, cx| {
            let Some(app) = app.upgrade() else {
                return div();
            };
            let View::SignedIn(sidebar) = &app.read(cx).view else {
                return div();
            };
            let Some(call) = &sidebar.voice else {
                return div();
            };
            let diagnostics = call
                .media
                .as_ref()
                .map_or_else(Default::default, |media| media.diagnostics);
            use livekit::prelude::ConnectionQuality;
            let quality = match diagnostics.quality {
                Some(ConnectionQuality::Excellent) => "Excellent",
                Some(ConnectionQuality::Good) => "Good",
                Some(ConnectionQuality::Poor) => "Poor",
                Some(ConnectionQuality::Lost) => "Lost",
                None => "Unavailable",
            };
            let metric = |value: Option<u32>, unit| {
                value.map_or_else(
                    || "Unavailable".to_owned(),
                    |value| format!("{}.{:01} {unit}", value / 10, value % 10),
                )
            };
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Label::new("Voice connection").font_semibold())
                .children(
                    [
                        ("Connection quality", quality.to_owned()),
                        ("Media round-trip", metric(diagnostics.round_trip_ms, "ms")),
                        (
                            "Audio jitter (worst track)",
                            metric(diagnostics.jitter_ms, "ms"),
                        ),
                        (
                            "Incoming audio packet loss",
                            metric(diagnostics.loss_percent, "%"),
                        ),
                    ]
                    .into_iter()
                    .map(|(label, value)| {
                        div()
                            .flex()
                            .justify_between()
                            .gap_2()
                            .child(Label::new(label).text_color(cx.theme().muted_foreground))
                            .child(Label::new(value))
                    }),
                )
        })
}

impl Spk {
    pub(super) fn participant_volume_button(
        &self,
        participant: &crate::session::Participant<'_>,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> Button {
        let id = participant.id.to_owned();
        let name = participant.name.to_owned();
        let volume = sidebar.voice_intent.preferred.participant_volume(&id);
        tool_button(
            "participant-volume",
            if volume.muted {
                IconName::VolumeX
            } else {
                IconName::Volume2
            },
            format!("Volume for {name}"),
        )
        .on_click(cx.listener(move |_, _, window, cx| {
            let app = cx.entity();
            let content = cx.new(|cx: &mut Context<ParticipantVolumeDialog>| {
                use crate::voice::ParticipantVolumeChange;
                use gpui_kit::component::slider::{SliderEvent, SliderValue};

                cx.observe(&app, |_, _, cx| cx.notify()).detach();
                let sliders = [
                    (
                        volume.mic,
                        ParticipantVolumeChange::Microphone as fn(u8) -> _,
                    ),
                    (
                        volume.screen,
                        ParticipantVolumeChange::Screen as fn(u8) -> _,
                    ),
                ]
                .map(|(initial, change)| {
                    let slider = cx.new(|_| {
                        gpui_kit::component::slider::SliderState::new()
                            .max(200.0)
                            .default_value(f32::from(initial))
                    });
                    cx.subscribe(&slider, move |dialog, _, event, cx| {
                        if let SliderEvent::Release(SliderValue::Single(value)) = event {
                            dialog.change(change(*value as u8), cx);
                        }
                        cx.notify();
                    })
                    .detach();
                    slider
                });
                ParticipantVolumeDialog {
                    app,
                    id: id.clone(),
                    sliders,
                }
            });
            let title = format!("Volume for {name}");
            window.open_dialog(cx, move |dialog, _, _| {
                dialog
                    .centered()
                    .width(px(380.0))
                    .title(title.clone())
                    .child(content.clone())
            });
        }))
    }
}

struct ParticipantVolumeDialog {
    app: Entity<Spk>,
    id: String,
    sliders: [Entity<gpui_kit::component::slider::SliderState>; 2],
}

impl ParticipantVolumeDialog {
    fn change(&self, change: crate::voice::ParticipantVolumeChange, cx: &mut Context<Self>) {
        self.app.update(cx, |app, cx| {
            app.send(
                Command::SetParticipantVolume(self.id.clone(), change),
                None,
                cx,
            );
        });
    }
}

impl Render for ParticipantVolumeDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::{
            slider::{Slider, SliderValue},
            switch::Switch,
        };

        let View::SignedIn(sidebar) = &self.app.read(cx).view else {
            return div();
        };
        let muted = sidebar
            .voice_intent
            .preferred
            .participant_volume(&self.id)
            .muted;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(Label::new("Only changes what you hear."))
            .children(
                ["Mic volume", "Screen Share volume"]
                    .into_iter()
                    .zip(&self.sliders)
                    .map(|(label, slider)| {
                        let SliderValue::Single(value) = slider.read(cx).value() else {
                            unreachable!("single participant volume slider")
                        };
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(Label::new(format!("{label} {value:.0}%")))
                            .child(
                                Slider::new(slider)
                                    .tab_stop(true)
                                    .accessibility_label(label),
                            )
                    }),
            )
            .child(
                Switch::new("mute-participant")
                    .label("Mute for me")
                    .accessibility_label("Mute for me")
                    .checked(muted)
                    .tab_stop(true)
                    .on_change(cx.listener(|dialog, enabled: &bool, _, cx| {
                        dialog.change(crate::voice::ParticipantVolumeChange::Muted(*enabled), cx);
                    })),
            )
    }
}
