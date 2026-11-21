use std::{
    collections::{HashMap, HashSet},
    future::pending,
    sync::Arc,
    time::Duration,
};

use async_channel::{Receiver, Sender};
use livekit::rtc_engine::lk_runtime::LkRuntime;
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use livekit::{DisconnectReason, prelude::*};
use tokio::{
    sync::{oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior, interval, sleep_until, timeout},
};
use url::Url;
use uuid::Uuid;

use crate::api::{SpkClient, VoiceFlags, VoiceFrame, VoiceSocket};

mod devices;
pub use devices::{Device, DeviceKind, DeviceList, DevicePreferences, Devices};

#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows_audio;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Joining,
    Connected,
    ListenOnly,
    Reconnecting,
    Leaving,
}

#[derive(Clone, Copy)]
pub enum Issue {
    Unauthorized,
    Token,
    Endpoint,
    Socket,
    Acknowledgment,
    Playback,
    Media,
    Disconnected,
    LiveKitDisconnected,
}

impl Issue {
    pub fn message(self) -> &'static str {
        match self {
            Self::Unauthorized => "Your session expired. Sign in again.",
            Self::Token => "SPK could not issue a voice token. Try joining again.",
            Self::Endpoint => {
                "SPK returned a different LiveKit URL. Check the app's build configuration."
            }
            Self::Socket => "The SPK voice connection was lost. Join again when it reconnects.",
            Self::Acknowledgment => "SPK did not confirm the voice join. Try again.",
            Self::Playback => "Could not open speakers. Check your audio settings and try again.",
            Self::Media => "Could not connect to LiveKit. Try joining again.",
            Self::Disconnected => "Disconnected from voice. Join again to continue.",
            Self::LiveKitDisconnected => "The voice connection was lost.",
        }
    }
}

pub enum Update {
    Media(Arc<Report>),
    Finished(Result<(), Issue>),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Mute {
    pub muted: bool,
    pub deafened: bool,
}

impl Mute {
    pub fn toggle_mic(&mut self) {
        self.muted = !self.effective_muted();
        self.deafened = false;
    }

    pub fn toggle_deafen(&mut self) {
        self.deafened = !self.deafened;
    }

    pub fn effective_muted(self) -> bool {
        self.muted || self.deafened
    }
}

#[derive(Clone, Default)]
pub struct Intent {
    pub mute: Mute,
    pub preferred: DevicePreferences,
    pub retry_mic: u64,
    pub retry_speakers: u64,
    pub refresh_devices: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report {
    pub status: Status,
    pub devices: Arc<Devices>,
    pub microphone: Option<Device>,
    pub speakers: Option<Device>,
    pub speaking: HashSet<String>,
}

#[derive(Clone)]
struct Join {
    channel_id: String,
    sid: String,
    nonce: String,
}

impl Join {
    fn matches(&self, sid: &str, channel_id: &str, nonce: &str) -> bool {
        self.sid == sid && self.channel_id == channel_id && self.nonce == nonce
    }
}

pub struct Call {
    join: Join,
    acknowledgment: Option<oneshot::Sender<()>>,
    cancel: Option<oneshot::Sender<()>>,
    intent: watch::Sender<Intent>,
    updates: Receiver<Update>,
    worker: JoinHandle<()>,
}

impl Call {
    pub fn start(
        channel_id: String,
        client: SpkClient,
        socket: VoiceSocket,
        intent: Intent,
    ) -> Self {
        Self::spawn(channel_id, socket, intent, |worker, ack, cancelled| {
            tokio::spawn(worker.run(client, ack, cancelled, Worker::connect_and_listen))
        })
    }

    fn spawn(
        channel_id: String,
        socket: VoiceSocket,
        initial: Intent,
        start_worker: impl FnOnce(
            Worker,
            oneshot::Receiver<()>,
            oneshot::Receiver<()>,
        ) -> JoinHandle<()>,
    ) -> Self {
        let join = Join {
            channel_id,
            sid: Uuid::new_v4().to_string(),
            nonce: Uuid::new_v4().to_string(),
        };
        let (acknowledgment, ack) = oneshot::channel();
        let (cancel, cancelled) = oneshot::channel();
        let (notifications, updates) = async_channel::unbounded();
        let (intent, requested) = watch::channel(initial);
        let worker = start_worker(
            Worker {
                join: join.clone(),
                socket,
                updates: notifications,
                requested,
            },
            ack,
            cancelled,
        );

        Self {
            join,
            acknowledgment: Some(acknowledgment),
            cancel: Some(cancel),
            intent,
            updates,
            worker,
        }
    }

    pub fn apply(&self, intent: &Intent) {
        self.intent.send_replace(intent.clone());
    }

    pub fn acknowledge(&mut self, sid: &str, channel_id: &str, nonce: &str) {
        if self.join.matches(sid, channel_id, nonce)
            && let Some(acknowledgment) = self.acknowledgment.take()
        {
            let _ = acknowledgment.send(());
        }
    }

    pub fn owns(&self, sid: &str) -> bool {
        self.join.sid == sid
    }

    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }

    pub async fn stop(mut self) {
        self.cancel();
        let _ = self.worker.await;
    }
}

pub async fn next_update(call: Option<&Call>) -> Update {
    let Some(call) = call else {
        return pending().await;
    };
    call.updates
        .recv()
        .await
        .unwrap_or(Update::Finished(Err(Issue::Media)))
}

struct Worker {
    join: Join,
    socket: VoiceSocket,
    updates: Sender<Update>,
    requested: watch::Receiver<Intent>,
}

type RoomEvents = tokio::sync::mpsc::UnboundedReceiver<RoomEvent>;

#[derive(Default)]
struct Media {
    audio: Option<PlatformAudio>,
    capture: Capture,
    connecting: Option<JoinHandle<Result<(Room, RoomEvents), Issue>>>,
    room: Option<Room>,
    joined_deafened: bool,
}

impl Media {
    async fn stop_audio(&mut self) {
        self.capture.silence();
        self.capture.jobs.shutdown().await;
        if let Some(audio) = self.audio.take() {
            stop_recording(&audio);
            LkRuntime::instance()
                .pc_factory()
                .set_adm_playout_enabled(false);
        }
    }

    async fn close(self) {
        if let Some(room) = self.room {
            let _ = room.close().await;
        } else if let Some(connecting) = self.connecting
            && let Ok(Ok((room, _))) = connecting.await
        {
            let _ = room.close().await;
        }
    }
}

impl Worker {
    async fn run(
        self,
        client: SpkClient,
        ack: oneshot::Receiver<()>,
        mut cancelled: oneshot::Receiver<()>,
        start_media: impl AsyncFnOnce(&Self, &mut Media, String) -> Result<(), Issue>,
    ) {
        let mut claimed = false;
        let mut media = Media::default();

        let result = tokio::select! {
            biased;
            _ = &mut cancelled => Ok(()),
            result = async {
                let (token, deafened) = self.claim(&client, ack, &mut claimed).await?;
                media.joined_deafened = deafened;
                start_media(&self, &mut media, token).await
            } => result,
        };

        media.stop_audio().await;
        tokio::join!(
            async {
                if claimed {
                    let _ = self
                        .socket
                        .send(VoiceFrame::Leave {
                            sid: &self.join.sid,
                        })
                        .await;
                }
            },
            media.close(),
        );
        self.update(Update::Finished(result));
    }

    async fn claim(
        &self,
        client: &SpkClient,
        ack: oneshot::Receiver<()>,
        claimed: &mut bool,
    ) -> Result<(String, bool), Issue> {
        let Join {
            channel_id,
            sid,
            nonce,
        } = &self.join;
        let token = client.voice_token(channel_id).await.map_err(|error| {
            if error.unauthorized() {
                Issue::Unauthorized
            } else {
                Issue::Token
            }
        })?;
        let endpoint = Url::parse(env!("LIVEKIT_URL")).expect("validated LiveKit URL");
        if Url::parse(&token.url).as_ref().ok() != Some(&endpoint) {
            return Err(Issue::Endpoint);
        }
        if token.token.trim().is_empty() {
            return Err(Issue::Token);
        }
        // The write may have reached SPK even if cancellation interrupts its receipt.
        *claimed = true;
        let deafened = self.requested.borrow().mute.deafened;
        self.send(VoiceFrame::Join {
            sid,
            channel_id,
            nonce,
            muted: true,
            deafened,
            watching: [],
        })
        .await?;
        timeout(Duration::from_secs(5), ack)
            .await
            .map_err(|_| Issue::Acknowledgment)?
            .map_err(|_| Issue::Acknowledgment)?;
        Ok((token.token, deafened))
    }

    async fn connect_and_listen(&self, media: &mut Media, token: String) -> Result<(), Issue> {
        let audio = media
            .audio
            .insert(PlatformAudio::new().map_err(|_| Issue::Playback)?);
        stop_recording(audio);
        let requested = self.requested.borrow().clone();
        let devices = Devices::read(audio);
        let speakers =
            route_speakers(audio, &devices.speakers, &requested, None).ok_or(Issue::Playback)?;
        LkRuntime::instance()
            .pc_factory()
            .set_adm_playout_enabled(false);
        // Drive the SDK's join to completion so cancellation can close any room it returns.
        let connection = media.connecting.insert(tokio::spawn(connect(token)));
        let connected = connection.await.map_err(|_| Issue::Media)?;
        media.connecting.take();
        let (room, events) = connected?;
        self.listen(
            media.room.insert(room),
            audio,
            events,
            speakers,
            media.joined_deafened,
            &mut media.capture,
        )
        .await
    }

    async fn listen(
        &self,
        room: &Room,
        audio: &PlatformAudio,
        mut events: RoomEvents,
        speakers: Device,
        joined_deafened: bool,
        capture: &mut Capture,
    ) -> Result<(), Issue> {
        let mut requested = self.requested.clone();
        let mut intent = requested.borrow_and_update().clone();
        let runtime = LkRuntime::instance();
        let mut devices = Arc::new(Devices::read(audio));
        // LiveKit can initialize WebRTC with the default route while connecting.
        let mut output = route_speakers(audio, &devices.speakers, &intent, Some(&speakers));
        runtime
            .pc_factory()
            .set_adm_playout_enabled(output.is_some() && !intent.mute.deafened);
        subscribe_to_microphones(room);
        self.ready().await?;
        let me = room.local_participant().identity();
        let mut speaking = Speaking::default();
        let mut presence = Presence::new(joined_deafened);
        let mut reported = None;
        // shortcut: a remove and return between scans can be missed, use backend notifications when available.
        let mut scan = interval(Duration::from_secs(1));
        scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
        scan.tick().await;
        let mut check_routes = true;
        let mut retry_mic = intent.retry_mic;
        let mut retry_speakers = intent.retry_speakers;
        let mut reconnecting = false;
        let mut resync = false;

        loop {
            if reconnecting && intent.mute.effective_muted() && capture.device.is_some() {
                capture.stop(room, audio).await;
            }
            if check_routes && !reconnecting {
                check_routes = false;
                let input = devices
                    .microphones
                    .resolve(intent.preferred.microphone.as_ref());
                let wanted = if intent.mute.effective_muted() {
                    None
                } else {
                    input
                };
                if capture.device.as_ref() != wanted || retry_mic != intent.retry_mic {
                    capture.stop(room, audio).await;
                    speaking.remove(me.as_str());
                    retry_mic = intent.retry_mic;
                }
                if let Some(device) = wanted
                    && capture.device.is_none()
                    && capture.jobs.is_empty()
                {
                    capture.start(room, audio, device.clone());
                }

                if intent.mute.deafened {
                    runtime.pc_factory().set_adm_playout_enabled(false);
                }
                if devices.speakers.resolve(intent.preferred.speakers.as_ref()) != output.as_ref()
                    || retry_speakers != intent.retry_speakers
                    || resync
                {
                    retry_speakers = intent.retry_speakers;
                    output = route_speakers(audio, &devices.speakers, &intent, output.as_ref());
                }
                runtime
                    .pc_factory()
                    .set_adm_playout_enabled(output.is_some() && !intent.mute.deafened);
                if resync
                    && !intent.mute.effective_muted()
                    && capture.published.is_some()
                    && let Some(track) = &capture.track
                {
                    track.unmute();
                }
            }

            speaking.expire(Instant::now());
            let muted = reconnecting || capture.published.is_none();
            if muted {
                speaking.remove(me.as_str());
            }
            let flags = VoiceFlags {
                muted,
                deafened: intent.mute.deafened,
                speaking: speaking.0.contains_key(me.as_str()),
            };
            let report = Report {
                status: if reconnecting {
                    Status::Reconnecting
                } else if !intent.mute.effective_muted()
                    && (capture.failed || (capture.device.is_none() && capture.jobs.is_empty()))
                {
                    Status::ListenOnly
                } else {
                    Status::Connected
                },
                devices: devices.clone(),
                microphone: capture
                    .published
                    .as_ref()
                    .and(capture.device.as_ref())
                    .cloned(),
                speakers: output.clone(),
                speaking: speaking.0.keys().cloned().collect(),
            };
            if reported.as_ref() != Some(&report) {
                reported = Some(report.clone());
                self.update(Update::Media(Arc::new(report)));
            }
            if resync {
                self.ready().await?;
                resync = false;
                presence.resync();
            }
            if presence.due(flags).is_some_and(|at| at <= Instant::now()) {
                self.state(flags).await?;
                presence.sent(flags, Instant::now());
            }

            tokio::select! {
                biased;
                result = requested.changed() => {
                    result.map_err(|_| Issue::Disconnected)?;
                    let next = requested.borrow_and_update().clone();
                    if next.refresh_devices != intent.refresh_devices || next.preferred != intent.preferred
                        || next.retry_mic != intent.retry_mic || next.retry_speakers != intent.retry_speakers
                    {
                        devices = Arc::new(Devices::read(audio));
                    }
                    intent = next;
                    check_routes = true;
                }
                event = events.recv() => match event {
                    Some(RoomEvent::TrackPublished { publication, .. }) => subscribe_to_microphone(&publication),
                    Some(RoomEvent::ActiveSpeakersChanged { speakers }) if !reconnecting => speaking.update(
                        speakers.into_iter().map(|speaker| speaker.identity().to_string()).collect(),
                        Instant::now(),
                    ),
                    Some(RoomEvent::ParticipantDisconnected(participant)) => speaking.remove(participant.identity().as_str()),
                    Some(RoomEvent::LocalTrackRepublished { publication, .. })
                        if capture.track.as_ref().is_none_or(|track| track.sid() != publication.sid()) =>
                    {
                        // The SDK can be republishing while mute or deafen removes the old track.
                        let _ = room.local_participant().unpublish_track(&publication.sid()).await;
                    }
                    Some(RoomEvent::Reconnecting) => {
                        reconnecting = true;
                        capture.silence();
                        runtime.pc_factory().set_adm_playout_enabled(false);
                        speaking.0.clear();
                    }
                    Some(RoomEvent::Reconnected) if reconnecting => {
                        reconnecting = false;
                        resync = true;
                        check_routes = true;
                        intent = requested.borrow_and_update().clone();
                        devices = Arc::new(Devices::read(audio));
                        capture.reconcile_publication(room, audio).await;
                        subscribe_to_microphones(room);
                    }
                    Some(RoomEvent::Disconnected { reason }) => return Err(disconnect_issue(reason)),
                    None => return Err(Issue::LiveKitDisconnected),
                    _ => {}
                },
                result = capture.jobs.join_next(), if !reconnecting && !capture.jobs.is_empty() => {
                    capture.complete(room, audio, result.and_then(Result::ok).flatten()).await;
                    check_routes = true;
                }
                _ = scan.tick() => {
                    devices = Arc::new(Devices::read(audio));
                    check_routes = true;
                }
                _ = wait_until(speaking.deadline()) => {},
                _ = wait_until(presence.due(flags)) => {},
            }
        }
    }

    async fn ready(&self) -> Result<(), Issue> {
        self.send(VoiceFrame::Ready {
            sid: &self.join.sid,
        })
        .await
    }

    async fn state(&self, flags: VoiceFlags) -> Result<(), Issue> {
        self.send(VoiceFrame::State {
            sid: &self.join.sid,
            flags,
        })
        .await
    }

    async fn send(&self, frame: VoiceFrame<'_>) -> Result<(), Issue> {
        self.socket.send(frame).await.map_err(|_| Issue::Socket)
    }

    fn update(&self, update: Update) {
        let _ = self.updates.try_send(update);
    }
}

fn route_speakers(
    audio: &PlatformAudio,
    list: &DeviceList,
    intent: &Intent,
    previous: Option<&Device>,
) -> Option<Device> {
    let preferred = list.resolve(intent.preferred.speakers.as_ref());
    let previous = previous.and_then(|device| {
        list.choices
            .iter()
            .chain(list.default.iter())
            .find(|choice| *choice == device)
    });
    preferred
        .into_iter()
        .chain(previous.filter(|device| Some(*device) != preferred))
        .chain(
            list.default
                .as_ref()
                .filter(|device| Some(*device) != preferred && Some(*device) != previous),
        )
        .find(|device| devices::switch_speakers(audio, device))
        .cloned()
}

async fn wait_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        sleep_until(deadline).await;
    } else {
        pending().await
    }
}

struct Presence {
    flags: Option<VoiceFlags>,
    next: Instant,
}

impl Presence {
    fn new(deafened: bool) -> Self {
        Self {
            flags: Some(VoiceFlags {
                muted: true,
                deafened,
                speaking: false,
            }),
            next: Instant::now(),
        }
    }

    fn due(&self, flags: VoiceFlags) -> Option<Instant> {
        (Some(flags) != self.flags).then_some(self.next)
    }

    fn sent(&mut self, flags: VoiceFlags, now: Instant) {
        self.flags = Some(flags);
        self.next = now + Duration::from_millis(50);
    }

    fn resync(&mut self) {
        self.flags = None;
        self.next = Instant::now();
    }
}

#[derive(Default)]
struct Speaking(HashMap<String, Option<Instant>>);

impl Speaking {
    fn update(&mut self, active: HashSet<String>, now: Instant) {
        for (id, off) in &mut self.0 {
            if !active.contains(id) {
                off.get_or_insert(now + Duration::from_millis(500));
            }
        }
        self.0.extend(active.into_iter().map(|id| (id, None)));
    }

    fn remove(&mut self, id: &str) {
        self.0.remove(id);
    }

    fn deadline(&self) -> Option<Instant> {
        self.0.values().filter_map(|off| *off).min()
    }

    fn expire(&mut self, now: Instant) {
        self.0.retain(|_, off| off.is_none_or(|off| off > now));
    }
}

async fn connect(token: String) -> Result<(Room, RoomEvents), Issue> {
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.join_retries = 0;
    Room::connect(env!("LIVEKIT_URL"), &token, options)
        .await
        .map_err(|_| Issue::Media)
}

fn disconnect_issue(reason: DisconnectReason) -> Issue {
    match reason {
        DisconnectReason::UnknownReason
        | DisconnectReason::SignalClose
        | DisconnectReason::StateMismatch
        | DisconnectReason::ConnectionTimeout
        | DisconnectReason::ServerShutdown
        | DisconnectReason::MediaFailure => Issue::LiveKitDisconnected,
        _ => Issue::Disconnected,
    }
}

fn subscribe_to_microphones(room: &Room) {
    for participant in room.remote_participants().into_values() {
        for publication in participant.track_publications().into_values() {
            subscribe_to_microphone(&publication);
        }
    }
}

fn subscribe_to_microphone(publication: &RemoteTrackPublication) {
    if publication.kind() == TrackKind::Audio && publication.source() == TrackSource::Microphone {
        publication.set_subscribed(true);
    }
}

#[derive(Default)]
struct Capture {
    device: Option<Device>,
    track: Option<LocalAudioTrack>,
    published: Option<TrackSid>,
    starting: Option<oneshot::Sender<()>>,
    jobs: JoinSet<Option<TrackSid>>,
    failed: bool,
}

impl Capture {
    fn silence(&self) {
        if let Some(track) = &self.track {
            track.mute();
        }
    }

    fn current_publication(&self, room: &Room) -> Option<TrackSid> {
        // A full LiveKit reconnect can republish the same track with a new SID.
        let sid = self.track.as_ref()?.sid();
        room.local_participant()
            .track_publications()
            .contains_key(&sid)
            .then_some(sid)
    }

    async fn reconcile_publication(&mut self, room: &Room, audio: &PlatformAudio) {
        if self.published.is_some() {
            if let Some(sid) = self.current_publication(room) {
                self.published = Some(sid);
            } else {
                self.stop(room, audio).await;
            }
        }
    }

    fn start(&mut self, room: &Room, audio: &PlatformAudio, device: Device) {
        let (starting, cancelled) = oneshot::channel();
        let track = LocalAudioTrack::create_audio_track("microphone", audio.rtc_source());
        track.mute();
        self.track = Some(track.clone());
        self.device = Some(device.clone());
        self.starting = Some(starting);
        self.jobs.spawn(publish_microphone(
            room.local_participant(),
            audio.clone(),
            device,
            track,
            cancelled,
        ));
    }

    async fn stop(&mut self, room: &Room, audio: &PlatformAudio) {
        self.device = None;
        self.failed = false;
        self.starting.take();
        let published = self.published.take();
        let sid = self.current_publication(room).or(published);
        if let Some(track) = self.track.take() {
            track.mute();
            stop_recording(audio);
        }
        if let Some(sid) = sid {
            let _ = room.local_participant().unpublish_track(&sid).await;
        }
    }

    async fn complete(&mut self, room: &Room, audio: &PlatformAudio, published: Option<TrackSid>) {
        if self.starting.take().is_none() {
            if let Some(sid) = published {
                let _ = room.local_participant().unpublish_track(&sid).await;
            }
            return;
        }
        self.published = published.and_then(|_| self.current_publication(room));
        if self.published.is_some() {
            if let Some(track) = &self.track {
                track.unmute();
            }
        } else {
            self.track = None;
            self.failed = true;
            stop_recording(audio);
        }
    }
}

fn stop_recording(audio: &PlatformAudio) {
    let _ = audio.stop_recording();
    LkRuntime::instance()
        .pc_factory()
        .set_adm_recording_enabled(false);
}

async fn publish_microphone(
    participant: LocalParticipant,
    audio: PlatformAudio,
    device: Device,
    track: LocalAudioTrack,
    mut cancelled: oneshot::Receiver<()>,
) -> Option<TrackSid> {
    #[cfg(target_os = "macos")]
    tokio::select! {
        biased;
        _ = &mut cancelled => return None,
        granted = microphone_permission() => if !granted { return None; },
    }
    if !matches!(
        cancelled.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ) || !devices::select_microphone(&audio, &device)
    {
        return None;
    }
    LkRuntime::instance()
        .pc_factory()
        .set_adm_recording_enabled(true);
    #[cfg(not(windows))]
    audio.start_recording().ok()?;

    let publication = participant
        .publish_track(
            LocalTrack::Audio(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .ok()?;

    // WASAPI capture starts through LiveKit's track negotiation on Windows.
    #[cfg(windows)]
    if !tokio::select! {
        biased;
        _ = &mut cancelled => false,
        captured = windows_audio::wait_for_capture(&track) => captured,
    } {
        stop_recording(&audio);
        let _ = participant.unpublish_track(&publication.sid()).await;
        return None;
    }
    Some(publication.sid())
}

#[cfg(target_os = "macos")]
async fn microphone_permission() -> bool {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};

    let (send, receive) = oneshot::channel();
    {
        let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
            return false;
        };
        let send = std::sync::Mutex::new(Some(send));
        let completion = RcBlock::new(move |granted: Bool| {
            if let Some(send) = send
                .lock()
                .expect("microphone permission callback mutex")
                .take()
            {
                let _ = send.send(granted.as_bool());
            }
        });
        // AVFoundation copies the block and invokes it once with the user's permission.
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &completion) };
    }
    receive.await.unwrap_or(false)
}
