use super::{remote_audio::RemoteAudio, *};

pub(super) struct LiveKitAudio<'a> {
    pub(super) room: &'a Arc<Room>,
    pub(super) audio: &'a PlatformAudio,
    pub(super) events: RoomEvents,
    pub(super) capture: &'a mut Capture,
    pub(super) remote: RemoteAudio,
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

    async fn devices(&self) -> Result<Devices, Issue> {
        Devices::scan(self.audio).await
    }

    fn switch_speakers(&self, device: &Device) -> bool {
        switch_speakers(self.audio, device)
    }

    fn set_playout(&self, enabled: bool) {
        LkRuntime::instance()
            .pc_factory()
            .set_adm_playout_enabled(enabled && self.remote.native_playout);
    }

    fn sync_remote_audio(
        &mut self,
        intent: &Intent,
        devices: &Devices,
        speakers: Option<&Device>,
    ) -> bool {
        self.remote.sync(self.room, intent, devices, speakers)
    }

    fn voice_output_healthy_since(&self) -> Option<Instant> {
        self.remote.voice_output_healthy_since()
    }

    fn suspend_remote_audio(&mut self) {
        self.remote.suspend();
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

    fn start_microphone(
        &mut self,
        device: Device,
        processing: VoiceProcessing,
        use_system_default: bool,
    ) {
        self.capture.start(
            self.room,
            device,
            processing,
            use_system_default,
            self.remote.render_reference(),
        );
    }

    async fn sync_microphone_processing(&mut self, processing: VoiceProcessing) {
        self.capture.sync_processing(self.room, processing).await;
    }

    async fn stop_microphone(&mut self) {
        self.capture.stop(self.room).await;
    }

    fn silence_microphone(&self) {
        self.capture.silence();
    }

    fn resume_microphone(&self) {
        self.capture.resume();
    }

    async fn reconcile_microphone(&mut self) {
        self.capture.reconcile_publication(self.room).await;
    }

    fn sample_microphones(&self, expected: Vec<String>, jobs: &mut JoinSet<MicrophoneSamples>) {
        let room = self.room.clone();
        jobs.spawn(async move { remote_audio::sample_microphones(&room, expected).await });
    }

    async fn repair_microphones(&mut self, repairs: Vec<(String, TrackSid)>) {
        for (_, sid) in &repairs {
            self.remote.forget(sid);
        }
        remote_audio::repair_microphones(self.room, repairs).await;
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
                self.capture.complete(self.room, published).await;
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
    fn room_event(&mut self, event: Option<RoomEvent>) -> Event {
        let event = match event {
            Some(RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(track),
                publication,
                ..
            }) => {
                track.disable();
                self.remote.forget(&publication.sid());
                MediaEvent::RemoteAudioChanged
            }
            Some(
                RoomEvent::TrackPublished { .. }
                | RoomEvent::TrackUnpublished { .. }
                | RoomEvent::TrackUnsubscribed { .. }
                | RoomEvent::TrackMuted { .. }
                | RoomEvent::TrackUnmuted { .. }
                | RoomEvent::ParticipantConnected(_),
            ) => MediaEvent::RemoteAudioChanged,
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
