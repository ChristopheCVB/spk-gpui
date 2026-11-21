use super::{voice::device_options, *};
use crate::{api::RadioStation, session::RadioPlayback};
use gpui_kit::component::Disableable;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};

struct RadioPanel {
    spk: Entity<Spk>,
    query: Entity<InputState>,
    volume: Entity<SliderState>,
    favorites: bool,
}

impl Spk {
    pub(super) fn open_radio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        self.device_picker = None;
        let volume = f32::from(sidebar.radio.volume);
        let spk = cx.entity();
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
            let volume = cx.new(|_| SliderState::new().max(200.0).default_value(volume));
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
            }
        });
        self.send(Command::RadioFavorites, None, cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .centered()
                .width(px(460.0))
                .title("Channel Radio")
                .child(panel.clone())
        });
    }
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
            .id(SharedString::from(id.clone()))
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let retry = radio.playback == RadioPlayback::Unavailable;
        let running = radio.enabled && !retry;
        let status = if !radio.enabled {
            "Paused for you"
        } else if retry {
            "This stream could not play. Retry or choose another station."
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

        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(radio.station.as_ref().map(|station| {
                div().flex().flex_col().gap_2()
                    .child(Label::new(station.name.clone()).font_semibold().truncate())
                    .child(Label::new(status).text_xs().text_color(theme.muted_foreground))
                    .child(div().flex().gap_2()
                        .child(
                            Button::new("playback").label(if retry { "Retry" } else if running { "Pause" } else { "Resume" })
                                .tab_stop(true)
                                .on_click(cx.listener(move |panel, _, _, cx| panel.send(
                                    if running { Command::PauseRadio } else { Command::ListenToRadio }, cx
                                ))),
                        )
                        .child(
                            Button::new("stop").ghost().label("Stop for everyone").tab_stop(true)
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
                .child(Button::new("all-stations").label("All stations").selected(!self.favorites).tab_stop(true)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        panel.favorites = false;
                        cx.notify();
                    })))
                .child(Button::new("favorites").label("Favorites").selected(self.favorites).tab_stop(true)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        let load = matches!(&panel.spk.read(cx).view, View::SignedIn(sidebar) if sidebar.radio.favorites.is_none());
                        panel.favorites = true;
                        if load { panel.send(Command::RadioFavorites, cx); }
                        cx.notify();
                    })))
            )
            .child(div().flex().gap_2()
                .child(Input::new(&self.query).w_full().aria_label(if self.favorites { "Filter Favorites" } else { "Search radio stations" }))
                .when(!self.favorites, |row| row.child(
                    Button::new("search").label("Search").tab_stop(true)
                        .on_click(cx.listener(|panel, _, _, cx| panel.search(cx)))
                ))
            )
            .children(radio.directory_error.map(|error| Label::new(error).text_color(theme.danger)))
            .when(display_loading, |body| body.child(Label::new("Loading…").text_color(theme.muted_foreground)))
            .when(!display_loading && entries.is_some() && stations.is_empty(), |body| {
                body.child(Label::new(if self.favorites { "No Favorites match." } else { "No stations found." })
                    .text_color(theme.muted_foreground))
            })
            .child(div().id("stations").flex().flex_col().max_h(px(224.0)).overflow_y_scroll()
                .children(stations.into_iter().map(|station| self.station_row(station, sidebar, cx))))
            .child(
                div().child(Label::new("Media Output").text_xs())
                    .child(device_options(sidebar, DeviceKind::MediaOutput, &self.spk, cx)),
            )
    }
}
