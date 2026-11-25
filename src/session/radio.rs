use std::future::pending;

use tokio::task::{AbortHandle, Id, JoinSet};
use uuid::Uuid;

use super::{ActionResult, Command, Sidebar, VoiceSession};
use crate::{
    api::{Failure, RadioStation, SpkClient, VoiceFrame},
    voice::{Device, MediaOutput, radio::RadioPlayer},
};

pub use crate::voice::radio::RadioPlayback;

#[derive(Clone)]
pub struct RadioView {
    pub station: Option<RadioStation>,
    pub stations: Option<Vec<RadioStation>>,
    pub favorites: Option<Vec<RadioStation>>,
    pub searching: bool,
    pub loading_favorites: bool,
    pub enabled: bool,
    pub volume: u8,
    pub playback: RadioPlayback,
    pub directory_error: Option<&'static str>,
}

impl Default for RadioView {
    fn default() -> Self {
        Self {
            station: None,
            stations: None,
            favorites: None,
            searching: false,
            loading_favorites: false,
            enabled: true,
            volume: 100,
            playback: RadioPlayback::Idle,
            directory_error: None,
        }
    }
}

pub(super) enum RadioResult {
    Search(Result<Vec<RadioStation>, Failure>),
    Favorites(Result<Vec<RadioStation>, Failure>),
    Favorite(String, Result<Option<RadioStation>, Failure>),
    Sent(String, Result<(), Failure>),
}

#[derive(Default)]
pub(super) struct RadioSession {
    scope: Option<String>,
    revision: Option<u64>,
    pub(super) player: Option<RadioPlayer>,
    player_device: Option<Device>,
    search: Option<AbortHandle>,
    pub tasks: JoinSet<RadioResult>,
}

impl RadioSession {
    pub fn sync(&mut self, sidebar: &mut Sidebar, voice: &VoiceSession) {
        let scope = voice.radio_scope(sidebar).map(|(sid, _)| sid);
        if self.scope.as_deref() != scope {
            self.scope = scope.map(str::to_owned);
            self.revision = None;
            self.player = None;
            sidebar.radio.station = None;
        }
        let preferences = &sidebar.voice_intent.preferred;
        sidebar.radio.volume = preferences.radio_volume.min(200);
        let output = MediaOutput {
            volume: if sidebar.voice_intent.mute.deafened {
                0.0
            } else {
                preferences.output_gain() * f32::from(sidebar.radio.volume) / 100.0
            },
            ..sidebar.media_output()
        };
        if sidebar.radio.enabled
            && let Some(station) = &sidebar.radio.station
        {
            let route_changed = self.player_device != output.device;
            if self.player.is_none()
                || route_changed && sidebar.radio.playback == RadioPlayback::Unavailable
            {
                self.player = Some(RadioPlayer::start(station.url.clone(), output.clone()));
            }
            self.player_device = output.device.clone();
            let player = self.player.as_mut().expect("station requested");
            player.set_output(output.clone());
            sidebar.radio.playback = *player.playback.borrow_and_update();
        } else {
            self.player = None;
            sidebar.radio.playback = RadioPlayback::Idle;
        }
        voice.set_radio_listening(
            sidebar,
            sidebar.radio.playback == RadioPlayback::Playing && output.volume > 0.0,
        );
    }

    pub async fn playback_changed(player: &mut Option<RadioPlayer>) {
        if let Some(player) = player
            && player.playback.changed().await.is_ok()
        {
            return;
        }
        pending().await
    }

    pub fn state(&mut self, sidebar: &mut Sidebar, revision: u64, station: Option<RadioStation>) {
        if self.revision.is_some_and(|current| revision <= current) {
            return;
        }
        self.revision = Some(revision);
        if sidebar.radio.station.as_ref().map(|station| &station.url)
            != station.as_ref().map(|station| &station.url)
        {
            self.player = None;
        }
        sidebar.radio.station = station;
    }

    pub fn command(
        &mut self,
        sidebar: &mut Sidebar,
        voice: &VoiceSession,
        client: &SpkClient,
        command: Command,
    ) {
        let client = client.clone();
        match command {
            Command::SearchRadio(query) => {
                if let Some(previous) = self.search.take() {
                    previous.abort();
                }
                let query = query.trim().to_owned();
                sidebar.radio.stations = None;
                sidebar.radio.directory_error = None;
                sidebar.radio.searching = (2..=100).contains(&query.encode_utf16().count());
                if sidebar.radio.searching {
                    self.search = Some(self.tasks.spawn(async move {
                        RadioResult::Search(client.radio_stations(&query).await)
                    }));
                } else {
                    sidebar.radio.directory_error = Some("Search must contain 2–100 characters.");
                }
            }
            Command::RadioFavorites if !sidebar.radio.loading_favorites => {
                sidebar.radio.loading_favorites = true;
                sidebar.radio.directory_error = None;
                self.tasks
                    .spawn(async move { RadioResult::Favorites(client.radio_favorites().await) });
            }
            Command::SetRadioFavorite { id, saved }
                if !sidebar.radio.loading_favorites && sidebar.radio.favorites.is_some() =>
            {
                sidebar.radio.loading_favorites = true;
                self.tasks.spawn(async move {
                    let result = client.set_radio_favorite(&id, saved).await;
                    RadioResult::Favorite(id, result)
                });
            }
            Command::SelectRadio(_) | Command::StopRadio => {
                let Some((socket, sid)) = voice.active_socket(sidebar) else {
                    sidebar.action_results.push(ActionResult::error(
                        "Radio controls are offline. Wait for reconnection.",
                    ));
                    return;
                };
                let station = match command {
                    Command::SelectRadio(id) => {
                        let Ok(id) = Uuid::parse_str(&id) else {
                            sidebar
                                .action_results
                                .push(ActionResult::error("Could not select this radio station."));
                            return;
                        };
                        sidebar.radio.enabled = true;
                        Some(id.to_string())
                    }
                    _ => None,
                };
                self.tasks.spawn(async move {
                    let frame = match &station {
                        Some(station_id) => VoiceFrame::RadioSet {
                            sid: &sid,
                            station_id,
                        },
                        None => VoiceFrame::RadioStop { sid: &sid },
                    };
                    let result = socket.send(frame).await;
                    RadioResult::Sent(sid, result)
                });
            }
            Command::PauseRadio => sidebar.radio.enabled = false,
            Command::ListenToRadio => {
                sidebar.radio.enabled = true;
                self.player = None;
            }
            _ => {}
        }
    }

    pub fn complete(
        &self,
        sidebar: &mut Sidebar,
        id: Id,
        result: RadioResult,
    ) -> Result<(), Failure> {
        let radio = &mut sidebar.radio;
        match result {
            RadioResult::Search(result)
                if self.search.as_ref().is_some_and(|search| search.id() == id) =>
            {
                radio.searching = false;
                radio.stations = Some(result?);
            }
            RadioResult::Search(_) => {}
            RadioResult::Favorites(result) => {
                radio.loading_favorites = false;
                radio.favorites = Some(result?);
            }
            RadioResult::Favorite(id, result) => {
                radio.loading_favorites = false;
                let favorite = match result {
                    Ok(favorite) => favorite,
                    Err(error) if error.unauthorized() => return Err(error),
                    Err(_) => {
                        sidebar.action_results.push(ActionResult::error(
                            "Could not update radio Favorites. Try again.",
                        ));
                        return Ok(());
                    }
                };
                let favorites = radio.favorites.get_or_insert_default();
                favorites.retain(|station| station.id != id);
                favorites.extend(favorite);
                favorites.sort_by_cached_key(|station| station.name.to_lowercase());
            }
            RadioResult::Sent(sid, result) => {
                if result.is_err() && self.scope.as_deref() == Some(&sid) {
                    sidebar.action_results.push(ActionResult::error(
                        "Could not send the radio command. Try again.",
                    ));
                }
            }
        }
        Ok(())
    }
}
