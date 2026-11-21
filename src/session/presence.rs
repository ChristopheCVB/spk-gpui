use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::task::JoinSet;

use super::Sidebar;
use crate::api::{Channel, Failure, Member, SpkClient, User, VideoPresence, VoiceFlags};
use crate::sounds::Sound;

#[derive(Clone)]
pub(super) struct VoiceMembership {
    pub(super) channel_id: String,
    flags: VoiceFlags,
    video: VideoPresence,
}

pub struct Participant<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub flags: VoiceFlags,
    pub video: Option<&'a VideoPresence>,
}

impl Sidebar {
    pub fn viewers(&self, channel: &str, presenter: &str) -> Vec<&str> {
        self.participants(channel)
            .into_iter()
            .filter(|user| {
                user.id != presenter
                    && user
                        .video
                        .is_some_and(|video| video.watching.iter().any(|id| id == presenter))
            })
            .map(|user| user.name)
            .collect()
    }

    pub fn participants(&self, channel: &str) -> Vec<Participant<'_>> {
        let mut people = self
            .voice_presence
            .iter()
            .filter(|(_, member)| member.channel_id == channel)
            .map(|(user, member)| Participant {
                id: user,
                name: self
                    .users
                    .get(user)
                    .filter(|user| !user.banned)
                    .and_then(User::name)
                    .unwrap_or("Unknown user"),
                flags: member.flags,
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
}

const SIDEBAR_RESOURCES: [Resource; 3] = [Resource::Channels, Resource::Users, Resource::Presence];

pub(super) enum Data {
    Channels(Vec<Channel>),
    Users(Vec<User>),
    Presence(Vec<Member>),
}

#[derive(Default)]
pub(super) struct Refresh {
    pub(super) tasks: JoinSet<(Resource, Result<Data, Failure>)>,
    in_flight: HashSet<Resource>,
    pub(super) dirty: HashSet<Resource>,
    failed: HashSet<Resource>,
    pub(super) presence_updates: Option<HashMap<String, Option<VoiceMembership>>>,
    pub(super) presence_ready: bool,
}

impl Refresh {
    pub(super) fn fetch(&mut self, client: &SpkClient, resource: Resource) {
        if !self.in_flight.insert(resource) {
            self.dirty.insert(resource);
            return;
        }
        if resource == Resource::Presence {
            self.presence_updates = Some(HashMap::new());
        }
        let client = client.clone();
        self.tasks.spawn(async move {
            let data = match resource {
                Resource::Channels => client.channels().await.map(Data::Channels),
                Resource::Users => client.users().await.map(Data::Users),
                Resource::Presence => client.voice_presence().await.map(Data::Presence),
            };
            (resource, data)
        });
    }

    pub(super) fn fetch_all(&mut self, client: &SpkClient) {
        for resource in SIDEBAR_RESOURCES {
            self.fetch(client, resource);
        }
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
                    if user_id != me
                        && voice.video.as_ref().is_some_and(|local| {
                            matches!(local.camera, crate::voice::video::CameraStatus::Live(..))
                        })
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
    ) {
        self.in_flight.remove(&resource);
        match data {
            Ok(data) => {
                self.failed.remove(&resource);
                match data {
                    Data::Channels(channels) => sidebar.channels = Some(channels),
                    Data::Users(users) => {
                        sidebar.users = Arc::new(
                            users
                                .into_iter()
                                .map(|user| (user.id.clone(), user))
                                .collect(),
                        );
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
                if resource == Resource::Presence {
                    self.presence_updates = None;
                }
            }
        }
        sidebar.load_failed = !self.failed.is_empty();
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
