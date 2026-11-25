use super::*;

use std::io::Cursor;

use gpui_kit::{Image, ImageFormat, RenderImage, SvgRenderer};
use image_webp::WebPDecoder;

use crate::{
    api::MessageTarget,
    chat_markdown::{image_preview_links, message_markdown},
};
use url::Url;

pub(in crate::session) type PreviewImage = Option<Arc<ChatImage>>;

type PreviewKey = (String, String);
pub(in crate::session) type PreviewResult = (PreviewKey, Result<Arc<RenderImage>, Failure>);

#[derive(Clone, Default)]
pub struct PreviewImages(Arc<HashMap<PreviewKey, Option<Arc<RenderImage>>>>);

impl PreviewImages {
    pub fn get(&self, message: &str, url: &str) -> Option<Arc<RenderImage>> {
        self.0.get(&(message.to_owned(), url.to_owned()))?.clone()
    }

    pub fn images(&self) -> impl Iterator<Item = &Arc<RenderImage>> {
        self.0.values().filter_map(Option::as_ref)
    }
}

impl ChatSession {
    pub fn fill_previews(&mut self, client: &SpkClient) {
        let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.history == History::Ready)
        else {
            return;
        };
        for message in view.messages.iter().rev() {
            for preview in message.previews.iter().filter(|card| card.image) {
                if self.preview_tasks.len() >= 4 {
                    return;
                }
                if preview.link().is_none() {
                    continue;
                }
                let key = (message.id.clone(), preview.url.clone());
                if view.preview_images.0.contains_key(&key) {
                    continue;
                }

                Arc::make_mut(&mut view.preview_images.0).insert(key.clone(), None);
                let client = client.clone();
                self.preview_tasks.spawn(async move {
                    let result = match client.preview_image(&key.0, &key.1).await {
                        Ok(bytes) => tokio::task::spawn_blocking(move || decode(bytes))
                            .await
                            .unwrap_or(Err(Failure::InvalidResponse)),
                        Err(error) => Err(error),
                    };
                    (key, result)
                });
            }
        }
    }

    pub fn preview_complete(&mut self, (key, result): PreviewResult) {
        if let (Some(view), Ok(image)) = (&mut self.selected, result)
            && let Some(previous) = Arc::make_mut(&mut view.preview_images.0).get_mut(&key)
        {
            *previous = Some(image);
        }
    }

    pub(super) fn prune_previews(&mut self) {
        let Some(view) = self.selected.as_mut() else {
            return;
        };
        if view.preview_images.0.is_empty() {
            return;
        }
        Arc::make_mut(&mut view.preview_images.0).retain(|(id, url), _| {
            view.messages.iter().any(|message| {
                message.id == *id
                    && message
                        .previews
                        .iter()
                        .any(|card| card.image && card.url == *url)
            })
        });
    }
    pub fn read_message_image(
        &mut self,
        client: &SpkClient,
        target: &MessageTarget,
        url: Url,
        body: &str,
        mut reply: tokio::sync::oneshot::Sender<PreviewImage>,
    ) {
        let Some(message) = self
            .selected
            .as_ref()
            .filter(|view| view.channel_id == target.channel_id() && view.history_ready())
            .and_then(|view| {
                view.messages
                    .iter()
                    .find(|message| message.id == target.message_id() && message.body == body)
            })
        else {
            return;
        };
        let rendered = message_markdown(&message.body, |_| "", |_| true, Some("spk-spoiler:"));
        if !image_preview_links(&rendered).contains(&url) {
            return;
        }

        let client = client.clone();
        self.message_preview.spawn(async move {
            tokio::select! {
                image = load_image(&client, url.as_str(), 800, 600) => { let _ = reply.send(image); }
                _ = reply.closed() => {}
            }
        });
    }
}

fn decode(bytes: Vec<u8>) -> Result<Arc<RenderImage>, Failure> {
    let header = WebPDecoder::new(Cursor::new(&bytes)).map_err(|_| Failure::InvalidResponse)?;
    let (width, height) = header.dimensions();
    if u64::from(width) * u64::from(height) > 2_000_000 || header.num_frames() > 1 {
        return Err(Failure::InvalidResponse);
    }
    Image::from_bytes(ImageFormat::Webp, bytes)
        .to_image_data(SvgRenderer::new(Arc::new(())))
        .map_err(|_| Failure::InvalidResponse)
}

pub(super) async fn load_image(
    client: &SpkClient,
    url: &str,
    width: u32,
    height: u32,
) -> PreviewImage {
    let bytes = client.public_image_preview(url).await.ok()?;
    tokio::task::spawn_blocking(move || {
        if matches!(
            image::guess_format(&bytes).ok()?,
            image::ImageFormat::Gif
                | image::ImageFormat::WebP
                | image::ImageFormat::Png
                | image::ImageFormat::Jpeg
        ) {
            return images::raster_thumbnail(&bytes, width, height);
        }
        None
    })
    .await
    .ok()
    .flatten()
}
