use super::*;

use std::io::Cursor;

use gpui_kit::{Image, ImageFormat, RenderImage, Size, SvgRenderer, size};
use image_webp::WebPDecoder;

use crate::{
    api::MessageTarget,
    chat_markdown::{image_preview_links, message_markdown},
};
use url::Url;

pub(in crate::session) type PreviewImage = Option<Arc<ChatImage>>;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(in crate::session) enum PreviewSource {
    Link,
    EmbedAvatar,
}

type PreviewKey = (String, String, PreviewSource);
pub(in crate::session) type PreviewResult = (PreviewKey, Result<Arc<RenderImage>, Failure>);

#[derive(Clone, Default)]
pub struct PreviewImages(Arc<HashMap<PreviewKey, Option<Arc<RenderImage>>>>);

impl PreviewImages {
    pub fn get(&self, message: &str, url: &str) -> Option<Arc<RenderImage>> {
        self.image(message, url, PreviewSource::Link)
    }

    pub fn avatar(&self, message: &str, url: &str) -> Option<Arc<RenderImage>> {
        self.image(message, url, PreviewSource::EmbedAvatar)
    }

    fn image(&self, message: &str, url: &str, source: PreviewSource) -> Option<Arc<RenderImage>> {
        self.0
            .get(&(message.to_owned(), url.to_owned(), source))?
            .clone()
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
            let avatar = view.webhook.then(|| webhook_avatar(message)).flatten();
            let previews = message
                .previews
                .iter()
                .filter(|card| card.image && card.link().is_some())
                .map(|card| (card.url.as_str(), PreviewSource::Link));
            let avatar = avatar
                .into_iter()
                .map(|url| (url, PreviewSource::EmbedAvatar));
            for (url, source) in avatar.chain(previews) {
                if self.preview_tasks.len() >= 4 {
                    return;
                }
                let key = (message.id.clone(), url.to_owned(), source);
                if view.preview_images.0.contains_key(&key) {
                    continue;
                }

                Arc::make_mut(&mut view.preview_images.0).insert(key.clone(), None);
                let client = client.clone();
                self.preview_tasks.spawn(async move {
                    let result = if source == PreviewSource::EmbedAvatar {
                        load_image(&client, &key.1, size(40, 40))
                            .await
                            .map(|image| image.pixels.clone())
                            .ok_or(Failure::InvalidResponse)
                    } else {
                        match client.preview_image(&key.0, &key.1).await {
                            Ok(bytes) => tokio::task::spawn_blocking(move || decode(bytes))
                                .await
                                .unwrap_or(Err(Failure::InvalidResponse)),
                            Err(error) => Err(error),
                        }
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
        Arc::make_mut(&mut view.preview_images.0).retain(|(id, url, source), _| {
            view.messages.iter().any(|message| {
                message.id == *id
                    && match source {
                        PreviewSource::EmbedAvatar => {
                            view.webhook && webhook_avatar(message) == Some(url)
                        }
                        PreviewSource::Link => message
                            .previews
                            .iter()
                            .any(|card| card.image && card.url == *url),
                    }
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
                image = load_image(&client, url.as_str(), size(800, 600)) => { let _ = reply.send(image); }
                _ = reply.closed() => {}
            }
        });
    }
}

fn webhook_avatar(message: &ChatMessage) -> Option<&str> {
    message.embed.as_ref()?.author.as_ref()?.avatar_url()
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

pub(super) async fn load_image(client: &SpkClient, url: &str, bounds: Size<u32>) -> PreviewImage {
    let bytes = client.public_image_preview(url).await.ok()?;
    tokio::task::spawn_blocking(move || {
        if matches!(
            image::guess_format(&bytes).ok()?,
            image::ImageFormat::Gif
                | image::ImageFormat::WebP
                | image::ImageFormat::Png
                | image::ImageFormat::Jpeg
        ) {
            return images::raster_thumbnail(&bytes, bounds);
        }
        None
    })
    .await
    .ok()
    .flatten()
}
