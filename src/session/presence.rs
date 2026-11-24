use std::{
    collections::{HashMap, HashSet},
    fs,
    future::pending,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use tokio::{
    sync::watch,
    task::{JoinSet, spawn_blocking},
};

use super::{Sidebar, VoiceView};
use crate::api::{
    Channel, Failure, Member, PresenceReport, SpkClient, User, UserPresence, UserStatus,
    VideoPresence, VoiceFlags, VoiceSocket,
};
use crate::sounds::Sound;
use crate::voice::video::{CameraStatus, VideoSource};

pub(super) struct PresenceSettings {
    pub(super) path: Option<PathBuf>,
    pub(super) status: UserStatus,
    pub(super) activity: watch::Receiver<bool>,
}

impl PresenceSettings {
    pub(super) fn load(activity: watch::Receiver<bool>) -> Self {
        let path = dirs::config_dir().map(|dir| dir.join("spk-gpui").join("user-status.json"));
        let status = path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            status,
            activity,
        }
    }

    pub(super) async fn save(&mut self, status: UserStatus) -> Result<(), &'static str> {
        self.status = status;
        let path = self.path.clone();
        let saved = spawn_blocking(move || -> io::Result<()> {
            let path = path.ok_or_else(|| io::Error::other("Missing config directory"))?;
            let directory = path.parent().expect("status file has a parent");
            fs::create_dir_all(directory)?;
            let mut file = tempfile::NamedTempFile::new_in(directory)?;
            serde_json::to_writer(&mut file, &status)?;
            file.flush()?;
            file.persist(path)?;
            Ok(())
        })
        .await;
        match saved {
            Ok(Ok(())) => Ok(()),
            _ => Err("Could not save your status for the next launch."),
        }
    }
}

#[derive(Default)]
pub(super) struct PresenceReporter {
    socket: Option<VoiceSocket>,
    status: Option<UserStatus>,
    active: Option<bool>,
}

impl PresenceReporter {
    pub(super) fn connected(&mut self, socket: VoiceSocket) {
        *self = Self {
            socket: Some(socket),
            ..Self::default()
        };
    }

    pub(super) async fn report(&mut self, status: UserStatus, active: bool) {
        let Some(socket) = &self.socket else {
            return pending().await;
        };
        if self.status != Some(status) {
            if socket
                .send_presence(PresenceReport::Status(status))
                .await
                .is_ok()
            {
                self.status = Some(status);
            } else {
                self.socket = None;
            }
        } else if self.active != Some(active) {
            if socket
                .send_presence(PresenceReport::Activity(active))
                .await
                .is_ok()
            {
                self.active = Some(active);
            } else {
                self.socket = None;
            }
        } else {
            pending().await
        }
    }
}

#[derive(Clone)]
pub(super) struct VoiceMembership {
    pub(super) channel_id: String,
    pub(super) flags: VoiceFlags,
    video: VideoPresence,
}

impl VoiceMembership {
    pub(super) fn video_feeds(&self, user: &str, call: Option<&VoiceView>) -> VideoFeeds {
        if let Some(report) = call
            .filter(|call| call.channel_id == self.channel_id)
            .and_then(|call| call.video.as_deref())
            .filter(|report| !report.reconnecting)
        {
            // SPK presence may lag behind an actual LiveKit publication or unpublish.
            return report
                .participants
                .get(user)
                .map_or_else(VideoFeeds::default, |video| VideoFeeds {
                    camera: video.live.contains(&VideoSource::Camera),
                    screen: video.live.contains(&VideoSource::Screen),
                });
        }
        VideoFeeds {
            camera: self.video.cam,
            screen: self.video.screen,
        }
    }
}

#[derive(Default)]
pub(crate) struct VideoFeeds {
    pub camera: bool,
    pub screen: bool,
}

impl VideoFeeds {
    pub(crate) fn is_live(&self) -> bool {
        self.camera || self.screen
    }
}

pub struct Participant<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub flags: VoiceFlags,
    pub microphone_connecting: bool,
    pub video: Option<&'a VideoPresence>,
}

impl Sidebar {
    pub(crate) fn video_feeds(&self, channel: &str, user: &str) -> VideoFeeds {
        if self.profile.id.as_deref() == Some(user)
            && let Some(call) = self
                .voice
                .as_ref()
                .filter(|call| call.channel_id == channel)
        {
            return VideoFeeds {
                camera: matches!(self.camera_status(), CameraStatus::Live(..)),
                screen: call
                    .screen
                    .as_ref()
                    .is_some_and(|screen| screen.state.active()),
            };
        }
        self.voice_presence
            .get(user)
            .filter(|member| member.channel_id == channel)
            .map_or_else(VideoFeeds::default, |member| {
                member.video_feeds(user, self.voice.as_ref())
            })
    }

    pub(crate) fn advertises_video(&self, channel: &str, user: &str) -> bool {
        self.voice_presence.get(user).is_some_and(|member| {
            member.channel_id == channel && (member.video.cam || member.video.screen)
        })
    }

    pub(crate) fn can_watch_video(&self, channel: &str, user: &str) -> bool {
        self.profile.id.as_deref() != Some(user)
            && self.voice.as_ref().is_some_and(|call| {
                call.channel_id == channel
                    && call.is_participating()
                    && call
                        .video
                        .as_ref()
                        .is_none_or(|report| !report.reconnecting)
            })
            && self.video_feeds(channel, user).is_live()
    }

    pub fn viewers(&self, channel: &str, presenter: &str) -> Vec<Participant<'_>> {
        self.participants(channel)
            .into_iter()
            .filter(|user| {
                user.id != presenter
                    && user
                        .video
                        .is_some_and(|video| video.watching.iter().any(|id| id == presenter))
            })
            .collect()
    }

    pub fn participants(&self, channel: &str) -> Vec<Participant<'_>> {
        let media = self
            .voice
            .as_ref()
            .filter(|voice| voice.channel_id == channel)
            .and_then(|voice| voice.media.as_deref());
        let mut people = self
            .voice_presence
            .iter()
            .filter(|(_, member)| member.channel_id == channel)
            .map(|(user, member)| Participant {
                id: user,
                name: if self.profile.id.as_ref() == Some(user) {
                    self.profile
                        .display_name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                } else {
                    self.person(user).and_then(User::name)
                }
                .unwrap_or("Unknown user"),
                flags: member.flags,
                microphone_connecting: self.voice_intent.expects_remote_microphone(user)
                    && !member.flags.muted
                    && !member.flags.deafened
                    && media.is_some_and(|report| report.connecting_microphones.contains(user)),
                video: Some(&member.video),
            })
            .collect::<Vec<_>>();
        people.sort_unstable_by(|left, right| (left.name, left.id).cmp(&(right.name, right.id)));
        people
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum Resource {
    Channels,
    Users,
    Presence,
    UserPresence,
}

const SIDEBAR_RESOURCES: [Resource; 3] = [Resource::Channels, Resource::Users, Resource::Presence];

pub(super) enum Data {
    Channels(Vec<Channel>),
    Users(Vec<User>),
    Presence(Vec<Member>),
    UserPresence(HashMap<String, UserPresence>),
}

#[derive(Default)]
pub(super) struct Refresh {
    pub(super) tasks: JoinSet<(Resource, Result<Data, Failure>)>,
    in_flight: HashSet<Resource>,
    pub(super) dirty: HashSet<Resource>,
    failed: HashSet<Resource>,
    discard_channel_snapshot: bool,
    pub(super) presence_updates: Option<HashMap<String, Option<VoiceMembership>>>,
    pub(super) presence_ready: bool,
    user_presence_updates: Option<HashMap<String, UserPresence>>,
}

impl Refresh {
    pub(super) fn fetch(&mut self, client: &SpkClient, resource: Resource) {
        if !self.in_flight.insert(resource) {
            self.dirty.insert(resource);
            return;
        }
        match resource {
            Resource::Presence => self.presence_updates = Some(HashMap::new()),
            Resource::UserPresence => self.user_presence_updates = Some(HashMap::new()),
            _ => {}
        }
        let client = client.clone();
        self.tasks.spawn(async move {
            let data = match resource {
                Resource::Channels => client.channels().await.map(Data::Channels),
                Resource::Users => client.users().await.map(Data::Users),
                Resource::Presence => client.voice_presence().await.map(Data::Presence),
                Resource::UserPresence => client.user_presence().await.map(Data::UserPresence),
            };
            (resource, data)
        });
    }

    pub(super) fn webhook_changed(&mut self, client: &SpkClient) {
        self.discard_channel_snapshot = self.channels_loading();
        self.fetch(client, Resource::Channels);
    }

    pub(super) fn channels_loading(&self) -> bool {
        self.in_flight.contains(&Resource::Channels)
    }

    pub(super) fn fetch_all(&mut self, client: &SpkClient) {
        for resource in SIDEBAR_RESOURCES {
            self.fetch(client, resource);
        }
    }

    pub(super) fn update_user_presence(
        &mut self,
        sidebar: &mut Sidebar,
        user_id: String,
        status: UserPresence,
    ) {
        if let Some(updates) = &mut self.user_presence_updates {
            updates.insert(user_id.clone(), status);
        }
        apply_user_presence(Arc::make_mut(&mut sidebar.user_presence), user_id, status);
    }

    pub(super) fn update_presence(
        &mut self,
        sidebar: &mut Sidebar,
        user_id: String,
        channel_id: Option<String>,
        flags: VoiceFlags,
        video: VideoPresence,
    ) -> Vec<(Sound, bool)> {
        let mut cues = Vec::new();
        if let Some(voice) = sidebar
            .voice
            .as_ref()
            .filter(|voice| self.presence_ready && voice.is_participating())
            && let Some(me) = sidebar.profile.id.as_deref()
        {
            let previous = sidebar
                .voice_presence
                .get(&user_id)
                .filter(|member| member.channel_id == voice.channel_id);
            let here = channel_id.as_deref() == Some(voice.channel_id.as_str());
            match (previous, here) {
                (None, true) if user_id != me => cues.push((Sound::Join, true)),
                (Some(_), false) if user_id != me => cues.push((Sound::Leave, false)),
                (Some(previous), true) => {
                    if previous.video.cam != video.cam {
                        cues.push((Sound::Camera, video.cam));
                    }
                    if user_id != me && previous.video.screen != video.screen {
                        cues.push((Sound::Screen, video.screen));
                    }
                    if user_id != me
                        && voice
                            .screen
                            .as_ref()
                            .is_some_and(|screen| screen.state.active())
                    {
                        let was = previous.video.watching.iter().any(|id| id == me);
                        let now = video.watching.iter().any(|id| id == me);
                        if was != now {
                            cues.push((Sound::Watch, now));
                        }
                    }
                }
                _ => {}
            }
        }
        let member = channel_id.map(|channel_id| VoiceMembership {
            channel_id,
            flags,
            video,
        });
        if let Some(updates) = &mut self.presence_updates {
            updates.insert(user_id.clone(), member.clone());
        }
        apply_presence(&mut sidebar.voice_presence, user_id, member);
        cues
    }

    pub(super) fn complete(
        &mut self,
        sidebar: &mut Sidebar,
        resource: Resource,
        data: Result<Data, Failure>,
    ) -> bool {
        let mut webhook_membership_changed = false;
        self.in_flight.remove(&resource);
        let discard =
            resource == Resource::Channels && std::mem::take(&mut self.discard_channel_snapshot);
        match data {
            Ok(data) => {
                self.failed.remove(&resource);
                match data {
                    // A snapshot started before a mutation may contain obsolete access.
                    Data::Channels(_) if discard => {}
                    Data::Channels(mut channels) => {
                        channels.retain(|channel| {
                            !channel.is_webhook() || sidebar.can_view_webhook(channel)
                        });
                        let previous: HashSet<_> = sidebar
                            .webhook_feeds()
                            .map(|feed| feed.id.as_str())
                            .collect();
                        let current: HashSet<_> = channels
                            .iter()
                            .filter(|channel| channel.is_webhook())
                            .map(|feed| feed.id.as_str())
                            .collect();
                        webhook_membership_changed = previous != current;
                        sidebar.channels = Some(channels);
                    }
                    Data::Users(users) => {
                        // A snapshot queued before a completed edit can contain the old Profile.
                        if !self.dirty.contains(&resource)
                            && let Some(me) = users
                                .iter()
                                .find(|user| sidebar.profile.id.as_ref() == Some(&user.id))
                        {
                            if matches!(me.role.as_str(), "admin" | "user") {
                                sidebar.profile.role.clone_from(&me.role);
                            }
                            if !sidebar.profile_edit.busy() {
                                sidebar.profile.display_name.clone_from(&me.display_name);
                                sidebar.profile.avatar_key.clone_from(&me.avatar_key);
                            }
                        }
                        sidebar.user_order = users.iter().map(|user| user.id.clone()).collect();
                        sidebar.users = Arc::new(
                            users
                                .into_iter()
                                .map(|user| (user.id.clone(), user))
                                .collect(),
                        );
                    }
                    Data::UserPresence(mut snapshot) => {
                        for (id, status) in self.user_presence_updates.take().into_iter().flatten()
                        {
                            apply_user_presence(&mut snapshot, id, status);
                        }
                        sidebar.user_presence = Arc::new(snapshot);
                    }
                    Data::Presence(members) => {
                        self.presence_ready = true;
                        sidebar.voice_presence = members
                            .into_iter()
                            .map(|member| {
                                (
                                    member.user_id,
                                    VoiceMembership {
                                        channel_id: member.channel_id,
                                        flags: member.flags,
                                        video: member.video,
                                    },
                                )
                            })
                            .collect();
                        for (user, member) in self.presence_updates.take().into_iter().flatten() {
                            apply_presence(&mut sidebar.voice_presence, user, member);
                        }
                    }
                }
            }
            Err(_) => {
                self.failed.insert(resource);
                match resource {
                    Resource::Presence => self.presence_updates = None,
                    Resource::UserPresence => self.user_presence_updates = None,
                    _ => {}
                }
            }
        }
        sidebar.load_failed = !self.failed.is_empty();
        webhook_membership_changed
    }
}

fn apply_presence(
    voice_presence: &mut HashMap<String, VoiceMembership>,
    user_id: String,
    member: Option<VoiceMembership>,
) {
    if let Some(member) = member {
        voice_presence.insert(user_id, member);
    } else {
        voice_presence.remove(&user_id);
    }
}

fn apply_user_presence(
    statuses: &mut HashMap<String, UserPresence>,
    user_id: String,
    status: UserPresence,
) {
    if status == UserPresence::Offline {
        statuses.remove(&user_id);
    } else {
        statuses.insert(user_id, status);
    }
}
