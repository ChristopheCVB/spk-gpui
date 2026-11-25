mod audio;
mod linked_images;
mod playback;
mod video;

use std::{
    collections::HashMap,
    io::BufReader,
    path::{Path, PathBuf},
};

use rodio::buffer::SamplesBuffer;
use tempfile::{Builder, NamedTempFile, TempPath};
use tokio::sync::oneshot;
use uuid::Uuid;
use video::VideoClip;

use super::*;
use crate::{
    api::{Attachment, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENT_PREVIEW_BYTES, MAX_ATTACHMENTS},
    session::{AttachmentInput, UploadStatus, image_preview},
};
use audio::AudioClip;
use linked_images::{LinkImage, LinkImageKey};
use playback::Playback;

const MAX_INLINE_LOADS: usize = 4;

#[derive(Default)]
pub(in crate::ui) struct AttachmentUi {
    picker: Option<Uuid>,
    picker_error: Option<&'static str>,
    cards: HashMap<String, AttachmentCard>,
    link_images: HashMap<LinkImageKey, LinkImage>,
    inline_loads: usize,
    opened_files: HashMap<String, TempPath>,
    playback: Option<Playback>,
    media_start: Option<Uuid>,
}

#[derive(Default)]
struct AttachmentCard {
    revealed: bool,
    pending: Option<Uuid>,
    error: Option<&'static str>,
    preview_pending: Option<Uuid>,
    preview_error: Option<&'static str>,
    preview_attempted: bool,
    preview: Option<Preview>,
}

impl AttachmentCard {
    fn request(&mut self, access: Access) -> &mut Option<Uuid> {
        if matches!(access, Access::Preview) {
            &mut self.preview_pending
        } else {
            &mut self.pending
        }
    }

    fn begin_request(&mut self, access: Access, spoiler: bool) -> Option<Uuid> {
        let preview = matches!(access, Access::Preview);
        if self.request(access).is_some()
            || (spoiler && !self.revealed)
            || (preview && self.preview_attempted)
        {
            return None;
        }

        let operation = Uuid::new_v4();
        *self.request(access) = Some(operation);
        if preview {
            self.preview_attempted = true;
        } else {
            self.error = None;
        }
        Some(operation)
    }

    fn media_ready(&self) -> bool {
        self.preview.as_ref().is_some_and(Preview::is_media)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachmentKind {
    Image,
    Text,
    Audio,
    WebmAudio,
    Video,
    File,
}

impl AttachmentKind {
    fn classify(file: &Attachment) -> Self {
        match file.mime.as_str() {
            mime if mime.starts_with("image/") => Self::Image,
            "audio/webm" => Self::WebmAudio,
            mime if is_audio(mime) => Self::Audio,
            mime if is_video(mime) => Self::Video,
            mime if mime.starts_with("audio/") || mime.starts_with("video/") => Self::File,
            _ if is_text(file) => Self::Text,
            _ => Self::File,
        }
    }

    fn automatic_preview(self) -> bool {
        matches!(self, Self::Image | Self::Text)
    }

    fn media_label(self) -> Option<&'static str> {
        match self {
            Self::Audio | Self::WebmAudio => Some("audio"),
            Self::Video => Some("video"),
            _ => None,
        }
    }
}

const TEXT_EXCERPT_BYTES: usize = 64 * 1024;
const TEXT_INITIAL_LINES: usize = 25;

struct TextPreview {
    text: String,
    excerpt_end: usize,
    excerpt_lines: usize,
    shown: usize,
}

impl TextPreview {
    fn new(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes).into_owned();
        let excerpt_end = if text.len() > TEXT_EXCERPT_BYTES {
            let end = text.floor_char_boundary(TEXT_EXCERPT_BYTES);
            text[..end].rfind('\n').unwrap_or(end)
        } else {
            text.len()
        };
        Self {
            excerpt_lines: text[..excerpt_end].lines().count(),
            excerpt_end,
            text,
            shown: TEXT_INITIAL_LINES,
        }
    }

    fn displayed(&self) -> String {
        self.text[..self.excerpt_end]
            .lines()
            .take(self.shown)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn can_page(&self) -> bool {
        self.shown < self.excerpt_lines
    }

    fn has_more(&self) -> bool {
        self.can_page() || self.excerpt_end < self.text.len()
    }

    fn can_collapse(&self) -> bool {
        !self.has_more() && self.shown > TEXT_INITIAL_LINES
    }

    fn show_all(&mut self) {
        self.excerpt_end = self.text.len();
        self.excerpt_lines = self.text.lines().count();
        self.shown = usize::MAX;
    }
}

enum Preview {
    Image(Arc<ChatImage>),
    Text(TextPreview),
    Audio(AudioClip),
    Video(VideoClip),
}

impl Preview {
    fn is_media(&self) -> bool {
        matches!(self, Self::Audio(_) | Self::Video(_))
    }

    fn audio_samples(&self) -> Option<SamplesBuffer> {
        match self {
            Self::Audio(clip) => Some(clip),
            Self::Video(clip) => clip.audio.as_ref(),
            _ => None,
        }
        .map(|clip| clip.samples.clone())
    }

    fn drop_images(self, window: &mut Window) {
        match self {
            Self::Image(image) => {
                let _ = window.drop_image(image.pixels.clone());
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

#[derive(Clone, Copy)]
enum Access {
    Preview,
    Play,
    Open(&'static str),
    OpenImage,
    Download,
}

enum AttachmentData {
    Preview(Vec<u8>),
    Original(NamedTempFile),
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
            self.sync_link_images(previous, next, switched, window);
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
    pub(in crate::ui) fn paste_attachment_paths(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(paths) = super::editor::clipboard_file_paths(cx) else {
            return false;
        };
        self.add_attachment_paths(paths, cx);
        true
    }

    pub(super) fn add_attachment_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.add_attachments(paths.into_iter().map(AttachmentInput::Path).collect(), cx);
    }

    fn add_attachments(&mut self, files: Vec<AttachmentInput>, cx: &mut Context<Self>) {
        if !files.is_empty()
            && let Some(chat) = chat(&self.view).filter(|chat| chat.can_send())
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
                    files.push(if image.bytes().len() as u64 > MAX_ATTACHMENT_BYTES {
                        AttachmentInput::Oversized
                    } else {
                        AttachmentInput::Clipboard {
                            filename: format!("pasted-image.{}", format.extension()),
                            mime: format.mime_type(),
                            bytes: bytes::Bytes::copy_from_slice(image.bytes()),
                        }
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
        tool_button("add-attachments", IconName::Paperclip, "Attach files")
            .disabled(self.chat_attachments.picker.is_some() || !chat.can_send())
            .on_click(cx.listener(move |app, _, window, cx| {
                app.choose_attachments(channel.clone(), window, cx)
            }))
    }

    pub(super) fn attachment_queue(&self, chat: &ChatView, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let disabled = !chat.can_send();
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
                let retry_channel = channel.clone();
                let status = match file.status {
                    UploadStatus::Uploading(percent) => format!("Uploading {percent}%"),
                    UploadStatus::Ready(_) => "Ready".into(),
                    UploadStatus::Failed(error) => error.into(),
                };
                div()
                    .id(SharedString::from(key.to_string()))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(
                        file.preview
                            .as_ref()
                            .filter(|_| !file.spoiler)
                            .map(|image| chat_image(image.clone()).w(px(56.)).h(px(42.))),
                    )
                    .child(Label::new(file.filename.clone()).max_w_full().truncate())
                    .child(
                        div()
                            .id("upload-status")
                            .role(Role::Status)
                            .max_w_full()
                            .child(Label::new(status).text_xs().text_color(
                                if matches!(file.status, UploadStatus::Failed(_)) {
                                    theme.danger
                                } else {
                                    theme.muted_foreground
                                },
                            )),
                    )
                    .child(
                        control_button("spoiler")
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
                    .when(matches!(file.status, UploadStatus::Failed(_)), |row| {
                        row.child(
                            control_button("retry")
                                .ghost()
                                .small()
                                .tab_stop(true)
                                .label("Retry")
                                .disabled(disabled)
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.send(
                                        Command::RetryChatAttachment {
                                            channel_id: retry_channel.clone(),
                                            key,
                                        },
                                        None,
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child(
                        control_button("cancel")
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
                    .max_w_full()
                    .p_2()
                    .rounded_md()
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border);
                if file.spoiler && !card.is_some_and(|card| card.revealed) {
                    let id = file.id.clone();
                    return row.id(SharedString::from(file.id.clone())).child(
                        control_button("reveal")
                            .small()
                            .tab_stop(true)
                            .label("Reveal spoiler attachment")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.preserve_chat_position();
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
                let kind = AttachmentKind::classify(file);
                let extension = open_extension(&file.mime);
                let open = match kind {
                    AttachmentKind::Image
                        if extension.is_some() || matches!(preview, Some(Preview::Image(_))) =>
                    {
                        Some(Access::OpenImage)
                    }
                    AttachmentKind::Image => None,
                    _ => extension.map(Access::Open),
                };
                let preview_pending = card.is_some_and(|card| card.preview_pending.is_some());
                let auto_preview = !card.is_some_and(|card| card.preview_attempted)
                    && file.size <= MAX_ATTACHMENT_PREVIEW_BYTES as u64
                    && kind.automatic_preview();
                if auto_preview {
                    let channel = message.channel_id.clone();
                    let message_id = message.id.clone();
                    let file = file.clone();
                    let app = cx.weak_entity();
                    row = when_visible(row, move |window, cx| {
                        let _ = app.update(cx, |app, cx| {
                            app.access_attachment(
                                channel.clone(),
                                message_id.clone(),
                                file.clone(),
                                Access::Preview,
                                window,
                                cx,
                            );
                        });
                    });
                }
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
                            .when_some(
                                kind.media_label()
                                    .filter(|_| file.size <= MAX_ATTACHMENT_PREVIEW_BYTES as u64),
                                |actions, kind| {
                                    actions.child(self.attachment_media_controls(
                                        message, file, kind, pending, cx,
                                    ))
                                },
                            )
                            .when_some(open, |actions, access| {
                                actions.child(
                                    self.attachment_access_button(
                                        access, message, file, pending, cx,
                                    ),
                                )
                            })
                            .child(self.attachment_access_button(
                                Access::Download,
                                message,
                                file,
                                pending,
                                cx,
                            )),
                    )
                    .when(pending || preview_pending || auto_preview, |row| {
                        row.child(
                            div()
                                .id("attachment-status")
                                .role(Role::Status)
                                .child(Label::new("Loading attachment…")),
                        )
                    })
                    .children(
                        card.and_then(|card| card.error.or(card.preview_error))
                            .map(|error| Label::new(error).text_color(theme.danger)),
                    );
                let row = row.id(SharedString::from(file.id.clone()));
                match preview {
                    Some(Preview::Image(image)) => {
                        let image = image.clone();
                        let channel = message.channel_id.clone();
                        let message_id = message.id.clone();
                        let file = file.clone();
                        row.child(
                            Button::new("view-attachment-image")
                                .ghost()
                                .tab_stop(true)
                                .accessibility_label(format!("Enlarge {}", file.filename))
                                .max_w_full()
                                .h_auto()
                                .p_0()
                                .child(
                                    chat_image(image.clone())
                                        .max_w_full()
                                        .w(px(360.))
                                        .h(px(240.)),
                                )
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    let (channel, message_id, file) =
                                        (channel.clone(), message_id.clone(), file.clone());
                                    let app = cx.weak_entity();
                                    open_chat_image(
                                        file.filename.clone(),
                                        image.clone(),
                                        move |window, cx| {
                                            window.close_dialog(cx);
                                            let _ = app.update(cx, |app, cx| {
                                                app.access_attachment(
                                                    channel.clone(),
                                                    message_id.clone(),
                                                    file.clone(),
                                                    Access::OpenImage,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    }
                    Some(Preview::Text(text)) => row.child(self.attachment_text(file, text, cx)),
                    Some(Preview::Audio(clip)) => row.when(clip.truncated, |row| {
                        row.child(Label::new("Short preview. Open for the full audio.").text_xs())
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
                        .when(clip.truncated, |row| {
                            row.child(
                                Label::new("Short preview. Open for the full video.").text_xs(),
                            )
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
            Access::Preview | Access::Play => unreachable!("inline attachment access"),
            Access::Open(_) | Access::OpenImage => ("open", "Open"),
            Access::Download => ("download", "Download"),
        };
        let channel = message.channel_id.clone();
        let message_id = message.id.clone();
        let file = file.clone();
        control_button(id)
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
        let auto = matches!(access, Access::Preview);
        let kind = AttachmentKind::classify(&file);
        if matches!(access, Access::Preview | Access::Play)
            && file.size > MAX_ATTACHMENT_PREVIEW_BYTES as u64
        {
            return;
        }
        if auto
            && (!kind.automatic_preview() || self.chat_attachments.inline_loads >= MAX_INLINE_LOADS)
            || !chat(&self.view).is_some_and(|chat| {
                chat.channel_id == channel
                    && chat.history_ready()
                    && chat.messages.iter().any(|item| {
                        item.id == message && item.attachments.iter().any(|item| item.id == file.id)
                    })
            })
        {
            return;
        }
        let state = self
            .chat_attachments
            .cards
            .entry(file.id.clone())
            .or_default();
        let Some(operation) = state.begin_request(access, file.spoiler) else {
            return;
        };
        if auto {
            self.chat_attachments.inline_loads += 1;
        }
        if matches!(access, Access::Play) {
            self.chat_attachments.media_start = Some(operation);
        }
        let file_id = file.id.clone();
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
                let commands = app.update_in(cx, |app, _, _| {
                    app.chat_attachments.cards.get_mut(&file_id)
                        .is_some_and(|card| *card.request(access) == Some(operation))
                        .then(|| app.commands.clone())
                }).map_err(|_| "Attachment unavailable.")?;
                let Some(commands) = commands else {
                    return Ok(None);
                };
                let data = if matches!(access, Access::Preview | Access::Play) {
                    let (reply, result) = oneshot::channel();
                    commands.send(Command::ReadChatAttachment {
                        channel_id: channel,
                        message_id: message,
                        attachment_id: file.id.clone(),
                        reply,
                    }).await.map_err(|_| "Attachment unavailable.")?;
                    AttachmentData::Preview(
                        result.await.map_err(|_| "Attachment unavailable.")?
                            .map_err(|_| "Could not access the attachment.")?,
                    )
                } else {
                    let save_path = destination.clone();
                    let staged = cx.background_executor()
                        .spawn(async move { stage_original(access, save_path.as_deref()) })
                        .await?;
                    let (reply, result) = oneshot::channel();
                    commands.send(Command::DownloadChatAttachment {
                        channel_id: channel,
                        message_id: message,
                        attachment_id: file.id.clone(),
                        destination: staged,
                        reply,
                    }).await.map_err(|_| "Attachment unavailable.")?;
                    AttachmentData::Original(
                        result.await.map_err(|_| "Attachment unavailable.")?
                            .map_err(|_| "Could not access the attachment.")?,
                    )
                };
                let current = app
                    .update_in(cx, |app, _, _| {
                        app.chat_attachments
                            .cards
                            .get_mut(&file_id)
                            .is_some_and(|card| *card.request(access) == Some(operation))
                    })
                    .unwrap_or(false);
                if !current {
                    return Ok(None);
                }
                cx.background_executor()
                    .spawn(async move {
                        match data {
                            AttachmentData::Preview(bytes) => load_preview(kind, bytes),
                            AttachmentData::Original(file) => finish_original(access, file, destination),
                        }.map(Some)
                    })
                    .await
            }
            .await;

            let _ = app.update_in(cx, |app, window, cx| {
                if auto {
                    app.chat_attachments.inline_loads -= 1;
                    cx.notify();
                }
                let current = app.chat_attachments.cards.get_mut(&file_id)
                    .is_some_and(|card| *card.request(access) == Some(operation));
                if !current {
                    return;
                }
                let play = matches!(access, Access::Play);
                let start = play && app.chat_attachments.media_start == Some(operation);
                if auto || start && matches!(&result, Ok(Some(AccessResult::Preview(_)))) {
                    app.preserve_chat_position();
                }
                if start && matches!(&result, Ok(Some(AccessResult::Preview(preview))) if preview.is_media()) {
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
                {
                    *state.request(access) = None;
                    match result {
                        Ok(Some(AccessResult::Preview(preview))) if !play || start => {
                            if let Some(previous) = state.preview.replace(preview) {
                                previous.drop_images(window);
                            }
                        }
                        Ok(Some(AccessResult::Preview(_))) => {}
                        Ok(Some(AccessResult::Open(path))) => {
                            cx.open_with_system(&path);
                            app.chat_attachments
                                .opened_files
                                .insert(file_id.clone(), path);
                        }
                        Ok(Some(AccessResult::Downloaded) | None) => {}
                        Err(error) if auto => state.preview_error = Some(error),
                        Err(error) => state.error = Some(error),
                    }
                    cx.notify();
                }
                if start {
                    app.chat_attachments.media_start = None;
                    if app.chat_attachments.cards.get(&file_id).is_some_and(AttachmentCard::media_ready) {
                        app.toggle_attachment_media(file_id, window, cx);
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn attachment_media_controls(
        &self,
        message: &ChatMessage,
        file: &Attachment,
        kind: &'static str,
        pending: bool,
        cx: &Context<Self>,
    ) -> Button {
        let playing = self
            .chat_attachments
            .playback
            .as_ref()
            .is_some_and(|playback| playback.id == file.id && playback.started.is_some());
        let file = file.clone();
        let channel = message.channel_id.clone();
        let message = message.id.clone();
        control_button("play-attachment")
            .small()
            .tab_stop(true)
            .label(if playing {
                format!("Pause {kind}")
            } else {
                format!("Play {kind}")
            })
            .disabled(pending)
            .on_click(cx.listener(move |app, _, window, cx| {
                if app
                    .chat_attachments
                    .cards
                    .get(&file.id)
                    .is_some_and(AttachmentCard::media_ready)
                {
                    app.toggle_attachment_media(file.id.clone(), window, cx);
                } else {
                    app.access_attachment(
                        channel.clone(),
                        message.clone(),
                        file.clone(),
                        Access::Play,
                        window,
                        cx,
                    );
                }
            }))
    }

    fn attachment_text(&self, file: &Attachment, text: &TextPreview, cx: &Context<Self>) -> Div {
        let action = |id, label, update: fn(&mut TextPreview)| {
            let file = file.id.clone();
            control_button(id)
                .ghost()
                .small()
                .tab_stop(true)
                .label(label)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.preserve_chat_position();
                    if let Some(Preview::Text(text)) = app
                        .chat_attachments
                        .cards
                        .get_mut(&file)
                        .and_then(|card| card.preview.as_mut())
                    {
                        update(text);
                        cx.notify();
                    }
                }))
        };
        div()
            .max_w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div().max_w_full().p_2().child(
                    Label::new(text.displayed())
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone()),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .when(text.can_page(), |row| {
                        row.child(action("text-more", "Show 50 more", |text| text.shown += 50))
                    })
                    .when(text.has_more(), |row| {
                        row.child(action("text-all", "Show all", TextPreview::show_all))
                    })
                    .when(text.can_collapse(), |row| {
                        row.child(action("text-collapse", "Collapse", |text| {
                            text.shown = TEXT_INITIAL_LINES;
                        }))
                    }),
            )
    }
}

fn load_preview(kind: AttachmentKind, bytes: Vec<u8>) -> Result<AccessResult, &'static str> {
    match kind {
        AttachmentKind::Image => image_preview(&bytes)
            .map(Preview::Image)
            .ok_or("Image preview unavailable. Try downloading the file."),
        AttachmentKind::Text => Ok(Preview::Text(TextPreview::new(&bytes))),
        AttachmentKind::WebmAudio => AudioClip::decode_webm(bytes).map(Preview::Audio),
        AttachmentKind::Audio => AudioClip::decode(&bytes).map(Preview::Audio),
        AttachmentKind::Video => VideoClip::decode(bytes).map(Preview::Video),
        AttachmentKind::File => Err("Attachment preview unavailable."),
    }
    .map(AccessResult::Preview)
}

fn stage_original(
    access: Access,
    destination: Option<&Path>,
) -> Result<NamedTempFile, &'static str> {
    if let Some(destination) = destination {
        let directory = destination
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        return NamedTempFile::new_in(directory).map_err(|_| "Could not save the file.");
    }
    let suffix = match access {
        Access::Open(extension) => extension,
        _ => "",
    };
    Builder::new()
        .prefix("spk-")
        .suffix(suffix)
        .tempfile()
        .map_err(|_| "Could not open the file.")
}

fn finish_original(
    access: Access,
    mut file: NamedTempFile,
    destination: Option<PathBuf>,
) -> Result<AccessResult, &'static str> {
    match access {
        Access::Open(_) => Ok(AccessResult::Open(file.into_temp_path())),
        Access::OpenImage => {
            let error = "Could not open the image.";
            let reader = image::ImageReader::new(BufReader::new(file.as_file_mut()))
                .with_guessed_format()
                .map_err(|_| error)?;
            let extension = reader
                .format()
                .and_then(|format| format.extensions_str().first())
                .ok_or(error)?;
            let path = Builder::new()
                .prefix("spk-")
                .suffix(&format!(".{extension}"))
                .tempfile()
                .map_err(|_| error)?
                .into_temp_path();
            file.persist(&path).map_err(|_| error)?;
            Ok(AccessResult::Open(path))
        }
        Access::Download => {
            let error = "Could not save the file.";
            file.persist(destination.ok_or(error)?).map_err(|_| error)?;
            Ok(AccessResult::Downloaded)
        }
        Access::Preview | Access::Play => unreachable!("received original for preview"),
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
        "image/png" | "image/x-png" => ".png",
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
    use super::{
        Access, AccessResult, Attachment, AttachmentCard, AttachmentKind, Preview, TextPreview,
        finish_original, load_preview, stage_original,
    };
    use rodio::Source;
    use std::{
        io::{Seek, Write},
        time::Duration,
    };

    #[test]
    fn sent_gif_attachment_keeps_both_preview_frames() {
        let fixture = include_bytes!("../../../tests/fixtures/attachments/preview.gif");
        let Ok(AccessResult::Preview(Preview::Image(preview))) =
            load_preview(AttachmentKind::Image, fixture.to_vec())
        else {
            panic!("expected an image preview for the sent GIF");
        };

        assert_eq!(preview.pixels.frame_count(), 2);
        assert_eq!(&preview.pixels.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
        assert_eq!(&preview.pixels.as_bytes(1).unwrap()[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn sent_gif_preview_honors_finite_repeats() {
        let mut fixture =
            include_bytes!("../../../tests/fixtures/attachments/preview.gif").to_vec();
        let extension = fixture
            .windows(11)
            .position(|data| data == b"NETSCAPE2.0")
            .unwrap();
        for extension_name in [b"NETSCAPE2.0", b"ANIMEXTS1.0"] {
            fixture[extension..extension + 11].copy_from_slice(extension_name);
            fixture[extension + 13] = 1;
            let Ok(AccessResult::Preview(Preview::Image(preview))) =
                load_preview(AttachmentKind::Image, fixture.clone())
            else {
                panic!("expected an image preview for the sent GIF");
            };

            assert_eq!(preview.frame_at(Duration::from_millis(610)).0, 0);
            assert_eq!(preview.frame_at(Duration::from_millis(1220)), (1, None));
        }
    }

    #[test]
    fn gif_loop_identifier_in_unrelated_extension_payload_is_ignored() {
        let mut fixture =
            include_bytes!("../../../tests/fixtures/attachments/preview.gif").to_vec();
        let extension = fixture
            .windows(12)
            .position(|data| data == b"\x0bNETSCAPE2.0")
            .unwrap();
        fixture.splice(extension..extension, *b"\x0bOTHERAPP1.0");
        let Ok(AccessResult::Preview(Preview::Image(preview))) =
            load_preview(AttachmentKind::Image, fixture)
        else {
            panic!("expected an image preview for the sent GIF");
        };

        assert_eq!(preview.frame_at(Duration::from_millis(610)), (1, None));
    }

    #[test]
    fn large_single_frame_gif_uses_the_static_preview_budget() {
        let mut fixture = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut fixture, 4096, 3072, &[255, 0, 0]).unwrap();
            encoder
                .write_frame(&gif::Frame {
                    width: 1,
                    height: 1,
                    buffer: std::borrow::Cow::Borrowed(&[0]),
                    ..Default::default()
                })
                .unwrap();
        }
        let Ok(AccessResult::Preview(Preview::Image(preview))) =
            load_preview(AttachmentKind::Image, fixture)
        else {
            panic!("expected a static image preview for the single-frame GIF");
        };
        assert_eq!(preview.pixels.frame_count(), 1);
    }

    #[test]
    fn sent_gif_preview_rejects_truncated_and_oversized_animations() {
        let mut fixture =
            include_bytes!("../../../tests/fixtures/attachments/preview.gif").to_vec();
        let fallback = "Image preview unavailable. Try downloading the file.";

        assert_eq!(
            load_preview(AttachmentKind::Image, fixture[..fixture.len() - 1].to_vec()).err(),
            Some(fallback),
        );
        // A 6000-by-6000 canvas fits the source pixel budget for one frame.
        fixture[6..8].copy_from_slice(&6000_u16.to_le_bytes());
        fixture[8..10].copy_from_slice(&6000_u16.to_le_bytes());
        assert_eq!(
            load_preview(AttachmentKind::Image, fixture).err(),
            Some(fallback)
        );
    }

    #[test]
    fn text_preview_expands_in_pages_and_restores_the_initial_excerpt() {
        let contents = (1..=80)
            .map(|line| format!("Line {line}\n"))
            .collect::<String>();
        let mut preview = TextPreview::new(contents.as_bytes());
        let initial = preview.displayed();
        assert_eq!(initial.lines().count(), 25);
        assert!(preview.has_more());
        preview.shown += 50;
        assert_eq!(preview.displayed().lines().count(), 75);
        assert!(preview.has_more());
        preview.show_all();
        assert_eq!(preview.displayed(), contents.trim_end());
        assert!(!preview.has_more());
        preview.shown = 25;
        assert_eq!(preview.displayed(), initial);

        let overlong = "€".repeat(30_000);
        let mut preview = TextPreview::new(overlong.as_bytes());
        assert!(preview.displayed().len() <= 64 * 1024);
        assert!(preview.has_more());
        preview.show_all();
        assert_eq!(preview.displayed(), overlong);
        assert!(preview.can_collapse());
    }

    #[test]
    fn paging_does_not_cross_the_initial_text_excerpt() {
        let contents = "x".repeat(1000) + "\n";
        let mut preview = TextPreview::new(contents.repeat(100).as_bytes());
        preview.shown = 75;

        assert_eq!(preview.displayed().lines().count(), 65);
        assert!(!preview.can_page());
        assert!(preview.has_more());
        assert!(!preview.can_collapse());

        preview.show_all();
        assert_eq!(preview.displayed().lines().count(), 100);
        assert!(preview.can_collapse());
    }

    #[test]
    fn preview_loading_leaves_file_actions_available() {
        let mut card = AttachmentCard::default();
        assert!(card.begin_request(Access::Preview, true).is_none());
        let preview = card.begin_request(Access::Preview, false).unwrap();
        assert!(card.begin_request(Access::OpenImage, false).is_some());
        assert_eq!(*card.request(Access::Preview), Some(preview));

        *card.request(Access::OpenImage) = None;
        assert!(card.begin_request(Access::Download, false).is_some());
        assert_eq!(*card.request(Access::Preview), Some(preview));
    }

    #[test]
    fn opens_the_original_image_using_its_recognized_format() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        let file = Attachment {
            id: "image".into(),
            filename: "uploaded-image".into(),
            mime: "image/x-png".into(),
            size: bytes.len() as u64,
            spoiler: false,
        };
        let kind = AttachmentKind::classify(&file);
        assert!(matches!(
            load_preview(kind, bytes.clone()),
            Ok(AccessResult::Preview(Preview::Image(_)))
        ));
        let original = |bytes: &[u8]| {
            let mut file = stage_original(Access::OpenImage, None).unwrap();
            file.write_all(bytes).unwrap();
            file.rewind().unwrap();
            finish_original(Access::OpenImage, file, None)
        };
        let Ok(AccessResult::Open(path)) = original(&bytes) else {
            panic!("a decoded image should open in the system viewer");
        };
        assert_eq!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("png")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(original(b"not an image").is_err());
    }

    #[test]
    fn previews_audio_only_webm() {
        let bytes = include_bytes!("../../../tests/fixtures/attachments/preview.weba");
        let Ok(AccessResult::Preview(Preview::Audio(clip))) =
            load_preview(AttachmentKind::WebmAudio, bytes.to_vec())
        else {
            panic!("audio-only WebM should open an inline audio preview");
        };
        assert!(clip.samples.take(4_800).any(|sample| sample.abs() > 0.05));
    }

    #[test]
    fn previews_audio_instead_of_rendering_it_as_text() {
        let wav = b"RIFF\x2A\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xBB\0\0\0\x77\x01\0\x02\0\x10\0data\x06\0\0\0\0\x80\0\0\0\x40";
        let file = Attachment {
            id: "sample".into(),
            filename: "recording.txt".into(),
            mime: "audio/wav".into(),
            size: wav.len() as u64,
            spoiler: false,
        };
        let kind = AttachmentKind::classify(&file);
        assert_eq!(kind.media_label(), Some("audio"));
        assert!(!kind.automatic_preview());
        let result = load_preview(kind, wav.to_vec());
        let Ok(AccessResult::Preview(Preview::Audio(clip))) = result else {
            panic!("WAV attachment should produce an inline audio preview");
        };
        assert_eq!(clip.samples.channels().get(), 1);
        assert_eq!(clip.samples.sample_rate().get(), 48_000);
        assert_eq!(clip.samples.collect::<Vec<_>>(), [-1.0, 0.0, 0.5]);
        assert!(!clip.truncated);
        assert!(load_preview(kind, b"bad WAV".to_vec()).is_err());
    }
}
