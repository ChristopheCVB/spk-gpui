mod audio;
mod video;

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
};

use tempfile::{Builder, NamedTempFile, TempPath};
use tokio::sync::oneshot;
use uuid::Uuid;
use video::VideoClip;

use super::*;
use crate::{
    api::{Attachment, MAX_ATTACHMENTS},
    session::{AttachmentInput, UploadStatus, image_preview},
    voice::MediaOutput,
};
use audio::{AudioClip, AudioPlayback, BAD_OUTPUT};

#[derive(Default)]
pub(in crate::ui) struct AttachmentUi {
    picker: Option<Uuid>,
    picker_error: Option<&'static str>,
    cards: HashMap<String, AttachmentCard>,
    opened_files: HashMap<String, TempPath>,
    playback: Option<Playback>,
}

#[derive(Default)]
struct AttachmentCard {
    revealed: bool,
    pending: Option<Uuid>,
    error: Option<&'static str>,
    preview: Option<Preview>,
}

enum Preview {
    Image(Arc<RenderImage>),
    Text(String),
    Audio(AudioClip),
    Video(VideoClip),
}

impl Preview {
    fn is_media(&self) -> bool {
        matches!(self, Self::Audio(_) | Self::Video(_))
    }

    fn drop_images(self, window: &mut Window) {
        match self {
            Self::Image(image) => {
                let _ = window.drop_image(image);
            }
            Self::Video(clip) => {
                for image in clip.images() {
                    let _ = window.drop_image(image);
                }
            }
            _ => {}
        }
    }
}

struct Playback {
    id: String,
    token: Uuid,
    audio: Option<AudioPlayback>,
    started: Option<Instant>,
    elapsed: Duration,
}

impl Playback {
    fn position(&self) -> Duration {
        self.elapsed + self.started.map_or(Duration::ZERO, |start| start.elapsed())
    }

    fn toggle(&mut self) {
        if let Some(start) = self.started.take() {
            self.elapsed += start.elapsed();
            if let Some(audio) = &self.audio {
                audio.player.pause();
            }
        } else {
            self.started = Some(Instant::now());
            if let Some(audio) = &self.audio {
                audio.player.play();
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Access {
    Preview,
    Open(&'static str),
    Download,
}

enum AccessResult {
    Preview(Preview),
    Open(TempPath),
    Downloaded,
}

impl AttachmentUi {
    pub(super) fn sync(
        &mut self,
        previous: Option<&ChatView>,
        next: Option<&ChatView>,
        signed_in: bool,
        window: &mut Window,
    ) {
        let switched = previous.map(|chat| &chat.channel_id) != next.map(|chat| &chat.channel_id);
        if switched {
            self.picker_error = None;
        }
        if switched
            || !previous
                .zip(next)
                .is_some_and(|(previous, next)| Arc::ptr_eq(&previous.messages, &next.messages))
        {
            self.cards.retain(|id, card| {
                let keep = !switched
                    && next.is_some_and(|chat| {
                        chat.messages
                            .iter()
                            .any(|message| message.attachments.iter().any(|file| &file.id == id))
                    });
                if !keep && let Some(preview) = card.preview.take() {
                    preview.drop_images(window);
                }
                keep
            });
            if self
                .playback
                .as_ref()
                .is_some_and(|playback| !self.cards.contains_key(&playback.id))
            {
                self.playback = None;
            }
        }
        if !signed_in {
            self.picker = None;
            self.opened_files.clear();
        }
    }
}

impl Spk {
    pub(super) fn add_attachment_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.add_attachments(paths.into_iter().map(AttachmentInput::Path).collect(), cx);
    }

    fn add_attachments(&mut self, files: Vec<AttachmentInput>, cx: &mut Context<Self>) {
        if !files.is_empty()
            && let Some(chat) =
                chat(&self.view).filter(|chat| chat.can_send() && chat.outgoing.is_none())
        {
            self.chat_attachments.picker_error = None;
            self.send(
                Command::AddChatAttachments {
                    channel_id: chat.channel_id.clone(),
                    files,
                },
                None,
                cx,
            );
        }
    }

    pub(super) fn paste_attachments(
        &mut self,
        item: &ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut files = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(paths) => {
                    files.extend(
                        paths
                            .paths()
                            .iter()
                            .take(MAX_ATTACHMENTS + 1 - files.len())
                            .cloned()
                            .map(AttachmentInput::Path),
                    );
                }
                ClipboardEntry::Image(image) => {
                    let format = image.format();
                    files.push(AttachmentInput::Clipboard {
                        filename: format!("pasted-image.{}", format.extension()),
                        mime: format.mime_type(),
                        bytes: image.bytes()[..image
                            .bytes()
                            .len()
                            .min(crate::api::MAX_ATTACHMENT_BYTES + 1)]
                            .to_vec(),
                    });
                }
                ClipboardEntry::String(_) => {}
            }
            if files.len() > MAX_ATTACHMENTS {
                break;
            }
        }
        let has_files = !files.is_empty();
        self.add_attachments(files, cx);
        has_files
    }

    fn choose_attachments(&mut self, channel: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_attachments.picker.is_some() {
            return;
        }
        let picker = Uuid::new_v4();
        self.chat_attachments.picker = Some(picker);
        self.chat_attachments.picker_error = None;
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn_in(window, async move |app, cx| {
            let result = selection.await;
            let _ = app.update_in(cx, |app, _, cx| {
                if app.chat_attachments.picker != Some(picker) {
                    return;
                }
                app.chat_attachments.picker = None;
                if chat(&app.view).is_some_and(|chat| chat.channel_id == channel) {
                    match result {
                        Ok(Ok(Some(paths))) => app.add_attachment_paths(paths, cx),
                        Ok(Ok(None)) => {}
                        _ => {
                            app.chat_attachments.picker_error =
                                Some("Could not open the file picker.")
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn attachment_picker(&self, chat: &ChatView, cx: &Context<Self>) -> Button {
        let channel = chat.channel_id.clone();
        Button::new("add-attachments")
            .ghost()
            .small()
            .tab_stop(true)
            .label("Attach files")
            .disabled(
                self.chat_attachments.picker.is_some()
                    || !chat.can_send()
                    || chat.outgoing.is_some(),
            )
            .on_click(cx.listener(move |app, _, window, cx| {
                app.choose_attachments(channel.clone(), window, cx)
            }))
    }

    pub(super) fn attachment_queue(&self, chat: &ChatView, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let disabled = chat.outgoing.is_some() || !chat.can_send();
        div()
            .id("attachment-queue")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(154.))
            .overflow_y_scroll()
            .children(
                self.chat_attachments
                    .picker_error
                    .or(chat.draft.upload_notice)
                    .map(|error| Label::new(error).text_color(theme.danger)),
            )
            .children(chat.draft.uploads.iter().map(|file| {
                let key = file.key;
                let channel = chat.channel_id.clone();
                let cancel_channel = channel.clone();
                let status = match file.status {
                    UploadStatus::Uploading(percent) => format!("Uploading {percent}%"),
                    UploadStatus::Ready(_) => "Ready".into(),
                    UploadStatus::Failed(error) => error.into(),
                };
                div()
                    .id(SharedString::from(key.to_string()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(
                        file.preview
                            .as_ref()
                            .filter(|_| !file.spoiler)
                            .map(|image| {
                                img(image.clone())
                                    .w(px(56.))
                                    .h(px(42.))
                                    .object_fit(ObjectFit::Contain)
                            }),
                    )
                    .child(Label::new(file.filename.clone()).truncate())
                    .child(div().id("upload-status").role(Role::Status).child(
                        Label::new(status).text_xs().text_color(
                            if matches!(file.status, UploadStatus::Failed(_)) {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            },
                        ),
                    ))
                    .child(
                        Button::new("spoiler")
                            .ghost()
                            .small()
                            .tab_stop(true)
                            .label(if file.spoiler {
                                "Remove spoiler"
                            } else {
                                "Mark as spoiler"
                            })
                            .disabled(disabled)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.send(
                                    Command::ToggleChatAttachmentSpoiler {
                                        channel_id: channel.clone(),
                                        key,
                                    },
                                    None,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("cancel")
                            .ghost()
                            .small()
                            .tab_stop(true)
                            .label(if matches!(file.status, UploadStatus::Uploading(_)) {
                                "Cancel upload"
                            } else {
                                "Remove file"
                            })
                            .disabled(disabled)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.send(
                                    Command::CancelChatAttachment {
                                        channel_id: cancel_channel.clone(),
                                        key,
                                    },
                                    None,
                                    cx,
                                );
                            })),
                    )
            }))
    }

    pub(super) fn attachment_cards(
        &self,
        message: &ChatMessage,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .children(message.attachments.iter().map(|file| {
                let card = self.chat_attachments.cards.get(&file.id);
                let mut row = div()
                    .id(SharedString::from(file.id.clone()))
                    .max_w_full()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border);
                if file.spoiler && !card.is_some_and(|card| card.revealed) {
                    let id = file.id.clone();
                    return row.child(
                        Button::new("reveal")
                            .small()
                            .tab_stop(true)
                            .label("Reveal spoiler attachment")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.chat_attachments
                                    .cards
                                    .entry(id.clone())
                                    .or_default()
                                    .revealed = true;
                                cx.notify();
                            })),
                    );
                }
                let preview = card.and_then(|card| card.preview.as_ref());
                let pending = card.is_some_and(|card| card.pending.is_some());
                let extension = open_extension(&file.mime);
                let inline = file.mime.starts_with("image/")
                    || is_text(file)
                    || is_audio(&file.mime)
                    || is_video(&file.mime);
                row = row
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .items_center()
                            .child(Label::new(file.filename.clone()).truncate())
                            .child(
                                Label::new(format_size(file.size))
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            )
                            .when(inline, |actions| {
                                actions.child(self.attachment_access_button(
                                    Access::Preview,
                                    message,
                                    file,
                                    pending || preview.is_some(),
                                    cx,
                                ))
                            })
                            .when_some(extension, |actions, extension| {
                                actions.child(self.attachment_access_button(
                                    Access::Open(extension),
                                    message,
                                    file,
                                    pending,
                                    cx,
                                ))
                            })
                            .child(self.attachment_access_button(
                                Access::Download,
                                message,
                                file,
                                pending,
                                cx,
                            )),
                    )
                    .when(pending, |row| {
                        row.child(
                            div()
                                .id("attachment-status")
                                .role(Role::Status)
                                .child(Label::new("Loading attachment…")),
                        )
                    })
                    .children(
                        card.and_then(|card| card.error)
                            .map(|error| Label::new(error).text_color(theme.danger)),
                    );
                match preview {
                    Some(Preview::Image(image)) => row.child(
                        img(image.clone())
                            .max_w_full()
                            .h(px(240.))
                            .object_fit(ObjectFit::Contain),
                    ),
                    Some(Preview::Text(text)) => row.child(
                        div()
                            .max_w_full()
                            .p_2()
                            .child(Label::new(text.clone()).text_xs().font_family("monospace")),
                    ),
                    Some(Preview::Audio(clip)) => row
                        .child(self.attachment_media_controls(file, "audio", pending, cx))
                        .when(clip.truncated, |row| {
                            row.child(Label::new("Short audio preview").text_xs())
                        }),
                    Some(Preview::Video(clip)) => {
                        let position = self
                            .chat_attachments
                            .playback
                            .as_ref()
                            .filter(|playback| playback.id == file.id)
                            .map_or(Duration::ZERO, Playback::position);
                        row.child(
                            img(clip.image_at(position))
                                .max_w_full()
                                .h(px(240.))
                                .object_fit(ObjectFit::Contain),
                        )
                        .child(self.attachment_media_controls(file, "video", pending, cx))
                        .when(clip.truncated, |row| {
                            row.child(Label::new("Short video preview").text_xs())
                        })
                        .when(clip.silent, |row| {
                            row.child(Label::new("Preview without audio").text_xs())
                        })
                    }
                    None => row,
                }
            }))
    }

    fn attachment_access_button(
        &self,
        access: Access,
        message: &ChatMessage,
        file: &Attachment,
        disabled: bool,
        cx: &Context<Self>,
    ) -> Button {
        let (id, label) = match access {
            Access::Preview => ("preview", "Preview"),
            Access::Open(_) => ("open", "Open"),
            Access::Download => ("download", "Download"),
        };
        let channel = message.channel_id.clone();
        let message_id = message.id.clone();
        let file = file.clone();
        Button::new(id)
            .ghost()
            .small()
            .tab_stop(true)
            .label(label)
            .disabled(disabled)
            .on_click(cx.listener(move |app, _, window, cx| {
                app.access_attachment(
                    channel.clone(),
                    message_id.clone(),
                    file.clone(),
                    access,
                    window,
                    cx,
                );
            }))
    }

    fn access_attachment(
        &mut self,
        channel: String,
        message: String,
        file: Attachment,
        access: Access,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self
            .chat_attachments
            .cards
            .entry(file.id.clone())
            .or_default();
        if state.pending.is_some() || (file.spoiler && !state.revealed) {
            return;
        }
        let operation = Uuid::new_v4();
        state.pending = Some(operation);
        let file_id = file.id.clone();
        state.error = None;
        let destination = matches!(access, Access::Download).then(|| {
            let root = dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(std::env::temp_dir);
            let name = safe_filename(&file.filename);
            cx.prompt_for_new_path(&root, Some(&name))
        });
        cx.spawn_in(window, async move |app, cx| {
            let result = async {
                let destination = match destination {
                    Some(selection) => match selection.await {
                        Ok(Ok(Some(path))) => Some(path),
                        Ok(Ok(None)) => return Ok(None),
                        _ => return Err("Could not open the save dialog."),
                    },
                    None => None,
                };
                let (reply, result) = oneshot::channel();
                app.update_in(cx, |app, _, _| {
                    if app
                        .chat_attachments
                        .cards
                        .get(&file_id)
                        .and_then(|card| card.pending)
                        == Some(operation)
                    {
                        let _ = app.commands.try_send(Command::ReadChatAttachment {
                            channel_id: channel,
                            message_id: message,
                            attachment_id: file.id.clone(),
                            reply,
                        });
                    }
                })
                .map_err(|_| "Attachment unavailable.")?;
                let bytes = result
                    .await
                    .map_err(|_| "Attachment unavailable.")?
                    .map_err(|_| "Could not access the attachment.")?;
                let current = app
                    .update_in(cx, |app, _, _| {
                        app.chat_attachments
                            .cards
                            .get(&file_id)
                            .and_then(|card| card.pending)
                            == Some(operation)
                    })
                    .unwrap_or(false);
                if !current {
                    return Ok(None);
                }
                cx.background_executor()
                    .spawn(async move {
                        load_attachment(access, &file.mime, bytes, destination).map(Some)
                    })
                    .await
            }
            .await;

            let _ = app.update_in(cx, |app, window, cx| {
                if app
                    .chat_attachments
                    .cards
                    .get(&file_id)
                    .and_then(|card| card.pending)
                    == Some(operation)
                    && matches!(&result, Ok(Some(AccessResult::Preview(preview))) if preview.is_media())
                {
                    app.chat_attachments.playback = None;
                    for (id, card) in &mut app.chat_attachments.cards {
                        if id != &file_id && card.preview.as_ref().is_some_and(Preview::is_media)
                            && let Some(preview) = card.preview.take()
                        {
                            preview.drop_images(window);
                        }
                    }
                }
                if let Some(state) = app
                    .chat_attachments
                    .cards
                    .get_mut(&file_id)
                    .filter(|card| card.pending == Some(operation))
                {
                    state.pending = None;
                    match result {
                        Ok(Some(AccessResult::Preview(preview))) => {
                            if let Some(previous) = state.preview.replace(preview) {
                                previous.drop_images(window);
                            }
                        }
                        Ok(Some(AccessResult::Open(path))) => {
                            cx.open_with_system(&path);
                            app.chat_attachments
                                .opened_files
                                .insert(file_id.clone(), path);
                        }
                        Ok(Some(AccessResult::Downloaded) | None) => {}
                        Err(error) => state.error = Some(error),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn attachment_media_controls(
        &self,
        file: &Attachment,
        kind: &'static str,
        pending: bool,
        cx: &Context<Self>,
    ) -> Button {
        let id = file.id.clone();
        let playing = self
            .chat_attachments
            .playback
            .as_ref()
            .is_some_and(|playback| playback.id == id && playback.started.is_some());
        Button::new("play-attachment")
            .small()
            .tab_stop(true)
            .label(if playing {
                format!("Pause {kind}")
            } else {
                format!("Play {kind}")
            })
            .disabled(pending)
            .on_click(cx.listener(move |app, _, window, cx| {
                app.toggle_attachment_media(id.clone(), window, cx);
            }))
    }

    fn toggle_attachment_media(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(playback) = self
            .chat_attachments
            .playback
            .as_mut()
            .filter(|playback| playback.id == id)
        {
            playback.toggle();
            cx.notify();
            return;
        }
        let View::SignedIn(sidebar) = &self.view else {
            return;
        };
        let Some(card) = self
            .chat_attachments
            .cards
            .get_mut(&id)
            .filter(|card| card.pending.is_none())
        else {
            return;
        };
        let (samples, video_duration) = match &card.preview {
            Some(Preview::Audio(clip)) => (Some(clip.samples.clone()), None),
            Some(Preview::Video(clip)) => (
                clip.audio.as_ref().map(|audio| audio.samples.clone()),
                Some(clip.duration),
            ),
            _ => return,
        };
        let preferred = &sidebar.voice_intent.preferred;
        let output = MediaOutput {
            device: sidebar
                .devices
                .media_outputs
                .resolve(preferred.media_output.as_ref())
                .cloned(),
            volume: preferred.output_gain(),
        };
        let operation = Uuid::new_v4();
        card.pending = Some(operation);
        card.error = None;
        self.chat_attachments.playback = None;
        cx.spawn_in(window, async move |app, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    samples
                        .map(|samples| AudioPlayback::open(samples, output))
                        .transpose()
                })
                .await;
            let started = app
                .update_in(cx, |app, _, cx| {
                    let Some(card) = app
                        .chat_attachments
                        .cards
                        .get_mut(&id)
                        .filter(|card| card.pending == Some(operation))
                    else {
                        return false;
                    };
                    card.pending = None;
                    cx.notify();
                    if !card.preview.as_ref().is_some_and(Preview::is_media) {
                        return false;
                    }
                    let audio = match result {
                        Ok(audio) => audio,
                        Err(error) => {
                            card.error = Some(error);
                            if video_duration.is_none() {
                                return false;
                            }
                            None
                        }
                    };
                    let mut playback = Playback {
                        id: id.clone(),
                        token: operation,
                        audio,
                        started: None,
                        elapsed: Duration::ZERO,
                    };
                    playback.toggle();
                    app.chat_attachments.playback = Some(playback);
                    true
                })
                .unwrap_or(false);
            if !started {
                return;
            }
            let interval = Duration::from_millis(if video_duration.is_some() { 40 } else { 120 });
            let mut ending = false;
            loop {
                cx.background_executor().timer(interval).await;
                let keep_playing = app
                    .update_in(cx, |app, _, cx| {
                        let Some(playback) = app
                            .chat_attachments
                            .playback
                            .as_mut()
                            .filter(|playback| playback.token == operation)
                        else {
                            return false;
                        };
                        let failed = playback.audio.as_ref().is_some_and(AudioPlayback::failed);
                        if failed {
                            playback.audio = None;
                            if let Some(card) = app.chat_attachments.cards.get_mut(&id) {
                                card.error = Some(BAD_OUTPUT);
                            }
                        }
                        let ended = match video_duration {
                            Some(duration) => playback.position() >= duration,
                            None => playback
                                .audio
                                .as_ref()
                                .is_none_or(|audio| audio.player.empty()),
                        };
                        // Keep the audio device open for one tick while it drains its last samples.
                        if ended && (ending || failed) {
                            app.chat_attachments.playback = None;
                            cx.notify();
                            return false;
                        }
                        ending = ended;
                        if failed || (video_duration.is_some() && playback.started.is_some()) {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_playing {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }
}

fn load_attachment(
    access: Access,
    mime: &str,
    bytes: Vec<u8>,
    destination: Option<PathBuf>,
) -> Result<AccessResult, &'static str> {
    match access {
        Access::Preview if mime.starts_with("image/") => image_preview(&bytes)
            .map(Preview::Image)
            .map(AccessResult::Preview)
            .ok_or("Image preview unavailable. Try downloading the file."),
        Access::Preview if is_audio(mime) => AudioClip::decode(&bytes)
            .map(Preview::Audio)
            .map(AccessResult::Preview),
        Access::Preview if is_video(mime) => VideoClip::decode(bytes)
            .map(Preview::Video)
            .map(AccessResult::Preview),
        Access::Preview => {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(64 * 1024)]);
            let mut lines = text.lines();
            let mut preview = lines.by_ref().take(25).collect::<Vec<_>>().join("\n");
            if lines.next().is_some() || bytes.len() > 64 * 1024 {
                preview.push_str("\n…");
            }
            Ok(AccessResult::Preview(Preview::Text(preview)))
        }
        Access::Open(extension) => {
            let error = "Could not open the file.";
            let mut file = Builder::new()
                .prefix("spk-")
                .suffix(extension)
                .tempfile()
                .map_err(|_| error)?;
            file.write_all(&bytes).map_err(|_| error)?;
            Ok(AccessResult::Open(file.into_temp_path()))
        }
        Access::Download => {
            let error = "Could not save the file.";
            let destination = destination.ok_or(error)?;
            let directory = destination
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut file = NamedTempFile::new_in(directory).map_err(|_| error)?;
            file.write_all(&bytes).map_err(|_| error)?;
            file.persist(destination).map_err(|_| error)?;
            Ok(AccessResult::Downloaded)
        }
    }
}

fn safe_filename(filename: &str) -> String {
    let name: String = filename
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let name = name.trim_matches(['.', ' ']);
    if name.is_empty() {
        "attachment".into()
    } else {
        name.to_owned()
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{bytes} B")
    }
}

fn is_audio(mime: &str) -> bool {
    matches!(
        mime,
        "audio/mpeg"
            | "audio/mp4"
            | "audio/m4a"
            | "audio/x-m4a"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/ogg"
            | "audio/flac"
            | "audio/x-flac"
            | "audio/aac"
    )
}

fn is_video(mime: &str) -> bool {
    matches!(mime, "video/mp4" | "video/webm" | "video/x-matroska")
}

fn is_text(file: &Attachment) -> bool {
    file.size < 1024 * 1024
        && (file.mime.starts_with("text/")
            || matches!(
                file.filename
                    .rsplit_once('.')
                    .map(|(_, suffix)| suffix.to_ascii_lowercase())
                    .as_deref(),
                Some(
                    "ts" | "tsx"
                        | "js"
                        | "jsx"
                        | "mjs"
                        | "json"
                        | "py"
                        | "vue"
                        | "css"
                        | "html"
                        | "xml"
                        | "yml"
                        | "yaml"
                        | "sh"
                        | "sql"
                        | "go"
                        | "rs"
                        | "java"
                        | "c"
                        | "cpp"
                        | "h"
                        | "cs"
                        | "php"
                        | "rb"
                        | "toml"
                        | "ini"
                        | "diff"
                        | "md"
                        | "txt"
                        | "log"
                        | "csv"
                        | "conf"
                        | "env"
                )
            ))
}

fn open_extension(mime: &str) -> Option<&'static str> {
    // Pick passive document and media types; uploading a filename cannot choose an executable association.
    Some(match mime {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        "image/tiff" => ".tiff",
        "audio/mpeg" => ".mp3",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => ".m4a",
        "audio/wav" | "audio/x-wav" => ".wav",
        "audio/ogg" => ".ogg",
        "audio/flac" | "audio/x-flac" => ".flac",
        "audio/aac" => ".aac",
        "audio/webm" => ".weba",
        "video/mp4" => ".mp4",
        "video/mpeg" => ".mpeg",
        "video/webm" => ".webm",
        "video/quicktime" => ".mov",
        "video/x-matroska" => ".mkv",
        "video/x-msvideo" => ".avi",
        "application/pdf" => ".pdf",
        "text/plain" => ".txt",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Access, AccessResult, Preview, load_attachment};
    use rodio::Source;

    #[test]
    fn previews_audio_instead_of_rendering_it_as_text() {
        let wav = b"RIFF\x2A\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xBB\0\0\0\x77\x01\0\x02\0\x10\0data\x06\0\0\0\0\x80\0\0\0\x40";
        let result = load_attachment(Access::Preview, "audio/wav", wav.to_vec(), None);
        let Ok(AccessResult::Preview(Preview::Audio(clip))) = result else {
            panic!("WAV attachment should produce an inline audio preview");
        };
        assert_eq!(clip.samples.channels().get(), 1);
        assert_eq!(clip.samples.sample_rate().get(), 48_000);
        assert_eq!(clip.samples.collect::<Vec<_>>(), [-1.0, 0.0, 0.5]);
        assert!(!clip.truncated);
        assert!(load_attachment(Access::Preview, "audio/wav", b"bad WAV".to_vec(), None).is_err());
    }
}
