use std::{
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
};

use gpui_kit::RenderImage;
use image::{Frame, ImageReader, Limits};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::api::{AttachmentRef, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS};

use super::*;

pub enum AttachmentInput {
    Path(PathBuf),
    Clipboard {
        filename: String,
        mime: &'static str,
        bytes: Vec<u8>,
    },
}

impl AttachmentInput {
    fn filename(&self) -> &str {
        match self {
            Self::Path(path) => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file"),
            Self::Clipboard { filename, .. } => filename,
        }
    }

    fn read(self) -> Result<(String, Vec<u8>), &'static str> {
        let (mime, bytes) = match self {
            Self::Path(path) => {
                let metadata = path.metadata().map_err(|_| "Could not read file.")?;
                if !metadata.is_file() {
                    return Err("Choose a regular file.");
                }
                check_size(metadata.len())?;
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                File::open(&path)
                    .and_then(|file| {
                        file.take(MAX_ATTACHMENT_BYTES as u64 + 1)
                            .read_to_end(&mut bytes)
                    })
                    .map_err(|_| "Could not read file.")?;
                let mime = mime_guess::from_path(&path).first_or_octet_stream();
                (mime.essence_str().to_owned(), bytes)
            }
            Self::Clipboard { mime, bytes, .. } => (mime.to_owned(), bytes),
        };
        check_size(bytes.len() as u64)?;
        Ok((mime, bytes))
    }
}

fn check_size(size: u64) -> Result<(), &'static str> {
    match size {
        0 => Err("This file is empty."),
        size if size > MAX_ATTACHMENT_BYTES as u64 => Err("Files can be 25 MB at most."),
        _ => Ok(()),
    }
}

#[derive(Clone)]
pub struct QueuedAttachment {
    pub key: Uuid,
    pub filename: String,
    pub status: UploadStatus,
    pub spoiler: bool,
    pub preview: Option<Arc<RenderImage>>,
}

#[derive(Clone)]
pub enum UploadStatus {
    Uploading(u8),
    Ready(String),
    Failed(&'static str),
}

pub(super) enum UploadUpdate {
    Progress(u8),
    Preview(Arc<RenderImage>),
    Finished(Result<String, &'static str>),
}

pub(in crate::session) struct UploadEvent {
    channel_id: String,
    key: Uuid,
    update: UploadUpdate,
}

pub(in crate::session) struct AttachmentWork {
    pub tasks: JoinSet<bool>,
    pub events: async_channel::Receiver<UploadEvent>,
    updates: async_channel::Sender<UploadEvent>,
    cancels: HashMap<Uuid, oneshot::Sender<()>>,
}

impl Default for AttachmentWork {
    fn default() -> Self {
        let (updates, events) = async_channel::unbounded();
        Self {
            tasks: JoinSet::new(),
            events,
            updates,
            cancels: HashMap::new(),
        }
    }
}

impl Draft {
    pub fn attachments_ready(&self) -> bool {
        self.uploads
            .iter()
            .any(|upload| matches!(upload.status, UploadStatus::Ready(_)))
    }

    pub fn attachments_block_send(&self) -> bool {
        self.uploads
            .iter()
            .any(|upload| matches!(upload.status, UploadStatus::Uploading(_)))
    }

    pub(super) fn attachment_refs(&self) -> Vec<AttachmentRef> {
        self.uploads
            .iter()
            .filter_map(|upload| match &upload.status {
                UploadStatus::Ready(id) => Some(AttachmentRef {
                    id: id.clone(),
                    spoiler: upload.spoiler,
                }),
                _ => None,
            })
            .collect()
    }
}

impl ChatSession {
    fn can_change_attachments(&self, channel_id: &str) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|view| view.channel_id == channel_id && view.can_send())
            && !self.outgoing.contains_key(channel_id)
    }

    pub fn add_attachments(
        &mut self,
        client: &SpkClient,
        channel_id: String,
        files: Vec<AttachmentInput>,
    ) {
        if files.is_empty() || !self.can_change_attachments(&channel_id) {
            return;
        }
        let draft = self.drafts.entry(channel_id.clone()).or_default();
        let slots = MAX_ATTACHMENTS - draft.uploads.len();
        draft.upload_notice = (files.len() > slots).then_some("At most 10 files per message.");
        for input in files.into_iter().take(slots) {
            let key = Uuid::new_v4();
            let filename = input.filename().to_owned();
            draft.uploads.push(QueuedAttachment {
                key,
                filename: filename.clone(),
                status: UploadStatus::Uploading(0),
                spoiler: false,
                preview: None,
            });
            let (cancel, cancelled) = oneshot::channel();
            self.attachments.cancels.insert(key, cancel);
            let updates = self.attachments.updates.clone();
            let channel_id = channel_id.clone();
            let client = client.clone();
            self.attachments.tasks.spawn(async move {
                let send_update = move |update| {
                    let _ = updates.try_send(UploadEvent {
                        channel_id: channel_id.clone(),
                        key,
                        update,
                    });
                };
                upload_attachment(client, input, filename, cancelled, send_update).await
            });
        }
    }

    pub fn cancel_attachment(&mut self, client: &SpkClient, channel_id: &str, key: Uuid) {
        if self.can_change_attachments(channel_id)
            && let Some(draft) = self.drafts.get_mut(channel_id)
            && let Some(index) = draft.uploads.iter().position(|upload| upload.key == key)
        {
            let upload = draft.uploads.remove(index);
            draft.upload_notice = None;
            self.attachments.cancel(client, upload);
        }
    }

    pub fn toggle_attachment_spoiler(&mut self, channel_id: &str, key: Uuid) {
        if self.can_change_attachments(channel_id)
            && let Some(draft) = self.drafts.get_mut(channel_id)
            && let Some(upload) = draft.uploads.iter_mut().find(|upload| upload.key == key)
        {
            upload.spoiler = !upload.spoiler;
        }
    }

    pub fn attachment_event(&mut self, client: &SpkClient, event: UploadEvent) {
        let UploadEvent {
            channel_id,
            key,
            update,
        } = event;
        if matches!(update, UploadUpdate::Finished(_)) {
            self.attachments.cancels.remove(&key);
        }
        let upload = self
            .drafts
            .get_mut(&channel_id)
            .and_then(|draft| draft.uploads.iter_mut().find(|upload| upload.key == key));
        match (upload, update) {
            (Some(upload), UploadUpdate::Preview(preview)) => upload.preview = Some(preview),
            (Some(upload), UploadUpdate::Progress(percent)) => {
                upload.status = UploadStatus::Uploading(percent)
            }
            (Some(upload), UploadUpdate::Finished(result)) => {
                upload.status = match result {
                    Ok(id) => UploadStatus::Ready(id),
                    Err(message) => UploadStatus::Failed(message),
                };
            }
            (None, UploadUpdate::Finished(Ok(id))) => self.attachments.cancel_remote(client, id),
            _ => {}
        }
    }

    pub fn read_attachment(
        &mut self,
        client: &SpkClient,
        channel_id: &str,
        message_id: &str,
        attachment_id: String,
        reply: oneshot::Sender<Result<Vec<u8>, Failure>>,
    ) {
        if !self.selected.as_ref().is_some_and(|view| {
            view.channel_id == channel_id
                && view.history == History::Ready
                && view.messages.iter().any(|message| {
                    message.id == message_id
                        && message
                            .attachments
                            .iter()
                            .any(|attachment| attachment.id == attachment_id)
                })
        }) {
            let _ = reply.send(Err(Failure::InvalidResponse));
            return;
        }
        let client = client.clone();
        self.attachments.tasks.spawn(async move {
            let result = client.attachment_bytes(&attachment_id).await;
            let expired = result.as_ref().is_err_and(Failure::unauthorized);
            let _ = reply.send(result);
            expired
        });
    }
}

impl AttachmentWork {
    pub(super) fn cancel(&mut self, client: &SpkClient, upload: QueuedAttachment) {
        if let Some(cancel) = self.cancels.remove(&upload.key) {
            let _ = cancel.send(());
        }
        if let UploadStatus::Ready(id) = upload.status {
            self.cancel_remote(client, id);
        }
    }

    fn cancel_remote(&mut self, client: &SpkClient, id: String) {
        let client = client.clone();
        self.tasks.spawn(async move {
            client
                .cancel_attachment(&id)
                .await
                .as_ref()
                .is_err_and(Failure::unauthorized)
        });
    }
}

async fn upload_attachment(
    client: SpkClient,
    input: AttachmentInput,
    filename: String,
    mut cancelled: oneshot::Receiver<()>,
    update: impl Fn(UploadUpdate) + Clone + Send + 'static,
) -> bool {
    let prepared = tokio::task::spawn_blocking(move || {
        let (mime, bytes) = input.read()?;
        let preview = mime
            .starts_with("image/")
            .then(|| image_preview(&bytes))
            .flatten();
        Ok::<_, &'static str>((mime, bytes, preview))
    });
    let (mime, bytes, preview) = match tokio::select! {
        result = prepared => result.unwrap_or(Err("Could not read file.")),
        _ = &mut cancelled => return false,
    } {
        Ok(prepared) => prepared,
        Err(message) => {
            update(UploadUpdate::Finished(Err(message)));
            return false;
        }
    };
    if let Some(preview) = preview {
        update(UploadUpdate::Preview(preview));
    }
    // Let a pending presign finish so a cancelled upload can still release its server row.
    let upload = match client
        .presign_attachment(&filename, &mime, bytes.len())
        .await
    {
        Ok(upload) => upload,
        Err(error) => {
            let message = match &error {
                Failure::Http(reqwest::StatusCode::TOO_MANY_REQUESTS) => {
                    "Too many uploads. Try again later."
                }
                Failure::Http(reqwest::StatusCode::PAYLOAD_TOO_LARGE) => {
                    "Files can be 25 MB at most."
                }
                _ => "Could not start the upload. Check that attachment storage is available.",
            };
            update(UploadUpdate::Finished(Err(message)));
            return error.unauthorized();
        }
    };
    let progress = update.clone();
    let mut last = 0;
    let result = tokio::select! {
        biased;
        _ = &mut cancelled => None,
        result = client.put_attachment(&upload, &mime, bytes, move |percent| {
            let percent = percent.min(99);
            if percent >= last + 5 {
                last = percent;
                progress(UploadUpdate::Progress(percent));
            }
        }) => Some(result),
    };
    if matches!(result, Some(Ok(()))) {
        update(UploadUpdate::Finished(Ok(upload.id)));
        return false;
    }
    let expired = client
        .cancel_attachment(&upload.id)
        .await
        .as_ref()
        .is_err_and(Failure::unauthorized);
    if result.is_some() {
        update(UploadUpdate::Finished(Err(
            "Upload failed. Remove the file or add it again.",
        )));
    }
    expired
}

// shortcut: previews are still raster thumbnails, add animation when the chat renderer supports it.
pub fn image_preview(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut preview = reader.decode().ok()?.thumbnail(768, 512).into_rgba8();
    for pixel in preview.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![Frame::new(preview)])))
}
