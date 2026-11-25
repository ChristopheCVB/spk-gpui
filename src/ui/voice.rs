pub(super) mod media;

use super::*;
use crate::{
    session::{View, VoiceProcessingSetting},
    shortcuts::Action,
    voice::{Device, DeviceKind, DeviceList, NoiseSuppression, VoiceProcessing},
};
use gpui_kit::base::{LayoutDirection, LogicalStyledExt};
use gpui_kit::component::{
    IndexPath,
    button::ButtonCustomVariant,
    menu::PopupMenuItem,
    popover::Popover,
    select::{SearchableVec, SelectEvent},
    slider::{Slider, SliderEvent, SliderState, SliderValue},
    switch::Switch,
    tooltip::Tooltip,
};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum VoiceControls {
    Profile,
    Session,
    Channel,
    BottomBar,
}

impl VoiceControls {
    pub(super) fn compact(self) -> bool {
        self != Self::Channel
    }
}

pub(super) fn voice_action_button(
    id: &'static str,
    icon: impl Into<Icon>,
    label: impl Into<SharedString>,
    location: VoiceControls,
) -> Button {
    let label = label.into();
    voice_button(id, icon, label.clone(), location).tooltip(label)
}

pub(super) fn voice_button(
    id: &'static str,
    icon: impl Into<Icon>,
    label: impl Into<SharedString>,
    location: VoiceControls,
) -> Button {
    let label = label.into();
    let button = control_button(id)
        .ghost()
        .small()
        .accessibility_label(label.clone())
        .tab_stop(true);
    if location.compact() {
        action_icon(button, icon)
    } else {
        icon_label_button(button, icon, label)
    }
}

pub(super) fn voice_shortcut_tooltip(
    mut button: Button,
    label: &'static str,
    keys: Vec<String>,
) -> Button {
    button.interactivity().tooltip(move |window, cx| {
        let keys = keys.clone();
        let tooltip = Tooltip::element(move |_, cx| {
            let theme = cx.theme();
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(label)
                .child("·")
                .children(keys.iter().map(|key| {
                    let key = if key == "Shift" {
                        "⇧".to_owned()
                    } else {
                        key.to_uppercase()
                    };
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(theme.background)
                        .text_color(theme.muted_foreground)
                        .text_xs()
                        .child(key)
                }))
        });
        floating_tooltip(tooltip, cx).build(window, cx)
    });
    button
}

fn voice_options_button(label: impl Into<SharedString>, open: bool) -> Button {
    tool_button(
        "options",
        if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronUp
        },
        label,
    )
    .w(ACTION_ICON_SIZE)
    .px_0()
}

type OptionsChange = Rc<dyn Fn(bool, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub(super) struct VoiceButtonGroup {
    id: &'static str,
    primary: Button,
    label: SharedString,
    popover: Popover,
    active_color: Option<Hsla>,
    controlled: Option<(bool, OptionsChange)>,
}

impl VoiceButtonGroup {
    pub(super) fn new(
        id: &'static str,
        primary: Button,
        label: impl Into<SharedString>,
        popover: Popover,
        active_color: Option<Hsla>,
    ) -> Self {
        Self {
            id,
            primary,
            label: label.into(),
            popover,
            active_color,
            controlled: None,
        }
    }

    pub(super) fn controlled(
        mut self,
        open: bool,
        on_change: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.controlled = Some((open, Rc::new(on_change)));
        self
    }
}

impl RenderOnce for VoiceButtonGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id, cx, |_, cx| (false, cx.focus_handle()));
        let (local_open, focus) = state.read(cx).clone();
        let (open, on_change) = self.controlled.unwrap_or_else(|| {
            (
                local_open,
                Rc::new(move |open, _, cx| {
                    state.update(cx, |(current, _), cx| {
                        *current = open;
                        cx.notify();
                    });
                }),
            )
        });
        let toggle_options = {
            let on_change = on_change.clone();
            let focus = focus.clone();
            move |window: &mut Window, cx: &mut App| {
                if !open {
                    focus.focus(window, cx);
                }
                on_change(!open, window, cx);
            }
        };
        let active_style = self.active_color.map(|color| {
            let highlight = color.opacity(0.15);
            (
                ButtonCustomVariant::new(cx)
                    .foreground(color)
                    .hover(highlight)
                    .active(highlight),
                color.opacity(0.10),
            )
        });
        let style_button = |button: Button| {
            button.when_some(active_style, |button, (variant, tint)| {
                button.custom(variant).bg(tint)
            })
        };
        let primary = style_button(self.primary);
        let options = style_button(voice_options_button(self.label, open))
            .open(open && active_style.is_none())
            .track_focus(&focus)
            // An open popover dismisses on mouse-down outside its contents.
            .on_mouse_down(MouseButton::Left, {
                let toggle_options = toggle_options.clone();
                move |_, window, cx| {
                    if !open {
                        toggle_options(window, cx);
                    }
                }
            })
            .on_click(move |event, window, cx| {
                if !matches!(event, ClickEvent::Mouse(_)) {
                    toggle_options(window, cx);
                }
            });
        let options = OptionsAccessibility(options).aria_expanded(open).0;

        let rtl = LayoutDirection::global(cx).is_rtl();
        div()
            .id(self.id)
            .role(Role::Group)
            .relative()
            .flex()
            .flex_row_dir(cx)
            .items_center()
            // ButtonGroup assigns toggle semantics to every child, including momentary actions.
            .child(square_button_edge(primary, rtl))
            .child(square_button_edge(options, !rtl))
            // The independent anchor keeps popover input handlers off the primary button.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .size_0()
                    .when(rtl, |el| el.left_0())
                    .when(!rtl, |el| el.right_0())
                    .child(
                        self.popover
                            .anchor(Anchor::BottomRight)
                            .trigger(VoiceOptionsAnchor)
                            .open(open)
                            .on_open_change(move |open, window, cx| on_change(*open, window, cx)),
                    ),
            )
    }
}

struct OptionsAccessibility(Button);

impl InteractiveElement for OptionsAccessibility {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.0.interactivity()
    }
}

impl StatefulInteractiveElement for OptionsAccessibility {}

fn square_button_edge(button: Button, left: bool) -> Button {
    if left {
        button.rounded_tl(px(0.0)).rounded_bl(px(0.0))
    } else {
        button.rounded_tr(px(0.0)).rounded_br(px(0.0))
    }
}

#[derive(IntoElement)]
struct VoiceOptionsAnchor;

impl Selectable for VoiceOptionsAnchor {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl RenderOnce for VoiceOptionsAnchor {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
    }
}

pub(super) fn voice_popover(
    id: &'static str,
    width: Pixels,
    cx: &App,
    button: impl FnOnce(bool) -> Button + 'static,
) -> Popover {
    floating_popover(id, cx)
        .anchor(Anchor::BottomRight)
        .w(width)
        .trigger(VoicePopoverTrigger {
            button,
            open: false,
        })
}

#[derive(IntoElement)]
struct VoicePopoverTrigger<F: FnOnce(bool) -> Button + 'static> {
    button: F,
    open: bool,
}

impl<F: FnOnce(bool) -> Button + 'static> Selectable for VoicePopoverTrigger<F> {
    fn selected(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl<F: FnOnce(bool) -> Button + 'static> RenderOnce for VoicePopoverTrigger<F> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state("voice-popover-trigger-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        div()
            .id("voice-popover-trigger")
            .child((self.button)(self.open).open(self.open).track_focus(&focus))
            .capture_any_mouse_down(move |event, window, cx| {
                // The popover saves the current focus before opening on mouse-down.
                if event.button == MouseButton::Left && !self.open {
                    focus.focus(window, cx);
                }
            })
    }
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
        let theme = cx.theme();

        div()
            .id(match location {
                VoiceControls::Profile => "profile-voice-controls",
                VoiceControls::Session => "session-voice-controls",
                VoiceControls::Channel => "room-voice-controls",
                VoiceControls::BottomBar => "bottom-bar-voice-controls",
            })
            .flex()
            .items_center()
            .gap_1()
            .when(location == VoiceControls::BottomBar, |controls| {
                controls.flex_wrap().max_w_full()
            })
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
                    let button = voice_button("toggle", icon, label, location)
                        .when(
                            action != Action::Mute || !mic_needs_retry(sidebar),
                            |button| button.toggled(off),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.voice_action(action, cx)));
                    let button = if let Some(binding) = binding {
                        let chord = binding.chord.to_string();
                        let keys = chord.split(" + ").map(str::to_owned).collect();
                        let tooltip = match action {
                            Action::Mute => "Mute / unmute",
                            Action::Deafen => "Deafen / undeafen",
                        };
                        voice_shortcut_tooltip(button, tooltip, keys)
                    } else {
                        button.tooltip(label)
                    };
                    let app = cx.weak_entity();
                    let popup_app = app.clone();
                    VoiceButtonGroup::new(
                        device_label(kind),
                        button,
                        picker_label,
                        floating_popover("device-picker", cx).w(px(300.0)).content(
                            move |_, window, cx| {
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
                                        let media =
                                            device_picker(sidebar, DeviceKind::MediaOutput, &app);
                                        let volume = sidebar.voice_intent.preferred.output_volume;
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
                            },
                        ),
                        off.then_some(theme.danger),
                    )
                    .controlled(open, move |open, _, cx| {
                        let _ = app.update(cx, |app, cx| {
                            if open {
                                app.device_picker = Some((location, kind));
                                app.send(Command::RefreshDevices, None, cx);
                            } else if app.device_picker == Some((location, kind)) {
                                app.device_picker = None;
                                cx.notify();
                            }
                        });
                    })
                }),
            )
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
            control_select(&noise, cx.theme().background, cx)
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
                .text_sm()
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

pub(super) fn quick_level(
    id: &'static str,
    value: u8,
    max: u8,
    app: &Entity<Spk>,
    window: &mut Window,
    cx: &mut App,
    command: impl Fn(u8) -> Command + 'static,
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
                control_select(&select, cx.theme().background, cx)
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
    connection_diagnostics_popover(
        tool_button(
            "voice-connection-diagnostics",
            IconName::Activity,
            "Voice connection diagnostics",
        ),
        cx,
    )
}

pub(super) fn connection_diagnostics_with_status(
    status: &'static str,
    cx: &Context<Spk>,
) -> Popover {
    connection_diagnostics_popover(
        icon_label_button(
            control_button("voice-connection-status")
                .ghost()
                .tab_stop(true),
            IconName::Activity,
            status,
        )
        .accessibility_label(format!("{status} Voice connection diagnostics"))
        .tooltip("Voice connection diagnostics")
        .text_color(cx.theme().muted_foreground),
        cx,
    )
}

fn connection_diagnostics_popover(trigger: Button, cx: &Context<Spk>) -> Popover {
    let app = cx.weak_entity();
    voice_popover("voice-connection", px(320.0), cx, |_| trigger).content(move |_, _, cx| {
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
                        .id(label)
                        .flex()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .child(
                            Label::new(label)
                                .line_height(px(16.0))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(Label::new(value).line_height(px(16.0)))
                }),
            )
    })
}

pub(super) fn participant_volume_item(
    id: String,
    name: String,
    app: WeakEntity<Spk>,
) -> PopupMenuItem {
    PopupMenuItem::new("Volume…")
        .icon(IconName::SlidersHorizontal)
        .on_click(move |_, window, cx| {
            let app = app.clone();
            let id = id.clone();
            let name = name.clone();
            window.defer(cx, move |window, cx| {
                let _ = app.update(cx, |this, cx| {
                    this.open_participant_volume(&id, &name, window, cx)
                });
            });
        })
}

impl Spk {
    fn open_participant_volume(
        &self,
        id: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let volume = sidebar.voice_intent.preferred.participant_volume(id);
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
                id: id.to_owned(),
                sliders,
            }
        });
        let title = format!("Volume for {name}");
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .centered()
                .border_0()
                .bg(cx.theme().popover)
                .width(px(380.0))
                .title(title.clone())
                .child(content.clone())
        });
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
                            .child(Label::new(format!("{label} {value:.0}%")).id(label))
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
