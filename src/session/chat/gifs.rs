use super::*;

use gpui_kit::RenderImage;
use reqwest::StatusCode;

use crate::{
    api::{GifItem, GifPage},
    chat_markdown::{image_preview_links, message_markdown},
};

#[derive(Clone)]
pub struct StagedGif {
    pub item: GifItem,
    pub image: Option<Arc<RenderImage>>,
}

#[derive(Clone, Default)]
pub struct GifPickerView {
    pub query: String,
    pub showing_favorites: bool,
    pub items: Arc<Vec<GifItem>>,
    pub favorites: Arc<Vec<GifItem>>,
    pub has_next: bool,
    pub browse_loading: bool,
    pub browse_error: Option<&'static str>,
    pub favorites_loading: bool,
    pub favorites_ready: bool,
    pub favorites_error: Option<&'static str>,
    pub saving: bool,
    pub mutation_error: Option<&'static str>,
    images: Arc<HashMap<String, Option<Arc<RenderImage>>>>,
}

impl GifPickerView {
    pub fn is_favorite(&self, id: &str) -> bool {
        self.favorites.iter().any(|gif| gif.id == id)
    }

    pub fn visible(&self) -> impl Iterator<Item = &GifItem> {
        let query = self.query.to_lowercase();
        let items = if self.showing_favorites {
            &self.favorites
        } else {
            &self.items
        };
        items
            .iter()
            .filter(move |gif| !self.showing_favorites || gif.title.to_lowercase().contains(&query))
    }

    pub fn image(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.images.get(url)?.clone()
    }
}

pub(in crate::session) enum FavoriteResult {
    List(Vec<GifItem>),
    Changed { gif: GifItem, saved: bool },
}

pub(in crate::session) type PreviewImage = Option<Arc<RenderImage>>;
pub(in crate::session) type MessageImageResult = (String, String, PreviewImage);

#[derive(Clone)]
pub struct MessageImage {
    pub message_id: String,
    pub url: String,
    pub image: Option<Arc<RenderImage>>,
    pub loading: bool,
}

#[derive(Default)]
pub(in crate::session) struct GifPicker {
    pub view: Option<GifPickerView>,
    page: u8,
    requested_page: u8,
    updating_favorite: bool,
    pub browsing: JoinSet<Result<GifPage, Failure>>,
    pub favorites: JoinSet<Result<FavoriteResult, Failure>>,
    pub previews: JoinSet<(String, PreviewImage)>,
    pub message_preview: JoinSet<MessageImageResult>,
}

impl GifPicker {
    pub fn show(&mut self, client: &SpkClient) {
        if self.view.is_none() {
            self.view = Some(GifPickerView::default());
            self.browse(client, 1, false);
            self.load_favorites(client);
        }
    }

    pub fn close(&mut self) {
        self.view = None;
        self.browsing = JoinSet::new();
        self.previews = JoinSet::new();
    }

    pub fn reset_conversation(&mut self) {
        self.close();
        self.message_preview = JoinSet::new();
    }

    pub fn show_favorites(&mut self, client: &SpkClient, show: bool) {
        let Some(view) = &mut self.view else {
            return;
        };
        if view.showing_favorites == show {
            return;
        }
        view.showing_favorites = show;
        self.browsing = JoinSet::new();
        self.previews = JoinSet::new();
        view.images = Arc::default();
        view.browse_loading = false;
        if !show {
            self.browse(client, 1, false);
        }
    }

    pub fn search(&mut self, client: &SpkClient, query: String) {
        let Some(view) = &mut self.view else {
            return;
        };
        let query: String = query.trim().chars().take(100).collect();
        if view.query == query {
            return;
        }
        view.query = query;
        if !view.showing_favorites {
            self.browse(client, 1, true);
        }
    }

    pub fn more(&mut self, client: &SpkClient) {
        if self
            .view
            .as_ref()
            .is_some_and(|view| view.has_next && !view.browse_loading)
        {
            self.browse(client, self.page + 1, false);
        }
    }

    pub fn retry_browse(&mut self, client: &SpkClient) {
        if self
            .view
            .as_ref()
            .is_some_and(|view| view.browse_error.is_some())
        {
            self.browse(client, self.requested_page, false);
        }
    }

    fn browse(&mut self, client: &SpkClient, page: u8, debounce: bool) {
        let Some(view) = self.view.as_mut().filter(|view| !view.showing_favorites) else {
            return;
        };
        self.browsing = JoinSet::new();
        if page == 1 {
            self.previews = JoinSet::new();
            view.items = Arc::default();
            view.images = Arc::default();
            view.has_next = false;
        }
        self.requested_page = page;
        view.browse_loading = true;
        view.browse_error = None;
        let query = view.query.clone();
        let client = client.clone();
        self.browsing.spawn(async move {
            if debounce {
                tokio::time::sleep(Duration::from_millis(350)).await;
            }
            client.gifs(&query, page).await
        });
    }

    pub fn browse_complete(&mut self, result: Result<GifPage, Failure>) {
        let Some(view) = &mut self.view else {
            return;
        };
        view.browse_loading = false;
        match result {
            Ok(page) => {
                self.page = self.requested_page;
                view.has_next = page.has_next && self.page < 50;
                let current = Arc::make_mut(&mut view.items);
                for gif in page.items {
                    if !current.iter().any(|existing| existing.id == gif.id) {
                        current.push(gif);
                    }
                }
            }
            Err(error) => {
                view.browse_error = Some(match error {
                    Failure::Http(StatusCode::SERVICE_UNAVAILABLE) => "GIFs are not set up",
                    Failure::Http(StatusCode::BAD_GATEWAY) => "GIF service unavailable",
                    Failure::Http(StatusCode::TOO_MANY_REQUESTS) => "Slow down",
                    _ => "Could not load GIFs",
                });
            }
        }
    }

    pub fn load_favorites(&mut self, client: &SpkClient) {
        let Some(view) = &mut self.view else {
            return;
        };
        if !self.favorites.is_empty() {
            view.favorites_loading = !view.favorites_ready;
            return;
        }
        view.favorites_loading = true;
        view.favorites_ready = false;
        view.favorites_error = None;
        let client = client.clone();
        self.favorites
            .spawn(async move { client.gif_favorites().await.map(FavoriteResult::List) });
    }

    pub fn toggle_favorite(&mut self, client: &SpkClient, id: &str) {
        let Some(view) = self
            .view
            .as_mut()
            .filter(|view| view.favorites_ready && !view.saving)
        else {
            return;
        };
        let Some(gif) = view.visible().find(|gif| gif.id == id).cloned() else {
            return;
        };
        let saved = !view.is_favorite(id);
        view.mutation_error = None;
        view.saving = true;
        self.updating_favorite = true;
        let client = client.clone();
        self.favorites.spawn(async move {
            let gif = client.update_gif_favorite(gif, saved).await?;
            Ok(FavoriteResult::Changed { gif, saved })
        });
    }

    pub fn favorites_complete(
        &mut self,
        client: &SpkClient,
        result: Result<FavoriteResult, Failure>,
    ) {
        let updating = std::mem::take(&mut self.updating_favorite);
        let Some(view) = &mut self.view else {
            return;
        };
        view.favorites_loading = false;
        if updating && !view.saving {
            // A previous picker's write finished while the new picker was opening.
            self.load_favorites(client);
            return;
        }
        if let Ok(FavoriteResult::List(favorites)) = result {
            view.favorites = Arc::new(favorites);
            view.favorites_ready = true;
        } else if view.saving {
            view.saving = false;
            match result {
                Ok(FavoriteResult::Changed { gif, saved }) => {
                    let items = Arc::make_mut(&mut view.favorites);
                    if saved {
                        items.push(gif);
                        items.sort_by(|a, b| (&a.title, &a.id).cmp(&(&b.title, &b.id)));
                    } else {
                        items.retain(|item| item.id != gif.id);
                    }
                }
                Err(error) => {
                    view.mutation_error = Some(match error {
                        Failure::Http(StatusCode::CONFLICT) => {
                            "You can save up to 50 GIF favorites."
                        }
                        Failure::Http(StatusCode::TOO_MANY_REQUESTS) => {
                            "Too many favorites requests. Try again in a minute."
                        }
                        _ => "Could not update favorites",
                    });
                }
                _ => {}
            }
        } else {
            view.favorites_error = Some("Could not load favorites");
        }
    }

    pub fn pick(&mut self, id: &str) -> Option<StagedGif> {
        let view = self.view.as_ref()?;
        let item = view.visible().find(|gif| gif.id == id)?.clone();
        let image = view.image(&item.preview);
        self.close();
        Some(StagedGif { item, image })
    }

    pub fn fill_previews(&mut self, client: &SpkClient) {
        let Some(view) = &mut self.view else {
            return;
        };
        let capacity = 4usize.saturating_sub(self.previews.len());
        if capacity == 0 {
            return;
        }
        let pending: Vec<_> = view
            .visible()
            .map(|gif| &gif.preview)
            .filter(|url| !view.images.contains_key(*url))
            .take(capacity)
            .cloned()
            .collect();
        for url in pending {
            if Arc::make_mut(&mut view.images)
                .insert(url.clone(), None)
                .is_some()
            {
                continue;
            }
            let client = client.clone();
            self.previews.spawn(async move {
                let image = load_image(&client, &url, 160, 100).await;
                (url, image)
            });
        }
    }

    pub fn preview_complete(&mut self, (url, image): (String, PreviewImage)) {
        if let Some(view) = &mut self.view {
            Arc::make_mut(&mut view.images).insert(url, image);
        }
    }
}

impl ChatSession {
    pub fn open_gifs(&mut self, client: &SpkClient) {
        if self
            .selected
            .as_ref()
            .is_some_and(|view| view.can_send() && !self.outgoing.contains_key(&view.channel_id))
        {
            self.gifs.show(client);
        }
    }

    pub fn set_gif(&mut self, id: Option<&str>, channel: Option<&str>) {
        let Some(view) = self.selected.as_ref().filter(|view| {
            view.can_send() && channel.is_none_or(|channel| view.channel_id == channel)
        }) else {
            return;
        };
        if self.outgoing.contains_key(&view.channel_id) {
            return;
        }
        let gif = if let Some(id) = id {
            let Some(gif) = self.gifs.pick(id) else {
                return;
            };
            Some(gif)
        } else {
            None
        };
        self.drafts.entry(view.channel_id.clone()).or_default().gif = gif;
    }

    pub fn preview_message_image(&mut self, client: &SpkClient, message_id: String, url: String) {
        let Some(view) = self
            .selected
            .as_mut()
            .filter(|view| view.history == History::Ready)
        else {
            return;
        };
        let Some(message) = view
            .messages
            .iter()
            .find(|message| message.id == message_id)
        else {
            return;
        };
        let rendered = message_markdown(&message.body, |_| "", |_| true, Some("spk-spoiler:"));
        if !image_preview_links(&rendered)
            .iter()
            .any(|link| link.as_str() == url)
        {
            return;
        }
        view.message_image = Some(MessageImage {
            message_id: message_id.clone(),
            url: url.clone(),
            image: None,
            loading: true,
        });
        self.gifs.message_preview = JoinSet::new();
        let client = client.clone();
        self.gifs.message_preview.spawn(async move {
            let image = load_image(&client, &url, 480, 300).await;
            (message_id, url, image)
        });
    }

    pub fn message_image_complete(&mut self, (id, url, image): MessageImageResult) {
        if let Some(preview) = self
            .selected
            .as_mut()
            .and_then(|view| view.message_image.as_mut())
            && preview.message_id == id
            && preview.url == url
        {
            preview.loading = false;
            preview.image = image;
        }
    }
}

async fn load_image(client: &SpkClient, url: &str, width: u32, height: u32) -> PreviewImage {
    let bytes = client.public_image_preview(url).await.ok()?;
    tokio::task::spawn_blocking(move || {
        if matches!(
            image::guess_format(&bytes).ok()?,
            image::ImageFormat::Gif
                | image::ImageFormat::WebP
                | image::ImageFormat::Png
                | image::ImageFormat::Jpeg
        ) {
            return attachments::raster_thumbnail(&bytes, width, height);
        }
        None
    })
    .await
    .ok()
    .flatten()
}
