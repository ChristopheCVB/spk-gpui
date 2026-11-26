use super::{
    voice::{
        device_picker,
        media::{MediaResult, media_results},
    },
    *,
};
use crate::{api::RadioStation, session::RadioPlayback};
use gpui_kit::component::Disableable;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RadioAction {
    Pause,
    Resume,
    Retry,
}

impl RadioAction {
    pub(super) fn for_primary(station_selected: bool, enabled: bool) -> Option<Self> {
        station_selected.then_some(if enabled { Self::Pause } else { Self::Resume })
    }

    pub(super) fn for_sidebar(sidebar: &Sidebar) -> Option<Self> {
        let radio = &sidebar.radio;
        Self::for_primary(radio.station.is_some(), radio.enabled).map(|action| {
            if action == Self::Pause && radio.playback == RadioPlayback::Unavailable {
                Self::Retry
            } else {
                action
            }
        })
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::Retry => "Retry",
        }
    }

    pub(super) fn command(self) -> Command {
        match self {
            Self::Pause => Command::PauseRadio,
            Self::Resume | Self::Retry => Command::ListenToRadio,
        }
    }
}

struct RadioPanel {
    spk: Entity<Spk>,
    query: Entity<InputState>,
    volume: Entity<SliderState>,
    favorites: bool,
    station_scroll: ScrollHandle,
    panel_scroll: ScrollHandle,
}

pub(super) fn radio_panel(
    spk: Entity<Spk>,
    volume: u8,
    window: &mut Window,
    cx: &mut App,
) -> (AnyView, FocusHandle) {
    let panel = cx.new(|cx: &mut Context<RadioPanel>| {
        cx.observe(&spk, |_, _, cx| cx.notify()).detach();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search stations"));
        cx.subscribe(&query, |panel, _, event, cx| {
            if !panel.favorites && matches!(event, InputEvent::PressEnter { .. }) {
                panel.search(cx);
            }
            cx.notify();
        })
        .detach();
        let volume = cx.new(|_| {
            SliderState::new()
                .max(200.0)
                .default_value(f32::from(volume))
        });
        cx.subscribe(&volume, |panel, _, event, cx| {
            if let SliderEvent::Release(SliderValue::Single(value)) = event {
                panel.send(Command::SetRadioVolume(*value as u8), cx);
            }
            cx.notify();
        })
        .detach();
        RadioPanel {
            spk,
            query,
            volume,
            favorites: false,
            station_scroll: ScrollHandle::new(),
            panel_scroll: ScrollHandle::new(),
        }
    });
    let focus = panel.read(cx).query.focus_handle(cx);
    (panel.into(), focus)
}

impl RadioPanel {
    fn send(&self, command: Command, cx: &mut Context<Self>) {
        self.spk.update(cx, |app, cx| app.send(command, None, cx));
    }

    fn search(&self, cx: &mut Context<Self>) {
        self.send(
            Command::SearchRadio(self.query.read(cx).value().to_string()),
            cx,
        );
    }

    fn station_row(
        &self,
        station: &RadioStation,
        sidebar: &Sidebar,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let radio = &sidebar.radio;
        let selected = radio
            .station
            .as_ref()
            .is_some_and(|current| current.id == station.id);
        let favorite = radio
            .favorites
            .iter()
            .flatten()
            .any(|item| item.id == station.id);
        let id = station.id.clone();
        let id_for_favorite = id.clone();
        let select_label = format!("Play {} for everyone", station.name);
        let favorite_label = if favorite {
            format!("Remove {} from Favorites", station.name)
        } else {
            format!("Add {} to Favorites", station.name)
        };
        div()
            .flex()
            .flex_shrink_0()
            .gap(px(4.0))
            .items_center()
            .child(
                Button::new("select")
                    .ghost()
                    .selected(selected)
                    .flex_1()
                    .min_w_0()
                    .h_auto()
                    .px_2()
                    .py(px(6.0))
                    .tab_stop(true)
                    .disabled(
                        station.available == Some(false)
                            || !matches!(sidebar.connection, crate::api::Connection::Online),
                    )
                    .accessibility_label(select_label)
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.send(Command::SelectRadio(id.clone()), cx)
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(if selected {
                                    IconName::Radio
                                } else {
                                    IconName::Play
                                })
                                .size(ACTION_ICON_SIZE)
                                .flex_shrink_0()
                                .when(selected, |icon| icon.text_color(cx.theme().primary)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        Label::new(station.name.clone())
                                            .line_height(px(20.0))
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(if station.available == Some(false) {
                                            "Unavailable".to_owned()
                                        } else {
                                            format!("{} · {}", station.country, station.codec)
                                        })
                                        .text_xs()
                                        .line_height(px(16.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate(),
                                    ),
                            ),
                    ),
            )
            .child(
                tool_button(
                    if favorite {
                        "remove-favorite"
                    } else {
                        "add-favorite"
                    },
                    IconName::Star,
                    favorite_label,
                )
                .selected(favorite)
                .disabled(radio.favorites.is_none() || radio.loading_favorites)
                .when(favorite, |button| button.bg(cx.theme().accent))
                .on_click(cx.listener(move |panel, _, _, cx| {
                    panel.send(
                        Command::SetRadioFavorite {
                            id: id_for_favorite.clone(),
                            saved: !favorite,
                        },
                        cx,
                    )
                })),
            )
    }
}

impl Render for RadioPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let View::SignedIn(sidebar) = &self.spk.read(cx).view else {
            return div();
        };
        if !sidebar
            .voice
            .as_ref()
            .is_some_and(|voice| voice.is_participating())
        {
            return div().child(Label::new("Join a voice channel to use Radio."));
        }
        let theme = cx.theme();
        let radio = &sidebar.radio;
        let playback_action = RadioAction::for_sidebar(sidebar);
        let status = if playback_action == Some(RadioAction::Resume) {
            "Paused for you"
        } else if playback_action == Some(RadioAction::Retry) {
            "Stream unavailable. Retry or choose another."
        } else if sidebar.voice_intent.mute.deafened {
            "Deafened"
        } else if radio.playback == RadioPlayback::Playing {
            "Playing"
        } else {
            "Connecting…"
        };
        let entries = if self.favorites {
            &radio.favorites
        } else {
            &radio.stations
        };
        let display_loading = if self.favorites {
            radio.loading_favorites
        } else {
            radio.searching
        };
        let query = self.query.read(cx).value().trim().to_lowercase();
        let stations = entries
            .iter()
            .flatten()
            .filter(|station| !self.favorites || station.name.to_lowercase().contains(&query))
            .collect::<Vec<_>>();

        let player =
            radio
                .station
                .as_ref()
                .zip(playback_action)
                .map(|(station, action)| {
                    let action_icon = match action {
                        RadioAction::Pause => IconName::Pause,
                        RadioAction::Resume => IconName::Play,
                        RadioAction::Retry => IconName::RotateCw,
                    };
                    let volume_percent = format!("{:.0}%", self.volume.read(cx).value().start());
                    div()
                        .flex()
                        .flex_col()
                        .flex_shrink_0()
                        .gap(px(12.0))
                        .child(
                            div()
                                .min_w_0()
                                .child(Label::new(station.name.clone()).font_semibold().truncate())
                                .child(
                                    Label::new(status)
                                        .text_xs()
                                        .text_color(theme.muted_foreground),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .gap_2()
                                .child(
                                    icon_label_button(
                                        control_button("playback").small(),
                                        action_icon,
                                        action.label(),
                                    )
                                    .tab_stop(true)
                                    .on_click(cx.listener(
                                        move |panel, _, _, cx| panel.send(action.command(), cx),
                                    )),
                                )
                                .child(
                                    icon_label_button(
                                        control_button("stop").ghost().small(),
                                        IconName::Square,
                                        "Stop for everyone",
                                    )
                                    .tab_stop(true)
                                    .disabled(!matches!(
                                        sidebar.connection,
                                        crate::api::Connection::Online
                                    ))
                                    .on_click(cx.listener(
                                        |panel, _, _, cx| panel.send(Command::StopRadio, cx),
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .child(Label::new("Volume (only you)").text_xs())
                                        .child(
                                            Label::new(volume_percent)
                                                .text_xs()
                                                .text_color(theme.muted_foreground),
                                        ),
                                )
                                .child(
                                    Slider::new(&self.volume)
                                        .w_full()
                                        .tab_stop(true)
                                        .accessibility_label("Radio volume (only you)"),
                                ),
                        )
                });
        let feedback = radio
            .directory_error
            .map(|error| (error, theme.danger))
            .or_else(|| {
                let message = if display_loading {
                    "Loading…"
                } else if entries.is_some() && stations.is_empty() {
                    if self.favorites {
                        "No Favorites match."
                    } else {
                        "No stations found."
                    }
                } else {
                    return None;
                };
                Some((message, theme.muted_foreground))
            });
        let results = div()
            .id("stations")
            .role(Role::Group)
            .aria_label("Radio stations")
            .flex()
            .flex_col()
            .h_full()
            .gap(px(4.0))
            .children(feedback.map(|(message, color)| Label::new(message).text_color(color)))
            .children(
                stations
                    .into_iter()
                    .enumerate()
                    .map(|(index, station)| MediaResult {
                        id: station.id.clone().into(),
                        index: index + usize::from(feedback.is_some()),
                        scroll: self.station_scroll.clone(),
                        content: self.station_row(station, sidebar, cx).into_any_element(),
                    }),
            );

        let panel = div()
            .id("channel-radio")
            .flex()
            .flex_col()
            .gap(px(12.0))
            .max_h(window.viewport_size().height - px(40.0))
            .overflow_y_scroll()
            .track_scroll(&self.panel_scroll)
            .child(
                div().flex().flex_shrink_0().items_center().gap_2()
                    .child(themed_icon(IconName::Radio, theme.primary, 16.0))
                    .child(Label::new("Channel Radio").font_semibold()),
            )
            .when(radio.station.is_none(), |body| {
                body.child(Label::new("Find a station to play in this channel.").flex_shrink_0().text_color(theme.muted_foreground))
            })
            .children(player)
            .child(div().flex_shrink_0().h(px(1.0)).bg(theme.border))
            .child(div().flex().flex_shrink_0().gap_2()
                .child(control_button("all-stations").ghost().small().label("All stations").selected(!self.favorites).tab_stop(true)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        panel.favorites = false;
                        cx.notify();
                    })))
                .child(icon_label_button(control_button("favorites").ghost().small(), IconName::Star, "Favorites").selected(self.favorites).tab_stop(true)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        let load = matches!(&panel.spk.read(cx).view, View::SignedIn(sidebar) if sidebar.radio.favorites.is_none());
                        panel.favorites = true;
                        if load { panel.send(Command::RadioFavorites, cx); }
                        cx.notify();
                    })))
            )
            .child(div().flex().flex_shrink_0().gap_2()
                .child(Input::new(&self.query).flex_1().min_w_0().aria_label(if self.favorites { "Filter Favorites" } else { "Search radio stations" }))
                .when(!self.favorites, |row| row.child(
                    tool_button("search", IconName::Search, "Find stations").primary()
                        .on_click(cx.listener(|panel, _, _, cx| panel.search(cx)))
                ))
            )
            .child(media_results(&self.station_scroll, results).h(px(240.0)).min_h(px(52.0)))
            .child(
                div().flex().flex_shrink_0().items_center().justify_between().gap_2()
                    .child(
                        Button::new("radio-browser").link().small().px_0()
                            .label("Stations from Radio Browser")
                            .tab_stop(true)
                            .on_click(|_, _, cx| cx.open_url("https://www.radio-browser.info/")),
                    )
                    .child(div().w(px(128.0)).min_w_0()
                        .child(Label::new("Media Output").text_xs().text_color(theme.muted_foreground))
                        .child(device_picker(sidebar, DeviceKind::MediaOutput, &self.spk))),
            );

        div()
            .relative()
            .w_full()
            .child(panel)
            .child(overlay_scrollbar(&self.panel_scroll))
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, RadioAction};

    #[test]
    fn primary_radio_action_toggles_listening_when_a_station_is_selected() {
        assert!(RadioAction::for_primary(false, true).is_none());
        assert!(matches!(
            RadioAction::for_primary(true, true).map(RadioAction::command),
            Some(Command::PauseRadio)
        ));
        assert!(matches!(
            RadioAction::for_primary(true, false).map(RadioAction::command),
            Some(Command::ListenToRadio)
        ));
    }
}
