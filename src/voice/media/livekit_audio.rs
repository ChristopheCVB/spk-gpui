use super::*;

pub(super) struct LiveKitAudio<'a> {
    pub(super) room: &'a Arc<Room>,
    pub(super) audio: &'a PlatformAudio,
    pub(super) events: RoomEvents,
    pub(super) capture: &'a mut Capture,
}

pub(super) enum Event {
    Media(MediaEvent),
    Published(Option<TrackSid>),
    Unpublish(TrackSid),
}

impl RoomAudio for LiveKitAudio<'_> {
    type Event = Event;
    #[cfg(target_os = "macos")]
    type PlayoutFormat = super::macos_audio::PlayoutFormat;
    #[cfg(not(target_os = "macos"))]
    type PlayoutFormat = ();

    fn identity(&self) -> String {
        self.room.local_participant().identity().to_string()
    }

    fn devices(&self) -> Devices {
        Devices::read(self.audio)
    }

    fn switch_speakers(&self, device: &Device) -> bool {
        switch_speakers(self.audio, device)
    }

    fn set_playout(&self, enabled: bool) {
        LkRuntime::instance()
            .pc_factory()
            .set_adm_playout_enabled(enabled);
    }

    fn subscribe_microphones(&self) {
        for participant in self.room.remote_participants().into_values() {
            for publication in participant.track_publications().into_values() {
                subscribe_to_microphone(&publication);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn playout_format(&self, device: &Device) -> Option<Self::PlayoutFormat> {
        super::macos_audio::playout_format(device)
    }

    #[cfg(not(target_os = "macos"))]
    fn playout_format(&self, _: &Device) -> Option<Self::PlayoutFormat> {
        None
    }

    fn macos_recording_initialized(&self) -> bool {
        cfg!(target_os = "macos") && self.audio.is_recording_initialized()
    }

    fn selected_microphone(&self) -> Option<&Device> {
        self.capture.selected_device()
    }

    fn active_microphone(&self) -> Option<&Device> {
        self.capture.active_device()
    }

    fn can_start_microphone(&self) -> bool {
        self.capture.can_start()
    }

    fn microphone_unavailable(&self) -> bool {
        self.capture.unavailable()
    }

    fn start_microphone(&mut self, device: Device) {
        self.capture.start(self.room, self.audio, device);
    }

    async fn stop_microphone(&mut self) {
        self.capture.stop(self.room, self.audio).await;
    }

    fn silence_microphone(&self) {
        self.capture.silence();
    }

    fn resume_microphone(&self) {
        self.capture.resume();
    }

    async fn reconcile_microphone(&mut self) {
        self.capture
            .reconcile_publication(self.room, self.audio)
            .await;
    }

    fn start_stats(&self, jobs: &mut JoinSet<Option<SessionStats>>) {
        let room = self.room.clone();
        jobs.spawn(async move {
            tokio::time::timeout(Duration::from_secs(1), room.get_stats())
                .await
                .ok()?
                .ok()
        });
    }

    async fn next_event(&mut self, reconnecting: bool) -> Event {
        tokio::select! {
            biased;
            event = self.events.recv() => self.room_event(event),
            Some(published) = self.capture.next_publication(), if !reconnecting => Event::Published(published),
        }
    }

    async fn handle_event(&mut self, event: Event) -> MediaEvent {
        match event {
            Event::Published(published) => {
                self.capture
                    .complete(self.room, self.audio, published)
                    .await;
                MediaEvent::MicrophoneChanged
            }
            Event::Unpublish(sid) => {
                let _ = self.room.local_participant().unpublish_track(&sid).await;
                MediaEvent::Other
            }
            Event::Media(event) => event,
        }
    }
}

impl LiveKitAudio<'_> {
    fn room_event(&self, event: Option<RoomEvent>) -> Event {
        let event = match event {
            Some(RoomEvent::TrackPublished { publication, .. }) => {
                subscribe_to_microphone(&publication);
                MediaEvent::Other
            }
            Some(RoomEvent::ActiveSpeakersChanged { speakers }) => MediaEvent::Speaking(
                speakers
                    .into_iter()
                    .map(|speaker| speaker.identity().to_string())
                    .collect(),
            ),
            Some(RoomEvent::ParticipantDisconnected(participant)) => {
                MediaEvent::ParticipantLeft(participant.identity().to_string())
            }
            Some(RoomEvent::LocalTrackRepublished { publication, .. })
                if publication.source() == TrackSource::Microphone
                    && !self.capture.owns_track(&publication.sid()) =>
            {
                // The SDK can be republishing while mute or deafen removes the old track.
                return Event::Unpublish(publication.sid());
            }
            Some(RoomEvent::ConnectionQualityChanged {
                quality,
                participant: Participant::Local(_),
            }) => MediaEvent::Quality(quality),
            Some(RoomEvent::Reconnecting) => MediaEvent::Reconnecting,
            Some(RoomEvent::Reconnected) => MediaEvent::Reconnected,
            Some(RoomEvent::Disconnected { reason }) => {
                MediaEvent::Disconnected(disconnect_issue(reason))
            }
            None => MediaEvent::Disconnected(Issue::LiveKitDisconnected),
            _ => MediaEvent::Other,
        };
        Event::Media(event)
    }
}

fn subscribe_to_microphone(publication: &RemoteTrackPublication) {
    if publication.kind() == TrackKind::Audio && publication.source() == TrackSource::Microphone {
        publication.set_subscribed(true);
    }
}
