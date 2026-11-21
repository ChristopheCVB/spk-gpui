use std::{collections::VecDeque, sync::Mutex};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::*;
use crate::{
    api::{SpkClient, VoiceSocket},
    voice::{Call, Intent},
};

pub(crate) fn start_call(
    peers: UnboundedSender<Peer>,
) -> impl Fn(String, SpkClient, VoiceSocket, Intent) -> Call {
    move |channel, client, socket, intent| {
        let peers = peers.clone();
        Call::spawn(channel, socket, intent, |worker, ack, cancelled| {
            tokio::spawn(worker.run(
                client,
                ack,
                cancelled,
                async move |worker: &Worker, media: &mut Media, token| {
                    assert_eq!(token, "fixture-token");
                    let (room, peer) = TestRoom::new();
                    let room = media.fixture.insert(room);
                    let devices = room.devices();
                    let speakers = route_speakers(
                        &devices.speakers,
                        worker.requested.borrow().preferred.speakers.as_ref(),
                        None,
                        |device| room.switch_speakers(device),
                    )
                    .ok_or(Issue::Playback)?;
                    peers.send(peer).ok();
                    Media::listen(worker, room, &speakers).await
                },
            ))
        })
    }
}

struct State {
    open: bool,
    permission: bool,
    devices: Devices,
    capture: Option<Device>,
    muted: bool,
    speakers: Option<Device>,
    hardware_format: Option<(u32, u32)>,
    output_format: Option<(u32, u32)>,
    subscribed: bool,
    stats: VecDeque<SessionStats>,
}

impl State {
    fn format(&self) -> (u32, u32) {
        self.hardware_format.unwrap_or_else(|| {
            if self
                .capture
                .as_ref()
                .is_some_and(|mic| mic.id == "default-mic")
            {
                (24_000, 1)
            } else {
                (48_000, 2)
            }
        })
    }
}

pub(crate) struct Peer {
    state: Arc<Mutex<State>>,
    playout_changed: Arc<tokio::sync::Notify>,
    events: UnboundedSender<MediaEvent>,
}

impl Peer {
    pub(crate) fn allow_microphone(&self) {
        self.state.lock().unwrap().permission = true;
    }

    pub(crate) fn plug_preferred_devices(&self, connected: bool) {
        let mut state = self.state.lock().unwrap();
        state.devices.microphones.choices =
            connected.then(Self::preferred_mic).into_iter().collect();
        state.devices.speakers.choices = connected
            .then(Self::preferred_speakers)
            .into_iter()
            .collect();
    }

    pub(crate) fn set_hardware_format(&self, format: Option<(u32, u32)>) {
        self.state.lock().unwrap().hardware_format = format;
    }

    pub(crate) async fn wait_for_playout(&self, format: (u32, u32)) {
        while self.state.lock().unwrap().output_format != Some(format) {
            self.playout_changed.notified().await;
        }
    }

    pub(crate) fn preferred_mic() -> Device {
        device("preferred-mic")
    }

    pub(crate) fn preferred_speakers() -> Device {
        device("preferred-speakers")
    }

    pub(crate) fn speaking(&self, identities: &[&str]) {
        self.events
            .send(MediaEvent::Speaking(
                identities.iter().map(|id| (*id).into()).collect(),
            ))
            .ok()
            .unwrap();
    }

    pub(crate) fn quality(&self, quality: ConnectionQuality) {
        self.events
            .send(MediaEvent::Quality(quality))
            .unwrap_or_else(|_| panic!("voice fixture closed"));
    }

    pub(crate) fn report_stats(&self, stats: SessionStats) {
        self.state.lock().unwrap().stats.push_back(stats);
    }

    pub(crate) fn reconnecting(&self) {
        self.events.send(MediaEvent::Reconnecting).ok().unwrap();
    }

    pub(crate) fn reconnected(&self) {
        self.events.send(MediaEvent::Reconnected).ok().unwrap();
    }

    pub(crate) fn disconnect(&self) {
        self.events
            .send(MediaEvent::Disconnected(Issue::LiveKitDisconnected))
            .ok()
            .unwrap();
    }

    pub(crate) fn exchange(&self, microphone: i16, incoming: i16) -> (Option<i16>, Option<i16>) {
        let state = self.state.lock().unwrap();
        (
            (state.open && state.capture.is_some() && !state.muted).then_some(microphone),
            (state.open && state.subscribed && state.output_format == Some(state.format()))
                .then_some(incoming),
        )
    }

    pub(crate) fn assert_released(&self) {
        let state = self.state.lock().unwrap();
        assert!(!state.open);
        assert!(state.capture.is_none());
        assert!(state.output_format.is_none());
        drop(state);
        assert_eq!(self.exchange(1, -1), (None, None));
    }
}

fn device(id: &str) -> Device {
    Device {
        id: id.into(),
        name: id.into(),
    }
}

pub(super) struct TestRoom {
    state: Arc<Mutex<State>>,
    playout_changed: Arc<tokio::sync::Notify>,
    events: UnboundedReceiver<MediaEvent>,
    selected: Option<Device>,
    active: bool,
    starting: bool,
}

impl TestRoom {
    fn new() -> (Self, Peer) {
        let (send, events) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(State {
            open: true,
            permission: false,
            devices: Devices {
                microphones: crate::voice::DeviceList {
                    default: Some(device("default-mic")),
                    choices: vec![Peer::preferred_mic()],
                },
                speakers: crate::voice::DeviceList {
                    default: Some(device("default-speakers")),
                    choices: vec![Peer::preferred_speakers()],
                },
                ..Devices::default()
            },
            capture: None,
            muted: true,
            speakers: None,
            hardware_format: None,
            output_format: None,
            subscribed: false,
            stats: VecDeque::new(),
        }));
        let playout_changed = Arc::new(tokio::sync::Notify::new());
        let peer = Peer {
            state: state.clone(),
            playout_changed: playout_changed.clone(),
            events: send,
        };
        (
            Self {
                state,
                playout_changed,
                events,
                selected: None,
                active: false,
                starting: false,
            },
            peer,
        )
    }

    pub(super) fn stop_audio(&mut self) {
        self.clear_microphone();
        self.set_playout(false);
    }

    pub(super) fn close(self) {
        let mut state = self.state.lock().unwrap();
        assert!(state.capture.is_none());
        assert!(state.output_format.is_none());
        state.open = false;
    }

    fn clear_microphone(&mut self) {
        self.selected = None;
        self.active = false;
        self.starting = false;
        let mut state = self.state.lock().unwrap();
        state.capture = None;
        state.muted = true;
    }
}

impl RoomAudio for TestRoom {
    type Event = MediaEvent;
    type PlayoutFormat = (u32, u32);

    fn identity(&self) -> String {
        "alice".to_owned()
    }

    fn devices(&self) -> Devices {
        self.state.lock().unwrap().devices.clone()
    }

    fn switch_speakers(&self, device: &Device) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.speakers.as_ref() != Some(device) {
            state.speakers = Some(device.clone());
            if state.output_format.is_some() {
                state.output_format = Some(state.format());
            }
        }
        true
    }

    fn set_playout(&self, enabled: bool) {
        let mut state = self.state.lock().unwrap();
        if enabled {
            // An initialized stream keeps its old format until explicitly reopened.
            if state.output_format.is_none() {
                state.output_format = Some(state.format());
                self.playout_changed.notify_one();
            }
        } else {
            state.output_format = None;
        }
    }

    fn sync_remote_audio(&mut self, _: &Intent, _: &Devices, _: Option<&Device>) {
        self.state.lock().unwrap().subscribed = true;
    }

    fn playout_format(&self, _: &Device) -> Option<Self::PlayoutFormat> {
        Some(self.state.lock().unwrap().format())
    }

    fn macos_recording_initialized(&self) -> bool {
        self.active
    }

    fn selected_microphone(&self) -> Option<&Device> {
        self.selected.as_ref()
    }

    fn active_microphone(&self) -> Option<&Device> {
        self.selected.as_ref().filter(|_| self.active)
    }

    fn can_start_microphone(&self) -> bool {
        self.selected.is_none()
    }

    fn microphone_unavailable(&self) -> bool {
        !self.starting && !self.active
    }

    fn start_microphone(&mut self, device: Device, _: VoiceProcessing, _: bool) {
        self.selected = Some(device);
        self.starting = true;
    }

    async fn stop_microphone(&mut self) {
        self.clear_microphone();
    }

    fn silence_microphone(&self) {
        self.state.lock().unwrap().muted = true;
    }

    fn resume_microphone(&self) {
        let mut state = self.state.lock().unwrap();
        state.muted = state.capture.is_none();
    }

    async fn reconcile_microphone(&mut self) {}

    fn start_stats(&self, jobs: &mut JoinSet<Option<SessionStats>>) {
        if let Some(stats) = self.state.lock().unwrap().stats.pop_front() {
            jobs.spawn(async move { Some(stats) });
        }
    }

    async fn next_event(&mut self, reconnecting: bool) -> MediaEvent {
        tokio::select! {
            biased;
            event = self.events.recv() => event.unwrap_or(MediaEvent::Disconnected(Issue::LiveKitDisconnected)),
            () = std::future::ready(()), if self.starting && !reconnecting => MediaEvent::MicrophoneChanged,
        }
    }

    async fn handle_event(&mut self, event: MediaEvent) -> MediaEvent {
        if matches!(event, MediaEvent::MicrophoneChanged) {
            self.starting = false;
            let mut state = self.state.lock().unwrap();
            self.active = state.permission;
            state.capture = self.selected.clone().filter(|_| self.active);
            state.muted = !self.active;
        }
        event
    }
}
