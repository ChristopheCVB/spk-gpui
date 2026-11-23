use super::{VoiceButtonGroup, VoiceControls, voice_action_button, voice_popover};
use crate::ui::{
    radio::{RadioAction, radio_panel},
    soundboard::soundboard_panel,
    *,
};
use gpui_kit::component::{
    popover::Popover,
    scroll::{Scrollbar, ScrollbarMode},
};

type MediaPanel = fn(Entity<Spk>, u8, &mut Window, &mut App) -> (AnyView, FocusHandle);

#[derive(Clone, Copy)]
enum MediaControl {
    Radio {
        action: Option<RadioAction>,
        volume: u8,
    },
    Soundboard {
        volume: u8,
    },
}

struct MediaPopover {
    panel: Option<(AnyView, FocusHandle)>,
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
        sidebar: &Sidebar,
        location: VoiceControls,
        cx: &Context<Self>,
    ) -> [impl IntoElement + use<>; 2] {
        let radio = MediaControl::Radio {
            action: RadioAction::for_primary(
                sidebar.radio.station.is_some(),
                sidebar.radio.enabled,
            ),
            volume: sidebar.radio.volume,
        };
        let soundboard = MediaControl::Soundboard {
            volume: sidebar.voice_intent.preferred.soundboard_volume.min(100),
        };
        [radio, soundboard].map(|control| MediaButton {
            control,
            location,
            app: cx.weak_entity(),
        })
    }
}

impl RenderOnce for MediaButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (id, load, build_panel, volume, radio): (_, fn() -> Command, MediaPanel, _, _) =
            match self.control {
                MediaControl::Radio { action, volume } => (
                    "radio",
                    || Command::RadioFavorites,
                    radio_panel,
                    volume,
                    Some(action),
                ),
                MediaControl::Soundboard { volume } => (
                    "soundboard",
                    || Command::LoadSoundboard,
                    soundboard_panel,
                    volume,
                    None,
                ),
            };
        let state =
            window.use_keyed_state((id, self.location as usize), cx, |_, cx| MediaPopover {
                panel: None,
                radio_primary: cx.focus_handle(),
            });
        let current = state.read(cx);
        let open = current.panel.is_some();
        let panel = current.panel.clone();
        let primary_focus = current.radio_primary.clone();
        let location = self.location;
        let open_panel = {
            let state = state.clone();
            let app = self.app.clone();
            move |open, window: &mut Window, cx: &mut App| {
                let panel = if open {
                    let app = app.upgrade()?;
                    let panel = build_panel(app.clone(), volume, window, cx);
                    app.update(cx, |app, cx| {
                        app.send(load(), None, cx);
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
        };
        let playing = radio.flatten() == Some(RadioAction::Pause);
        let primary = radio.map(|action| {
            let label = action.map_or_else(
                || "Channel Radio".to_owned(),
                |action| format!("{} Radio", action.label()),
            );
            voice_action_button("radio-primary", IconName::Radio, "Radio", location)
                .accessibility_label(label.clone())
                .tooltip(label)
                .track_focus(&primary_focus)
                .when_some(action, |button, _| {
                    button.toggled(playing).selected(playing)
                })
                .on_click({
                    let app = self.app.clone();
                    let state = state.clone();
                    let open_panel = open_panel.clone();
                    move |_, window, cx| {
                        if let Some(action) = action {
                            let _ = app.update(cx, |app, cx| {
                                app.send(action.command(), None, cx);
                            });
                        } else if state.read(cx).panel.is_none() {
                            primary_focus.focus(window, cx);
                            open_panel(true, window, cx);
                        }
                    }
                })
        });
        let width = px(330.0).min(window.viewport_size().width - px(16.0));
        let popover = if primary.is_some() {
            Popover::new(id).w(width)
        } else {
            voice_popover(id, width, move |_| {
                voice_action_button(
                    "soundboard-trigger",
                    IconName::Megaphone,
                    "Soundboard",
                    location,
                )
            })
        }
        .when_some(panel, |popover, (panel, search)| {
            popover.track_focus(&search).child(panel)
        });
        div().id((id, self.location as usize)).child(match primary {
            Some(primary) => {
                VoiceButtonGroup::new("radio-controls", primary, "Radio options", popover)
                    .controlled(open, move |open, window, cx| {
                        open_panel(open, window, cx);
                    })
                    .into_any_element()
            }
            None => popover
                .open(open)
                .on_open_change(move |open, window, cx| {
                    if let Some(search) = open_panel(*open, window, cx) {
                        search.focus(window, cx);
                    }
                })
                .into_any_element(),
        })
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
