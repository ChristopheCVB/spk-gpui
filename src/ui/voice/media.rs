use super::{VoiceControls, voice_action_button, voice_options_button};
use crate::ui::{radio::radio_panel, soundboard::soundboard_panel, *};
use gpui_kit::component::{
    popover::Popover,
    scroll::{Scrollbar, ScrollbarMode},
};

#[derive(Clone, Copy)]
enum MediaControl {
    Radio(Option<bool>),
    Soundboard,
}

impl MediaControl {
    fn id(self) -> &'static str {
        match self {
            Self::Radio(_) => "radio",
            Self::Soundboard => "soundboard",
        }
    }

    fn set_open(
        self,
        state: &Entity<MediaPopover>,
        app: &WeakEntity<Spk>,
        open: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<FocusHandle> {
        let panel = if open {
            let app = app.upgrade()?;
            let View::SignedIn(sidebar) = &app.read(cx).view else {
                return None;
            };
            let (panel, load) = match self {
                Self::Radio(_) => {
                    let volume = sidebar.radio.volume;
                    (
                        radio_panel(app.clone(), volume, window, cx),
                        Command::RadioFavorites,
                    )
                }
                Self::Soundboard => {
                    let volume = sidebar.voice_intent.preferred.soundboard_volume.min(100);
                    (
                        soundboard_panel(app.clone(), volume, window, cx),
                        Command::LoadSoundboard,
                    )
                }
            };
            app.update(cx, |app, cx| {
                app.send(load, None, cx);
                app.send(Command::RefreshDevices, None, cx);
            });
            Some(panel)
        } else {
            None
        };
        let search = panel.as_ref().map(|(_, search)| search.clone());
        state.update(cx, |state, cx| {
            state.panel = panel;
            cx.notify();
        });
        search
    }
}

struct MediaPopover {
    panel: Option<(AnyView, FocusHandle)>,
    trigger: FocusHandle,
    radio_primary: FocusHandle,
}

#[derive(IntoElement)]
pub(super) struct MediaButton {
    control: MediaControl,
    location: VoiceControls,
    app: WeakEntity<Spk>,
}

impl Spk {
    pub(in crate::ui) fn voice_media_buttons(
        &self,
        location: VoiceControls,
        cx: &Context<Self>,
    ) -> [impl IntoElement + use<>; 2] {
        use crate::session::RadioPlayback;
        let running = match &self.view {
            View::SignedIn(sidebar) if sidebar.radio.station.is_some() => {
                Some(sidebar.radio.enabled && sidebar.radio.playback != RadioPlayback::Unavailable)
            }
            _ => None,
        };
        [MediaControl::Radio(running), MediaControl::Soundboard].map(|control| MediaButton {
            control,
            location,
            app: cx.weak_entity(),
        })
    }
}

impl RenderOnce for MediaButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let control = self.control;
        let id = control.id();
        let state =
            window.use_keyed_state((id, self.location as usize), cx, |_, cx| MediaPopover {
                panel: None,
                trigger: cx.focus_handle(),
                radio_primary: cx.focus_handle(),
            });
        let current = state.read(cx);
        let open = current.panel.is_some();
        let anchor_focus = current.trigger.clone();
        let panel = current.panel.clone();
        let primary_focus = current.radio_primary.clone();
        let (trigger, primary) = match control {
            MediaControl::Radio(running) => {
                let label = match running {
                    Some(true) => "Pause Radio",
                    Some(false) => "Resume Radio",
                    None => "Channel Radio",
                };
                let primary =
                    voice_action_button("radio-primary", IconName::Radio, "Radio", self.location)
                        .accessibility_label(label)
                        .tooltip(label)
                        .track_focus(&primary_focus)
                        .when_some(running, |button, playing| {
                            button.toggled(playing).selected(playing)
                        })
                        .on_click({
                            let app = self.app.clone();
                            let state = state.clone();
                            move |_, window, cx| {
                                if let Some(playing) = running {
                                    let _ = app.update(cx, |app, cx| {
                                        app.send(
                                            if playing {
                                                Command::PauseRadio
                                            } else {
                                                Command::ListenToRadio
                                            },
                                            None,
                                            cx,
                                        );
                                    });
                                } else if state.read(cx).panel.is_none() {
                                    primary_focus.focus(window, cx);
                                    control.set_open(&state, &app, true, window, cx);
                                }
                            }
                        });
                (
                    voice_options_button("Radio options", open).selected(running == Some(true)),
                    Some(primary),
                )
            }
            MediaControl::Soundboard => (
                voice_action_button(
                    "soundboard-trigger",
                    IconName::Megaphone,
                    "Soundboard",
                    self.location,
                ),
                None,
            ),
        };
        let popover = Popover::new(id)
            .anchor(Anchor::BottomRight)
            .w(px(330.0).min(window.viewport_size().width - px(16.0)))
            .open(open)
            .trigger(trigger.track_focus(&anchor_focus))
            .on_open_change(move |open, window, cx| {
                if let Some(search) = control.set_open(&state, &self.app, *open, window, cx) {
                    search.focus(window, cx);
                }
            })
            .when_some(panel, |popover, (panel, search)| {
                popover.track_focus(&search).child(panel)
            });
        div()
            .id((id, self.location as usize))
            .flex()
            .items_center()
            .children(primary)
            .child(
                div()
                    .id("trigger")
                    .capture_any_mouse_down(move |event, window, cx| {
                        // The popover saves the current focus before opening on mouse-down.
                        if event.button == MouseButton::Left && !open {
                            anchor_focus.focus(window, cx);
                        }
                    })
                    .child(popover),
            )
    }
}

pub(in crate::ui) fn media_results(
    scroll: &ScrollHandle,
    results: Stateful<Div>,
) -> impl IntoElement + use<> {
    div()
        .relative()
        .child(
            results
                .w_full()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .pr_3(),
        )
        .child(Scrollbar::vertical(scroll).mode(ScrollbarMode::Always))
}

#[derive(IntoElement)]
pub(in crate::ui) struct MediaResult {
    pub id: SharedString,
    pub index: usize,
    pub scroll: ScrollHandle,
    pub content: AnyElement,
}

impl RenderOnce for MediaResult {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window.use_keyed_state((self.id.clone(), self.index), cx, |window, cx| {
            let focus = cx.focus_handle();
            let subscription = window.on_focus_in(&focus, cx, move |window, _| {
                self.scroll.scroll_to_item(self.index);
                window.refresh();
            });
            (focus, subscription)
        });
        div()
            .id(self.id)
            .min_w_0()
            .track_focus(&focus.read(cx).0)
            .child(self.content)
    }
}
