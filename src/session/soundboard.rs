use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{
    sync::watch,
    task::{AbortHandle, Id, JoinSet},
    time,
};
use uuid::Uuid;

use super::{Command, Sidebar, VoiceSession};
use crate::{
    api::{Failure, SoundboardClip, SpkClient, VoiceFrame},
    voice::{MediaOutput, soundboard},
};

#[derive(Clone, Default)]
pub struct SoundboardView {
    pub clips: Option<Arc<[SoundboardClip]>>,
    pub loading: bool,
    pub cooling: bool,
    pub directory_error: Option<&'static str>,
    pub playback_error: Option<&'static str>,
}

pub(super) enum SoundboardResult {
    Directory(Result<Vec<SoundboardClip>, Failure>),
    Playback(Result<(), Failure>),
    Sent(Result<(), Failure>),
    Cooled,
}

#[derive(Default)]
pub(super) struct SoundboardSession {
    sid: Option<String>,
    output: Option<watch::Sender<MediaOutput>>,
    playback: HashMap<Id, (bool, AbortHandle)>,
    pub tasks: JoinSet<SoundboardResult>,
}

impl SoundboardSession {
    pub fn sync(&mut self, sidebar: &mut Sidebar, voice: &VoiceSession) {
        let sid = voice.active_call_sid(sidebar);
        if self.sid.as_deref() != sid {
            *self = Self {
                sid: sid.map(str::to_owned),
                ..Self::default()
            };
            sidebar.soundboard = SoundboardView::default();
        }
        if self.sid.is_none() {
            return;
        }

        let preferred = &sidebar.voice_intent.preferred;
        if sidebar.voice_intent.mute.deafened || !preferred.soundboard_enabled {
            self.playback.retain(|_, (preview, task)| {
                if !*preview {
                    task.abort();
                }
                *preview
            });
        }
        let output = MediaOutput {
            device: sidebar
                .devices
                .media_outputs
                .resolve(preferred.media_output.as_ref())
                .cloned(),
            volume: preferred.output_gain() * f32::from(preferred.soundboard_volume.min(100))
                / 100.0,
        };
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

    pub fn relay(&mut self, sidebar: &Sidebar, client: &SpkClient, user_id: &str, sound_id: &str) {
        let Some(voice) = sidebar
            .voice
            .as_ref()
            .filter(|voice| voice.is_participating())
        else {
            return;
        };
        // The relay has no channel or session ID. Ignore senders whose Voice Presence puts them in another room.
        let from_voice_channel = sidebar.profile.id.as_deref() == Some(user_id)
            || sidebar
                .voice_presence
                .get(user_id)
                .is_none_or(|member| member.channel_id == voice.channel_id);
        if from_voice_channel
            && !sidebar.voice_intent.mute.deafened
            && sidebar.voice_intent.preferred.soundboard_enabled
            && let Ok(id) = Uuid::parse_str(sound_id)
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
                soundboard::play(bytes, settings)
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
            Command::LoadSoundboard if !board.loading => {
                board.loading = true;
                board.directory_error = None;
                let client = client.clone();
                self.tasks.spawn(async move {
                    SoundboardResult::Directory(client.soundboard_clips().await)
                });
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
                board.playback_error = None;
                if preview {
                    self.play(client, sound_id, true);
                    return;
                }
                if board.cooling || !sidebar.voice_intent.preferred.soundboard_enabled {
                    return;
                }
                let Some((socket, sid)) = voice.active_socket(sidebar) else {
                    sidebar.soundboard.playback_error =
                        Some("Soundboard is offline. Wait for reconnection.");
                    return;
                };
                sidebar.soundboard.cooling = true;
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
                self.tasks.spawn(async {
                    time::sleep(Duration::from_secs(3)).await;
                    SoundboardResult::Cooled
                });
            }
            _ => {}
        }
    }

    pub fn complete(
        &mut self,
        board: &mut SoundboardView,
        id: Id,
        result: SoundboardResult,
    ) -> Result<(), Failure> {
        match result {
            SoundboardResult::Directory(result) => {
                board.loading = false;
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
                    board.playback_error = Some("Could not play this Soundboard clip.");
                    return Err(error);
                }
            }
            SoundboardResult::Sent(Err(_)) => {
                board.playback_error = Some("Could not send this Soundboard clip. Try again.");
            }
            SoundboardResult::Sent(Ok(())) => {}
            SoundboardResult::Cooled => board.cooling = false,
        }
        Ok(())
    }
}
