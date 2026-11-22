use super::*;
use crate::{
    session::{View, VoiceProcessingSetting},
    shortcuts::Action,
    voice::{Device, DeviceKind, DeviceList, NoiseSuppression, VoiceProcessing},
};
use gpui_kit::component::{
    IndexPath,
    popover::Popover,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    slider::{Slider, SliderEvent, SliderState, SliderValue},
    switch::Switch,
};

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
            .when(compact, |controls| controls.flex_wrap().max_w_full())
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
                    let binding = self.shortcuts.binding(action);
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
                            .when_some(binding, |button, binding| {
                                button
                                    .tooltip(format!("{label} ({})", binding.chord))
                                    .child(
                                        gpui_kit::component::kbd::Kbd::new(
                                            binding.chord.keystroke(),
                                        )
                                        .outline(),
                                    )
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.voice_action(action, cx)),
                            ),
                        )
                        .child({
                            let app = cx.weak_entity();
                            let popup_app = app.clone();
                            Popover::new("device-picker")
                                .anchor(Anchor::BottomRight)
                                .w(px(300.0))
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
                                .content(move |_, window, cx| {
                                    let Some(app) = popup_app.upgrade() else {
                                        return div();
                                    };
                                    let View::SignedIn(sidebar) = &app.read(cx).view else {
                                        return div();
                                    };
                                    let picker = device_picker(sidebar, kind, &app);
                                    let (label, extra) = match kind {
                                        DeviceKind::Microphone => (
                                            "Input device",
                                            microphone_controls(
                                                sidebar.voice_intent.preferred.processing,
                                                sidebar.echo_reference_unavailable(),
                                                &app,
                                                window,
                                                cx,
                                            )
                                            .into_any_element(),
                                        ),
                                        DeviceKind::Speakers => {
                                            let media = device_picker(
                                                sidebar,
                                                DeviceKind::MediaOutput,
                                                &app,
                                            );
                                            let volume =
                                                sidebar.voice_intent.preferred.output_volume;
                                            (
                                                "Voice output",
                                                speaker_controls(media, volume, &app, window, cx)
                                                    .into_any_element(),
                                            )
                                        }
                                        DeviceKind::MediaOutput => unreachable!(),
                                    };
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(Label::new(label).font_semibold())
                                        .child(picker)
                                        .child(extra)
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

pub(super) fn processing_switch(
    app: &Entity<Spk>,
    label: &'static str,
    checked: bool,
    setting: fn(bool) -> VoiceProcessingSetting,
) -> Switch {
    let app = app.downgrade();
    Switch::new(label)
        .label(label)
        .accessibility_label(label)
        .checked(checked)
        .tab_stop(true)
        .on_change(move |enabled, _, cx| {
            let _ = app.update(cx, |app, cx| {
                app.send(Command::SetVoiceProcessing(setting(*enabled)), None, cx);
            });
        })
}

const NOISE_MODES: [(NoiseSuppression, &str); 3] = [
    (NoiseSuppression::Off, "Off"),
    (NoiseSuppression::Standard, "Standard"),
    (NoiseSuppression::Enhanced, "Enhanced (DeepFilterNet)"),
];

fn microphone_controls(
    settings: VoiceProcessing,
    echo_reference_unavailable: bool,
    app: &Entity<Spk>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let echo = settings.echo_cancellation;
    let noise = window.use_keyed_state(
        (
            "quick-noise-suppression",
            settings.noise_suppression as usize,
        ),
        cx,
        |window, cx| {
            let app = app.downgrade();
            cx.subscribe(&cx.entity(), move |_, _, event, cx| {
                if let SelectEvent::Confirm(Some(label)) = event
                    && let Some((mode, _)) = NOISE_MODES.iter().find(|(_, name)| name == label)
                {
                    let _ = app.update(cx, |app, cx| {
                        app.send(
                            Command::SetVoiceProcessing(VoiceProcessingSetting::NoiseSuppression(
                                *mode,
                            )),
                            None,
                            cx,
                        );
                    });
                }
            })
            .detach();
            let index = NOISE_MODES
                .iter()
                .position(|(noise, _)| *noise == settings.noise_suppression);
            SelectState::new(
                SearchableVec::new(NOISE_MODES.map(|(_, label)| label).to_vec()),
                index.map(|index| IndexPath::default().row(index)),
                window,
                cx,
            )
        },
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new("Noise suppression").font_semibold())
        .child(
            Select::new(&noise)
                .small()
                .w_full()
                .accessibility_label("Noise suppression"),
        )
        .child(processing_switch(
            app,
            "Echo cancellation",
            echo,
            VoiceProcessingSetting::EchoCancellation,
        ))
        .when(echo && echo_reference_unavailable, |controls| {
            controls.child(
                Label::new("Echo cancellation is unavailable on the active speaker output.")
                    .text_xs(),
            )
        })
        .child(processing_switch(
            app,
            "Auto gain",
            settings.auto_gain,
            VoiceProcessingSetting::AutoGain,
        ))
        .child(Label::new("Sensitivity").font_semibold())
        .child(
            Label::new("Higher picks up quieter speech.")
                .text_xs()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            Slider::new(&quick_level(
                "quick-sensitivity",
                settings.sensitivity,
                100,
                app,
                window,
                cx,
                |value| Command::SetVoiceProcessing(VoiceProcessingSetting::Sensitivity(value)),
            ))
            .tab_stop(true)
            .accessibility_label("Microphone sensitivity"),
        )
}

fn speaker_controls(
    media: impl IntoElement,
    volume: u8,
    app: &Entity<Spk>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let level = quick_level(
        "quick-output-volume",
        volume,
        200,
        app,
        window,
        cx,
        Command::SetOutputVolume,
    );
    let SliderValue::Single(volume) = level.read(cx).value() else {
        unreachable!("single output volume slider");
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new("Media output").font_semibold())
        .child(media)
        .child(
            div()
                .flex()
                .justify_between()
                .child(Label::new("Output volume").font_semibold())
                .child(Label::new(format!("{volume:.0}%")).text_color(cx.theme().muted_foreground)),
        )
        .child(
            Slider::new(&level)
                .tab_stop(true)
                .accessibility_label("Output volume"),
        )
}

fn quick_level(
    id: &'static str,
    value: u8,
    max: u8,
    app: &Entity<Spk>,
    window: &mut Window,
    cx: &mut App,
    command: fn(u8) -> Command,
) -> Entity<SliderState> {
    window.use_keyed_state(id, cx, |_, cx| {
        let app = app.downgrade();
        cx.subscribe(&cx.entity(), move |_, _, event, cx| {
            if let SliderEvent::Release(SliderValue::Single(value)) = event {
                let _ = app.update(cx, |app, cx| app.send(command(*value as u8), None, cx));
            }
        })
        .detach();
        SliderState::new()
            .max(f32::from(max))
            .default_value(f32::from(value.min(max)))
    })
}

#[derive(Clone, PartialEq)]
struct DeviceOption(Option<Device>);

impl SelectItem for DeviceOption {
    type Value = Option<Device>;

    fn title(&self) -> SharedString {
        self.0
            .as_ref()
            .map_or("System default", |device| device.name.as_str())
            .to_owned()
            .into()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

struct DevicePickerState {
    select: Entity<SelectState<SearchableVec<DeviceOption>>>,
    options: Vec<DeviceOption>,
    selected: IndexPath,
}

#[derive(IntoElement)]
struct DevicePicker {
    app: WeakEntity<Spk>,
    kind: DeviceKind,
    options: Vec<DeviceOption>,
    selected: IndexPath,
    status: Option<String>,
}

pub(super) fn device_picker(
    sidebar: &Sidebar,
    kind: DeviceKind,
    app: &Entity<Spk>,
) -> impl IntoElement + use<> {
    let (list, active, preferred) = devices(sidebar, kind);
    let selected = list
        .preferred(preferred)
        .and_then(|preferred| list.choices.iter().position(|device| device == preferred))
        .map_or(0, |index| index + 1);
    let status = device_status(active, preferred, list);
    let selected_label = selected
        .checked_sub(1)
        .map_or("System default", |index| list.choices[index].name.as_str());
    DevicePicker {
        app: app.downgrade(),
        kind,
        options: std::iter::once(DeviceOption(None))
            .chain(
                list.choices
                    .iter()
                    .cloned()
                    .map(|device| DeviceOption(Some(device))),
            )
            .collect(),
        selected: IndexPath::default().row(selected),
        status: (status != selected_label).then_some(status),
    }
}

impl RenderOnce for DevicePicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let label = device_label(self.kind);
        let key = ("audio-device-select", self.kind as usize);
        let picker = window.use_keyed_state(key, cx, |window, cx| {
            let select = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(self.options.clone()),
                    Some(self.selected),
                    window,
                    cx,
                )
            });
            let app = self.app.clone();
            let kind = self.kind;
            cx.subscribe(&select, move |_, _, event, cx| {
                if let SelectEvent::Confirm(Some(device)) = event {
                    let _ = app.update(cx, |app, cx| {
                        app.send(Command::ChooseDevice(kind, device.clone()), None, cx);
                    });
                }
            })
            .detach();
            DevicePickerState {
                select,
                options: self.options.clone(),
                selected: self.selected,
            }
        });
        let state = picker.read(cx);
        let select = state.select.clone();
        if state.options != self.options || state.selected != self.selected {
            picker.update(cx, |state, cx| {
                select.update(cx, |select, cx| {
                    if state.options != self.options {
                        select.set_items(SearchableVec::new(self.options.clone()), window, cx);
                    }
                    select.set_selected_index(Some(self.selected), window, cx);
                });
                state.options = self.options;
                state.selected = self.selected;
            });
        }

        let refresh = move |cx: &mut App| {
            let _ = self.app.update(cx, |app, cx| {
                app.send(Command::RefreshDevices, None, cx);
            });
        };
        let refresh_key = refresh.clone();
        div()
            .id(("audio-device-picker", self.kind as usize))
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| refresh(cx))
            .capture_key_down(move |event, _, cx| {
                if matches!(
                    event.keystroke.key.as_str(),
                    "enter" | "space" | "down" | "up"
                ) {
                    refresh_key(cx);
                }
            })
            .child(
                Select::new(&select)
                    .small()
                    .accessibility_label(label)
                    .w_full(),
            )
            .when_some(self.status, |body, status| {
                body.child(
                    Label::new(status)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                )
            })
    }
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
                        ("Audio upload", metric(diagnostics.upload_kbps, "kbps")),
                        ("Audio download", metric(diagnostics.download_kbps, "kbps")),
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
