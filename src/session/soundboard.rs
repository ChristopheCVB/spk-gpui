use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{
    sync::watch,
    task::{AbortHandle, Id, JoinSet},
    time::Instant,
};
use uuid::Uuid;

use super::{ActionResult, Command, Sidebar, VoiceSession};
use crate::{
    api::{Failure, SoundboardClip, SpkClient, VoiceFrame},
    voice::{MediaOutput, soundboard},
};

const MAX_CLIP: Duration = Duration::from_secs(soundboard::MAX_SECONDS as u64);
const PLAYER_REST: Duration = Duration::from_secs(1);
const UNKNOWN_CLIP: Duration = Duration::from_secs(1);

#[derive(Clone, Default)]
pub struct SoundboardView {
    pub has_new_clip: bool,
    pub clips: Option<Arc<[SoundboardClip]>>,
    pub loading: bool,
    pub playback_blocked: bool,
    pub active_clips: HashMap<String, Option<SoundboardClip>>,
    pub directory_error: Option<&'static str>,
}

pub(super) enum SoundboardResult {
    Directory(Result<Vec<SoundboardClip>, Failure>),
    Playback(Result<(), Failure>),
    Sent(Result<(), Failure>),
}

struct AcceptedClip {
    sound_id: String,
    started_at: Instant,
    ends_at: Instant,
    clip: Option<SoundboardClip>,
}

impl AcceptedClip {
    fn resolve(&mut self, clips: &[SoundboardClip], still_loading: bool) {
        self.clip = clips.iter().find(|clip| clip.id == self.sound_id).cloned();
        let duration = match &self.clip {
            Some(clip) => Duration::from_millis(u64::from(clip.duration_ms)).min(MAX_CLIP),
            None if still_loading => MAX_CLIP,
            // shortcut: estimate when catalog lookup misses, until the relay provides a duration.
            None => UNKNOWN_CLIP,
        };
        self.ends_at = self.started_at + duration;
    }
}

#[derive(Default)]
pub(super) struct SoundboardSession {
    sid: Option<String>,
    output: Option<watch::Sender<MediaOutput>>,
    playback: HashMap<Id, (bool, AbortHandle)>,
    pub tasks: JoinSet<SoundboardResult>,
    reload: bool,
    accepted: HashMap<String, AcceptedClip>,
    last_player: Option<String>,
    pub next_transition: Option<Instant>,
}

impl SoundboardSession {
    pub fn sync(&mut self, sidebar: &mut Sidebar, voice: &VoiceSession) {
        let sid = voice.active_call_sid(sidebar);
        if self.sid.as_deref() != sid {
            *self = Self {
                sid: sid.map(str::to_owned),
                ..Self::default()
            };
            sidebar.soundboard = SoundboardView {
                has_new_clip: sidebar.soundboard.has_new_clip,
                ..SoundboardView::default()
            };
        }
        if self.sid.is_none() {
            return;
        }

        self.update_turn(&mut sidebar.soundboard, sidebar.profile.id.as_deref());

        let preferred = &sidebar.voice_intent.preferred;
        if sidebar.voice_intent.mute.deafened || !preferred.soundboard_enabled {
            self.playback.retain(|_, (preview, task)| {
                if !*preview {
                    task.abort();
                }
                *preview
            });
        }
        let output = soundboard_output(sidebar);
        if let Some(settings) = &self.output {
            settings.send_if_modified(|current| {
                if *current == output {
                    return false;
                }
                *current = output;
                true
            });
        } else {
            self.output = Some(watch::channel(output).0);
        }
    }

    fn update_turn(&mut self, board: &mut SoundboardView, me: Option<&str>) {
        let now = Instant::now();
        let my_rest = self
            .last_player
            .as_deref()
            .filter(|&player| me == Some(player));
        board.active_clips.clear();
        self.next_transition = None;
        self.accepted.retain(|user, sound| {
            let playing = now < sound.ends_at;
            let deadline = if playing {
                board.active_clips.insert(user.clone(), sound.clip.clone());
                sound.ends_at
            } else if Some(user.as_str()) == my_rest {
                sound.ends_at + PLAYER_REST
            } else {
                return false;
            };
            if now >= deadline {
                return false;
            }
            self.next_transition =
                Some(self.next_transition.map_or(deadline, |at| at.min(deadline)));
            true
        });
        board.playback_blocked = !self.accepted.is_empty();
    }

    pub fn relay(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        user_id: String,
        sound_id: String,
    ) {
        let Some(voice) = sidebar
            .voice
            .as_ref()
            .filter(|voice| voice.is_participating())
        else {
            return;
        };
        // The relay has no channel or session ID. Ignore senders whose Voice Presence puts them in another room.
        let from_voice_channel = sidebar.profile.id.as_deref() == Some(user_id.as_str())
            || sidebar
                .voice_presence
                .get(&user_id)
                .is_none_or(|member| member.channel_id == voice.channel_id);
        let Ok(id) = Uuid::parse_str(&sound_id) else {
            return;
        };
        if !from_voice_channel {
            return;
        }

        let board = &mut sidebar.soundboard;
        let now = Instant::now();
        let mut accepted = AcceptedClip {
            sound_id,
            started_at: now,
            ends_at: now,
            clip: None,
        };
        accepted.resolve(board.clips.as_deref().unwrap_or_default(), true);
        if accepted.clip.is_none() {
            self.refresh(board, client);
        }
        self.last_player = Some(user_id.clone());
        self.accepted.insert(user_id, accepted);

        if !sidebar.voice_intent.mute.deafened && sidebar.voice_intent.preferred.soundboard_enabled
        {
            self.play(client, id, false);
        }
    }

    fn play(&mut self, client: &SpkClient, id: Uuid, preview: bool) {
        let Some(output) = &self.output else { return };
        if self.playback.len() >= 8 {
            return;
        }
        let settings = output.subscribe();
        let client = client.clone();
        let task = self.tasks.spawn(async move {
            let result = async {
                let bytes = client.soundboard_file(id).await?;
                soundboard::play(bytes, settings, None)
                    .await
                    .map_err(|_| Failure::InvalidResponse)
            }
            .await;
            SoundboardResult::Playback(result)
        });
        self.playback.insert(task.id(), (preview, task));
    }

    pub fn command(
        &mut self,
        sidebar: &mut Sidebar,
        voice: &VoiceSession,
        client: &SpkClient,
        command: Command,
    ) {
        if self.sid.is_none() {
            return;
        }
        let board = &mut sidebar.soundboard;
        match command {
            Command::OpenSoundboard | Command::LoadSoundboard => {
                if matches!(command, Command::OpenSoundboard) {
                    board.has_new_clip = false;
                }
                self.refresh(board, client);
            }
            command @ (Command::PlaySound(_) | Command::PreviewSound(_)) => {
                let (id, preview) = match command {
                    Command::PlaySound(id) => (id, false),
                    Command::PreviewSound(id) => (id, true),
                    _ => unreachable!(),
                };
                let Ok(sound_id) = Uuid::parse_str(&id) else {
                    return;
                };
                if !board
                    .clips
                    .iter()
                    .flat_map(|clips| clips.iter())
                    .any(|clip| clip.id == id)
                {
                    return;
                }
                if preview {
                    self.play(client, sound_id, true);
                    return;
                }
                if board.playback_blocked || !sidebar.voice_intent.preferred.soundboard_enabled {
                    return;
                }
                let Some((socket, sid)) = voice.active_socket(sidebar) else {
                    sidebar.action_results.push(ActionResult::error(
                        "Soundboard is offline. Wait for reconnection.",
                    ));
                    return;
                };
                self.tasks.spawn(async move {
                    SoundboardResult::Sent(
                        socket
                            .send(VoiceFrame::Sound {
                                sid: &sid,
                                sound_id: &id,
                            })
                            .await,
                    )
                });
            }
            _ => {}
        }
    }

    pub fn refresh(&mut self, board: &mut SoundboardView, client: &SpkClient) {
        if board.loading {
            self.reload = true;
            return;
        }
        board.loading = true;
        board.directory_error = None;
        let client = client.clone();
        self.tasks
            .spawn(async move { SoundboardResult::Directory(client.soundboard_clips().await) });
    }

    pub fn complete(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        id: Id,
        result: SoundboardResult,
    ) -> Result<(), Failure> {
        match result {
            SoundboardResult::Directory(result) => {
                let board = &mut sidebar.soundboard;
                board.loading = false;
                if std::mem::take(&mut self.reload)
                    && !result.as_ref().is_err_and(Failure::unauthorized)
                {
                    self.refresh(board, client);
                }
                for sound in self
                    .accepted
                    .values_mut()
                    .filter(|sound| sound.clip.is_none())
                {
                    sound.resolve(result.as_deref().unwrap_or_default(), board.loading);
                }
                match result {
                    Ok(clips) => board.clips = Some(clips.into()),
                    Err(error) => {
                        board.clips = None;
                        board.directory_error = Some("Soundboard is unavailable.");
                        return Err(error);
                    }
                }
            }
            SoundboardResult::Playback(result) => {
                if self.playback.remove(&id).is_some()
                    && let Err(error) = result
                {
                    sidebar
                        .action_results
                        .push(ActionResult::error("Could not play this Soundboard clip."));
                    return Err(error);
                }
            }
            SoundboardResult::Sent(Err(_)) => {
                sidebar.action_results.push(ActionResult::error(
                    "Could not send this Soundboard clip. Try again.",
                ));
            }
            SoundboardResult::Sent(Ok(())) => {}
        }
        Ok(())
    }
}

pub(super) fn soundboard_output(sidebar: &Sidebar) -> MediaOutput {
    let preferred = &sidebar.voice_intent.preferred;
    MediaOutput {
        volume: preferred.output_gain() * f32::from(preferred.soundboard_volume.min(100)) / 100.0,
        ..sidebar.media_output()
    }
}
