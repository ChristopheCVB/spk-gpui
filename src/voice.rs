use std::{collections::HashSet, future::pending, sync::Arc, time::Duration};

use async_channel::{Receiver, Sender};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use url::Url;
use uuid::Uuid;

use crate::api::{
    Failure, SpkClient, VideoPresence, VoiceFlags, VoiceFrame, VoiceJoin, VoiceSocket,
};

pub(crate) mod audio_preview;
mod devices;
mod media;
pub(crate) mod radio;
pub(crate) mod screen;
pub(crate) mod soundboard;
pub(crate) mod video;

pub use devices::{
    Device, DeviceKind, DeviceList, DevicePreferences, Devices, NoiseSuppression,
    ParticipantVolumeChange, VoiceProcessing,
};
pub(crate) use devices::{MediaOutput, open_media_output};
pub use media::Diagnostics;
use media::Media;
#[cfg(test)]
pub(crate) use media::fixture;

#[cfg(test)]
pub(crate) mod tests;
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
    TokenRejected,
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
            Self::Token | Self::TokenRejected => {
                "SPK could not issue a voice token. Try joining again."
            }
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
    Video(Arc<video::VideoReport>),
    VideoParticipantLeft(String),
    Screen(screen::Report),
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
    pub radio_listening: Option<bool>,
    pub camera: video::CameraIntent,
    pub screen: screen::Request,
    pub watching: HashSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report {
    pub status: Status,
    pub devices: Arc<Devices>,
    pub microphone: Option<Device>,
    pub speakers: Option<Device>,
    pub speaking: HashSet<String>,
    pub diagnostics: Diagnostics,
    pub echo_reference_unavailable: bool,
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
    socket: watch::Sender<Option<VoiceSocket>>,
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
            tokio::spawn(worker.run(client, ack, cancelled, Media::connect_and_listen))
        })
    }

    #[cfg(test)]
    pub(crate) fn start_for_test(
        channel_id: String,
        client: SpkClient,
        socket: VoiceSocket,
        intent: Intent,
    ) -> Self {
        Self::spawn(channel_id, socket, intent, |worker, ack, cancelled| {
            tokio::spawn(worker.run(
                client,
                ack,
                cancelled,
                async |worker: &Worker, _: &mut Media, _| {
                    let mut requested = worker.requested.clone();
                    loop {
                        let intent = requested.borrow_and_update().clone();
                        let muted = intent.mute.effective_muted();
                        worker.publish_flags(
                            VoiceFlags {
                                muted,
                                deafened: intent.mute.deafened,
                                speaking: !muted,
                                radio_listening: None,
                            },
                            true,
                            false,
                        );
                        let mut watching: Vec<_> = intent.watching.iter().cloned().collect();
                        watching.sort_unstable();
                        worker.publish_video_presence(VideoPresence {
                            cam: intent.camera.enabled,
                            watching,
                            ..VideoPresence::default()
                        });
                        worker.update(Update::Media(Arc::new(Report {
                            status: Status::Connected,
                            devices: Arc::default(),
                            microphone: None,
                            speakers: None,
                            diagnostics: Diagnostics::default(),
                            echo_reference_unavailable: false,
                            speaking: if muted {
                                HashSet::new()
                            } else {
                                HashSet::from(["alice".to_owned()])
                            },
                        })));
                        requested.changed().await.map_err(|_| Issue::Disconnected)?;
                    }
                },
            ))
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
        let initial_socket = socket.clone();
        let (socket, connection) = watch::channel(Some(socket));
        let worker = start_worker(
            Worker {
                join: join.clone(),
                initial_socket,
                connection,
                presence: watch::Sender::new(None),
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
            socket,
            updates,
            worker,
        }
    }

    pub fn apply(&self, intent: &Intent) {
        if self.cancel.is_some() {
            self.intent.send_replace(intent.clone());
        }
    }

    pub fn set_socket(&self, socket: Option<VoiceSocket>) {
        if self.cancel.is_some() {
            self.socket.send_replace(socket);
        }
    }

    pub fn acknowledge(&mut self, sid: &str, channel_id: &str, nonce: &str) {
        if self.join.matches(sid, channel_id, nonce)
            && let Some(acknowledgment) = self.acknowledgment.take()
        {
            let _ = acknowledgment.send(());
        }
    }

    pub fn awaiting_acknowledgment(&self) -> bool {
        self.acknowledgment.is_some()
    }

    pub fn sid(&self) -> &str {
        &self.join.sid
    }

    pub fn owns(&self, sid: &str) -> bool {
        self.join.sid == sid
    }

    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            self.intent.send_modify(|intent| {
                intent.camera.enabled = false;
                intent.screen.action = screen::Action::Stop;
                intent.watching.clear();
            });
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
    initial_socket: VoiceSocket,
    connection: watch::Receiver<Option<VoiceSocket>>,
    presence: watch::Sender<Option<PublishedPresence>>,
    updates: Sender<Update>,
    requested: watch::Receiver<Intent>,
}

#[derive(Clone, PartialEq)]
struct PublishedPresence {
    flags: VoiceFlags,
    video: VideoPresence,
    ready: bool,
    revision: u64,
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
        let mut connection = self.connection.clone();
        let mut media = Media::default();

        let result = tokio::select! {
            biased;
            _ = &mut cancelled => Ok(()),
            result = async {
                let (token, deafened) = tokio::select! {
                    biased;
                    _ = connection.changed() => return Err(Issue::Socket),
                    result = self.claim(&client, ack, &mut claimed) => result?,
                };
                tokio::select! {
                    biased;
                    result = start_media(&self, &mut media, token) => result,
                    () = self.sync_presence(deafened) => Err(Issue::Socket),
                }
            } => result,
        };

        media.stop_audio().await;
        tokio::join!(
            async {
                let socket = self.connection.borrow().clone();
                if claimed && let Some(socket) = socket {
                    let _ = socket
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
            } else if matches!(error, Failure::Http(status)
                if status.is_client_error() || status == reqwest::StatusCode::SERVICE_UNAVAILABLE)
            {
                Issue::TokenRejected
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
        self.initial_socket
            .send(VoiceFrame::Join {
                sid,
                channel_id,
                mode: VoiceJoin::Initial { nonce },
                muted: true,
                deafened,
                watching: &[],
            })
            .await
            .map_err(|_| Issue::Socket)?;
        timeout(Duration::from_secs(5), ack)
            .await
            .map_err(|_| Issue::Acknowledgment)?
            .map_err(|_| Issue::Acknowledgment)?;
        Ok((token.token, deafened))
    }

    fn publish_flags(&self, flags: VoiceFlags, ready: bool, resync: bool) {
        self.presence.send_if_modified(|current| {
            let revision =
                current.as_ref().map_or(0, |current| current.revision) + u64::from(resync);
            let next = Some(PublishedPresence {
                flags,
                video: current
                    .as_ref()
                    .map(|state| state.video.clone())
                    .unwrap_or_default(),
                ready,
                revision,
            });
            if *current == next {
                return false;
            }
            *current = next;
            true
        });
    }

    fn publish_video_presence(&self, video: VideoPresence) {
        self.presence.send_if_modified(|current| {
            let Some(state) = current else {
                return false;
            };
            let video = VideoPresence {
                screen: state.video.screen,
                ..video
            };
            if state.video == video {
                return false;
            }
            state.video = video;
            true
        });
    }

    fn publish_screen_presence(&self, screen: bool) {
        self.presence.send_if_modified(|current| {
            let Some(state) = current else {
                return false;
            };
            if state.video.screen == screen {
                return false;
            }
            state.video.screen = screen;
            true
        });
    }

    async fn sync_presence(&self, joined_deafened: bool) {
        let mut connection = self.connection.clone();
        let mut published = self.presence.subscribe();
        loop {
            let socket = connection.borrow_and_update().clone();
            let changed = match socket {
                Some(socket) => {
                    let reclaim = !socket.same_connection(&self.initial_socket);
                    tokio::select! {
                        biased;
                        changed = connection.changed() => changed,
                        _ = self.sync_socket(&socket, &mut published, reclaim, joined_deafened) => {
                            connection.changed().await
                        }
                    }
                }
                None => connection.changed().await,
            };
            if changed.is_err() {
                return;
            }
        }
    }

    async fn sync_socket(
        &self,
        socket: &VoiceSocket,
        published: &mut watch::Receiver<Option<PublishedPresence>>,
        reclaim: bool,
        joined_deafened: bool,
    ) -> Result<(), Failure> {
        let sid = &self.join.sid;
        let first = published
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Failure::Connection)?
            .clone()
            .expect("presence is ready");
        let mut revision = first.revision;
        let mut presence = Presence::new(joined_deafened);
        if reclaim {
            socket
                .send(VoiceFrame::Join {
                    sid,
                    channel_id: &self.join.channel_id,
                    mode: VoiceJoin::Reclaim { ready: first.ready },
                    muted: first.flags.muted,
                    deafened: first.flags.deafened,
                    watching: &first.video.watching,
                })
                .await?;
            presence.resync();
        } else if first.ready {
            socket.send(VoiceFrame::Ready { sid }).await?;
            if revision != 0 {
                presence.resync();
            }
        }
        let mut requested = self.requested.clone();
        loop {
            let latest = published
                .borrow_and_update()
                .clone()
                .expect("presence is ready");
            if latest.ready && latest.revision != revision {
                socket.send(VoiceFrame::Ready { sid }).await?;
                revision = latest.revision;
                presence.resync();
            }
            let flags = VoiceFlags {
                radio_listening: requested.borrow_and_update().radio_listening,
                ..latest.flags
            };
            let video = latest.video;
            tokio::select! {
                biased;
                changed = published.changed() => {
                    changed.map_err(|_| Failure::Connection)?;
                }
                changed = requested.changed() => {
                    changed.map_err(|_| Failure::Connection)?;
                }
                _ = wait_until(presence.due(flags, &video)) => {
                    socket.send(VoiceFrame::State { sid, flags, video: &video }).await?;
                    presence.sent(flags, video, Instant::now());
                }
            }
        }
    }

    fn update(&self, update: Update) {
        let _ = self.updates.try_send(update);
    }
}

async fn wait_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        sleep_until(deadline).await;
    } else {
        pending().await
    }
}

struct Presence {
    state: Option<(VoiceFlags, VideoPresence)>,
    next: Instant,
}

impl Presence {
    fn new(deafened: bool) -> Self {
        Self {
            state: Some((
                VoiceFlags {
                    muted: true,
                    deafened,
                    speaking: false,
                    radio_listening: None,
                },
                VideoPresence::default(),
            )),
            next: Instant::now(),
        }
    }

    fn due(&self, flags: VoiceFlags, video: &VideoPresence) -> Option<Instant> {
        (!self
            .state
            .as_ref()
            .is_some_and(|(previous_flags, previous_video)| {
                *previous_flags == flags && previous_video == video
            }))
        .then_some(self.next)
    }

    fn sent(&mut self, flags: VoiceFlags, video: VideoPresence, now: Instant) {
        self.state = Some((flags, video));
        self.next = now + Duration::from_millis(50);
    }

    fn resync(&mut self) {
        self.state = None;
        self.next = Instant::now();
    }
}
