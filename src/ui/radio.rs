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
            .gap_1()
            .items_center()
            .child(
                Button::new("select")
                    .ghost()
                    .selected(selected)
                    .flex_1()
                    .min_w_0()
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
                            .flex_col()
                            .child(Label::new(station.name.clone()).truncate())
                            .child(
                                Label::new(if station.available == Some(false) {
                                    "Unavailable".to_owned()
                                } else {
                                    format!("{} · {}", station.country, station.codec)
                                })
                                .text_xs()
                                .text_color(cx.theme().muted_foreground),
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
        let reserved_height = if radio.station.is_some() {
            365.0
        } else {
            275.0
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

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().items_center().gap_2()
                    .child(themed_icon(IconName::Radio, theme.primary, 16.0))
                    .child(Label::new("Channel Radio").font_semibold()),
            )
            .when(radio.station.is_none(), |body| {
                body.child(Label::new("Find a station to play in this channel.").text_color(theme.muted_foreground))
            })
            .children(radio.station.as_ref().zip(playback_action).map(|(station, action)| {
                div().flex().flex_col().gap_2()
                    .child(Label::new(station.name.clone()).font_semibold().truncate())
                    .child(Label::new(status).text_xs().text_color(theme.muted_foreground))
                    .child(div().flex().gap_2()
                        .child(
                            Button::new("playback").small().label(action.label())
                                .tab_stop(true)
                                .on_click(cx.listener(move |panel, _, _, cx| panel.send(action.command(), cx))),
                        )
                        .child(
                            Button::new("stop").ghost().small().label("Stop for everyone").tab_stop(true)
                                .disabled(!matches!(sidebar.connection, crate::api::Connection::Online))
                                .on_click(cx.listener(|panel, _, _, cx| panel.send(Command::StopRadio, cx))),
                        )
                    )
                    .child(div().flex().items_center().gap_2()
                        .child(Label::new("Volume (only you)").text_xs())
                        .child(Slider::new(&self.volume).flex_1().tab_stop(true).accessibility_label("Radio volume (only you)"))
                        .child(Label::new(format!("{:.0}%", self.volume.read(cx).value().start())).text_xs())
                    )
            }))
            .children(radio.command_error.as_ref().map(|error| Label::new(error.clone()).text_color(theme.danger)))
            .child(div().flex().gap_2()
                .child(Button::new("all-stations").small().label("All stations").selected(!self.favorites).tab_stop(true)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        panel.favorites = false;
                        cx.notify();
                    })))
                .child(Button::new("favorites").small().icon(IconName::Star).label("Favorites").selected(self.favorites).tab_stop(true)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        let load = matches!(&panel.spk.read(cx).view, View::SignedIn(sidebar) if sidebar.radio.favorites.is_none());
                        panel.favorites = true;
                        if load { panel.send(Command::RadioFavorites, cx); }
                        cx.notify();
                    })))
            )
            .child(div().flex().gap_2()
                .child(Input::new(&self.query).flex_1().min_w_0().aria_label(if self.favorites { "Filter Favorites" } else { "Search radio stations" }))
                .when(!self.favorites, |row| row.child(
                    tool_button("search", IconName::Search, "Find stations").primary()
                        .on_click(cx.listener(|panel, _, _, cx| panel.search(cx)))
                ))
            )
            .children(radio.directory_error.map(|error| Label::new(error).text_color(theme.danger)))
            .when(display_loading, |body| body.child(Label::new("Loading…").text_color(theme.muted_foreground)))
            .when(!display_loading && entries.is_some() && stations.is_empty(), |body| {
                body.child(Label::new(if self.favorites { "No Favorites match." } else { "No stations found." })
                    .text_color(theme.muted_foreground))
            })
            .child(media_results(
                &self.station_scroll,
                div().id("stations").flex().flex_col()
                    .max_h((window.viewport_size().height - px(reserved_height)).min(px(224.0)))
                    .children(stations.into_iter().enumerate().map(|(index, station)| MediaResult {
                        id: station.id.clone().into(),
                        index,
                        scroll: self.station_scroll.clone(),
                        content: self.station_row(station, sidebar, cx).into_any_element(),
                    })),
            ))
            .child(
                div().child(Label::new("Media Output").text_xs())
                    .child(device_picker(sidebar, DeviceKind::MediaOutput, &self.spk)),
            )
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
