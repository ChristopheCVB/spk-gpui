use std::{future::pending, path::PathBuf, sync::Arc, time::Duration};

use reqwest::StatusCode;
use tokio::{
    sync::watch,
    task::{JoinSet, spawn_blocking},
};
use uuid::Uuid;

use super::{ActionResult, Sidebar, soundboard::soundboard_output};
use crate::{
    api::{Failure, SoundboardClip, SpkClient},
    voice::{
        MediaOutput,
        soundboard::{self, PreparedSound, SoundSource, SoundSourceError},
    },
};

pub use crate::voice::soundboard::{
    Crop as SoundboardCrop, Trim as SoundboardTrim, Waveform as SoundboardWaveform,
};

const ACCESS_REQUIRED: &str = "Admin access is required.";
const CHOOSE_AUDIO: &str = "Choose an audio file first.";

pub enum SoundboardAdminAction {
    Show,
    Close,
    ChooseFile(PathBuf),
    Discard,
    Trim(SoundboardTrim, f64),
    NudgeTrim(SoundboardTrim, f64),
    Gain(u16),
    Preview,
    PreviewSaved(String),
    StopPreview,
    Add {
        name: String,
        icon: String,
    },
    Edit {
        id: String,
        name: String,
        icon: String,
    },
    Delete(String),
}

#[derive(Clone, Copy, Default)]
pub enum SoundboardPreview {
    #[default]
    Idle,
    Saved,
    Upload {
        position: f64,
    },
}

impl SoundboardPreview {
    pub fn is_playing(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

#[derive(Clone, Default)]
pub struct SoundboardAdminView {
    pub clips: Option<Arc<[SoundboardClip]>>,
    pub loading: bool,
    pub waveform: Option<SoundboardWaveform>,
    pub selection: Option<SoundboardCrop>,
    pub decoding: bool,
    pub preview: SoundboardPreview,
    pub saving: bool,
    pub mutation_revision: u64,
    pub directory_error: Option<&'static str>,
    pub validation_error: Option<&'static str>,
}

pub(super) enum Completion {
    Loaded(Result<Vec<SoundboardClip>, Failure>),
    Selected(Result<SoundSource, WorkError>),
    Previewed(Result<(), WorkError>),
}

pub(super) enum WorkError {
    Validation(&'static str),
    Action(&'static str),
    Request(Failure),
}

impl From<Failure> for WorkError {
    fn from(value: Failure) -> Self {
        Self::Request(value)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Mutation {
    Add,
    Edit,
    Delete,
}

#[derive(Default)]
pub(super) struct AdminSoundboard {
    open: bool,
    source: Option<Arc<SoundSource>>,
    preview_output: Option<watch::Sender<MediaOutput>>,
    pub preview_progress: Option<watch::Receiver<Duration>>,
    pub tasks: JoinSet<Completion>,
    pub mutations: JoinSet<(Mutation, Result<(), WorkError>)>,
}

impl AdminSoundboard {
    pub fn sync(&mut self, sidebar: &mut Sidebar) {
        if self.open && !sidebar.is_admin() {
            self.close(&mut sidebar.admin_soundboard);
        }
        if let Some(output) = &self.preview_output {
            let next = soundboard_output(sidebar);
            output.send_if_modified(|current| {
                if *current == next {
                    return false;
                }
                *current = next;
                true
            });
        }
        let view = &mut sidebar.admin_soundboard;
        if let (SoundboardPreview::Upload { position }, Some(progress), Some(crop)) = (
            &mut view.preview,
            &mut self.preview_progress,
            view.selection,
        ) {
            *position = (crop.start + progress.borrow_and_update().as_secs_f64()).min(crop.end);
        }
    }

    pub async fn preview_changed(progress: &mut Option<watch::Receiver<Duration>>) {
        if let Some(progress) = progress
            && progress.changed().await.is_ok()
        {
            return;
        }
        pending().await
    }

    fn finish_preview(&mut self, view: &mut SoundboardAdminView) {
        self.tasks = JoinSet::new();
        self.preview_output = None;
        self.preview_progress = None;
        view.preview = SoundboardPreview::Idle;
    }

    fn close(&mut self, view: &mut SoundboardAdminView) {
        self.open = false;
        self.finish_preview(view);
        self.source = None;
        *view = SoundboardAdminView {
            saving: !self.mutations.is_empty(),
            mutation_revision: view.mutation_revision,
            ..Default::default()
        };
    }

    fn clear_source(&mut self, view: &mut SoundboardAdminView) {
        self.source = None;
        view.waveform = None;
        view.selection = None;
    }

    fn load(&mut self, view: &mut SoundboardAdminView, client: &SpkClient) {
        if !view.loading {
            view.loading = true;
            view.directory_error = None;
            let client = client.clone();
            self.tasks
                .spawn(async move { Completion::Loaded(client.soundboard_clips().await) });
        }
    }

    pub fn command(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        action: SoundboardAdminAction,
    ) {
        use SoundboardAdminAction::*;
        let admin = sidebar.is_admin();
        let output = soundboard_output(sidebar);
        let view = &mut sidebar.admin_soundboard;
        if matches!(action, Close) {
            self.close(view);
            return;
        }
        if !admin {
            sidebar
                .action_results
                .push(ActionResult::error(ACCESS_REQUIRED));
            return;
        }
        if matches!(action, Show) {
            self.open = true;
            if !view.saving && !view.preview.is_playing() {
                view.validation_error = None;
                self.load(view, client);
            }
            return;
        }
        if !self.open || view.loading || view.saving || view.decoding {
            return;
        }
        if view.preview.is_playing() {
            match action {
                StopPreview => {
                    self.finish_preview(view);
                    return;
                }
                Trim(..) | NudgeTrim(..) | Gain(_)
                    if matches!(view.preview, SoundboardPreview::Upload { .. }) => {}
                _ => return,
            }
        }
        view.validation_error = None;
        match action {
            ChooseFile(path) => {
                self.clear_source(view);
                view.decoding = true;
                self.tasks.spawn(async move {
                    Completion::Selected(
                        spawn_blocking(move || SoundSource::open(&path))
                            .await
                            .unwrap_or(Err(SoundSourceError::Read(
                                "Could not read the audio file.",
                            )))
                            .map_err(|error| match error {
                                SoundSourceError::Invalid(message) => {
                                    WorkError::Validation(message)
                                }
                                SoundSourceError::Read(message) => WorkError::Action(message),
                            }),
                    )
                });
            }
            Discard => self.clear_source(view),
            Trim(target, seconds) | NudgeTrim(target, seconds) => {
                if let (Some(source), Some(crop)) = (&self.source, &mut view.selection) {
                    let previous = (crop.start, crop.end);
                    let seconds = if matches!(action, NudgeTrim(..)) {
                        seconds
                            + match target {
                                SoundboardTrim::Start | SoundboardTrim::Region => crop.start,
                                SoundboardTrim::End => crop.end,
                            }
                    } else {
                        seconds
                    };
                    source.trim(crop, target, seconds);
                    if previous != (crop.start, crop.end) && view.preview.is_playing() {
                        self.finish_preview(view);
                    }
                }
            }
            Gain(gain) => {
                if let Some(crop) = &mut view.selection {
                    crop.gain = gain;
                    if view.preview.is_playing() {
                        self.finish_preview(view);
                    }
                }
            }
            Preview => {
                let (Some(source), Some(crop)) = (self.source.clone(), view.selection) else {
                    view.validation_error = Some(CHOOSE_AUDIO);
                    return;
                };
                let (sender, receiver) = watch::channel(output);
                let (progress, positions) = watch::channel(Duration::ZERO);
                self.preview_output = Some(sender);
                self.preview_progress = Some(positions);
                view.preview = SoundboardPreview::Upload {
                    position: crop.start,
                };
                self.tasks.spawn(async move {
                    Completion::Previewed(
                        async {
                            let clip = prepare_crop(source, crop).await?;
                            soundboard::play(clip.wav, receiver, Some(progress))
                                .await
                                .map_err(|_| WorkError::Action("Could not preview this sound."))
                        }
                        .await,
                    )
                });
            }
            PreviewSaved(id) => {
                let Ok(id) = Uuid::parse_str(&id) else { return };
                let (sender, receiver) = watch::channel(output);
                self.preview_output = Some(sender);
                view.preview = SoundboardPreview::Saved;
                let client = client.clone();
                self.tasks.spawn(async move {
                    Completion::Previewed(
                        async {
                            let wav = client.soundboard_file(id).await?;
                            soundboard::play(wav, receiver, None)
                                .await
                                .map_err(|_| WorkError::Action("Could not preview this sound."))
                        }
                        .await,
                    )
                });
            }
            Add { .. } | Edit { .. } | Delete(_) => {
                view.saving = true;
                let source = self.source.clone();
                let client = client.clone();
                let crop = view.selection;
                self.mutations.spawn(async move {
                    let kind = match &action {
                        Add { .. } => Mutation::Add,
                        Edit { .. } => Mutation::Edit,
                        Delete(_) => Mutation::Delete,
                        _ => unreachable!(),
                    };
                    (kind, mutate(&client, source, crop, action).await)
                });
            }
            Show | Close | StopPreview => {}
        }
    }

    pub fn complete(&mut self, sidebar: &mut Sidebar, result: Completion) -> Result<(), Failure> {
        let view = &mut sidebar.admin_soundboard;
        match result {
            Completion::Loaded(result) => {
                view.loading = false;
                match result {
                    Ok(clips) => view.clips = Some(clips.into()),
                    Err(error) => {
                        view.directory_error =
                            Some(request_message(error, "Could not load sounds. Try again.")?)
                    }
                }
            }
            Completion::Selected(result) => {
                view.decoding = false;
                match result {
                    Ok(source) => {
                        view.waveform = Some(source.waveform.clone());
                        view.selection = Some(source.initial_crop());
                        self.source = Some(Arc::new(source));
                    }
                    Err(error) => {
                        self.show_error(sidebar, error, "Could not read the audio file.")?
                    }
                }
            }
            Completion::Previewed(result) => {
                self.finish_preview(view);
                if let Err(error) = result {
                    self.show_error(sidebar, error, "Could not preview this sound.")?;
                }
            }
        }
        Ok(())
    }

    pub fn mutated(
        &mut self,
        sidebar: &mut Sidebar,
        client: &SpkClient,
        kind: Mutation,
        result: Result<(), WorkError>,
    ) -> Result<bool, Failure> {
        let view = &mut sidebar.admin_soundboard;
        view.saving = false;
        match result {
            Ok(()) => {
                let message = match kind {
                    Mutation::Add => {
                        self.clear_source(view);
                        "Sound added."
                    }
                    Mutation::Edit => "Sound updated.",
                    Mutation::Delete => "Sound deleted.",
                };
                view.mutation_revision += 1;
                if self.open {
                    self.load(view, client);
                }
                sidebar.action_results.push(ActionResult::success(message));
                Ok(true)
            }
            Err(error) => {
                self.show_error(
                    sidebar,
                    error,
                    match kind {
                        Mutation::Add => "Could not save the sound. Try again.",
                        Mutation::Edit => "Could not update the sound. Try again.",
                        Mutation::Delete => "Could not delete the sound. Try again.",
                    },
                )?;
                let view = &mut sidebar.admin_soundboard;
                if self.open && view.clips.is_none() {
                    self.load(view, client);
                }
                Ok(false)
            }
        }
    }

    fn show_error(
        &self,
        sidebar: &mut Sidebar,
        error: WorkError,
        fallback: &'static str,
    ) -> Result<(), Failure> {
        let message = match error {
            WorkError::Validation(message) => {
                if self.open {
                    sidebar.admin_soundboard.validation_error = Some(message);
                }
                return Ok(());
            }
            WorkError::Action(message) => message,
            WorkError::Request(error) => request_message(error, fallback)?,
        };
        sidebar.action_results.push(ActionResult::error(message));
        Ok(())
    }
}

async fn prepare_crop(
    source: Arc<SoundSource>,
    crop: SoundboardCrop,
) -> Result<PreparedSound, WorkError> {
    spawn_blocking(move || source.crop(crop))
        .await
        .map_err(|_| WorkError::Action("Could not prepare the selected audio."))?
        .map_err(WorkError::Validation)
}

async fn mutate(
    client: &SpkClient,
    source: Option<Arc<SoundSource>>,
    crop: Option<SoundboardCrop>,
    action: SoundboardAdminAction,
) -> Result<(), WorkError> {
    match action {
        SoundboardAdminAction::Add { name, icon } => {
            let name = sound_name(&name, &icon)?;
            let (Some(source), Some(crop)) = (source, crop) else {
                return Err(WorkError::Validation(CHOOSE_AUDIO));
            };
            let clip = prepare_crop(source, crop).await?;
            client
                .add_soundboard_clip(&name, &icon, clip.duration_ms, clip.wav)
                .await?;
        }
        SoundboardAdminAction::Edit { id, name, icon } => {
            let name = sound_name(&name, &icon)?;
            client
                .edit_soundboard_clip(sound_id(&id)?, &name, &icon)
                .await?;
        }
        SoundboardAdminAction::Delete(id) => client.delete_soundboard_clip(sound_id(&id)?).await?,
        _ => unreachable!(),
    }
    Ok(())
}

fn sound_id(id: &str) -> Result<Uuid, WorkError> {
    Uuid::parse_str(id).map_err(|_| WorkError::Action("This sound is unavailable."))
}

fn sound_name(name: &str, icon: &str) -> Result<String, WorkError> {
    let clean: String = name.chars().filter(|ch| !ch.is_ascii_control()).collect();
    let name = clean.trim();
    if !(1..=32).contains(&name.encode_utf16().count()) {
        return Err(WorkError::Validation(
            "Sound names must be 1–32 characters.",
        ));
    }
    if !(1..=64).contains(&icon.len()) || !crate::api::has_icon_name_characters(icon) {
        return Err(WorkError::Validation("Choose a valid sound icon."));
    }
    Ok(name.to_owned())
}

fn request_message(error: Failure, fallback: &'static str) -> Result<&'static str, Failure> {
    match error {
        error if error.unauthorized() => Err(error),
        Failure::Http(StatusCode::FORBIDDEN) => Ok(ACCESS_REQUIRED),
        Failure::Http(StatusCode::NOT_FOUND) => Ok("This sound is no longer available."),
        _ => Ok(fallback),
    }
}
