use super::{
    preview_images::{PreviewImage, load_image},
    *,
};

use gpui_kit::Size;
use reqwest::StatusCode;

use crate::api::{GifItem, GifPage};

const PICKER_PREVIEW_SIZE: Size<u32> = Size {
    width: 160,
    height: 100,
};
// The viewport can show part of three rows of two thumbnails.
const MAX_PICKER_PREVIEW_BYTES: usize = 192 * 1024 * 1024;

pub(in crate::session) type StagedGifPreview = (String, String, PreviewImage);

#[derive(Clone)]
pub struct StagedGif {
    pub item: GifItem,
    pub image: Option<Arc<ChatImage>>,
    preview_attempted: bool,
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
    pub preview_urls: Vec<String>,
    images: Arc<HashMap<String, Option<Arc<ChatImage>>>>,
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

    pub fn image(&self, url: &str) -> Option<Arc<ChatImage>> {
        self.images.get(url)?.clone()
    }

    pub fn images(&self) -> impl Iterator<Item = &Arc<ChatImage>> {
        self.images.values().filter_map(Option::as_ref)
    }
}

pub(in crate::session) enum FavoriteResult {
    List(Vec<GifItem>),
    Changed { gif: GifItem, saved: bool },
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
    loading_previews: HashSet<String>,
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
        self.loading_previews.clear();
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
        self.loading_previews.clear();
        view.images = Arc::default();
        view.preview_urls.clear();
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
            self.loading_previews.clear();
            view.items = Arc::default();
            view.images = Arc::default();
            view.preview_urls.clear();
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
    ) -> Option<ActionResult> {
        let updating = std::mem::take(&mut self.updating_favorite);
        let action_result = if updating {
            result.as_ref().err().map(|error| {
                ActionResult::error(match error {
                    Failure::Http(StatusCode::CONFLICT) => "You can save up to 50 GIF favorites.",
                    Failure::Http(StatusCode::TOO_MANY_REQUESTS) => {
                        "Too many favorites requests. Try again in a minute."
                    }
                    _ => "Could not update favorites",
                })
            })
        } else {
            None
        };
        let Some(view) = &mut self.view else {
            return action_result;
        };
        view.favorites_loading = false;
        if updating && !view.saving {
            // A previous picker's write finished while the new picker was opening.
            self.load_favorites(client);
            return action_result;
        }
        if let Ok(FavoriteResult::List(favorites)) = result {
            view.favorites = Arc::new(favorites);
            view.favorites_ready = true;
        } else if view.saving {
            view.saving = false;
            if let Ok(FavoriteResult::Changed { gif, saved }) = result {
                let items = Arc::make_mut(&mut view.favorites);
                if saved {
                    items.push(gif);
                    items.sort_by(|a, b| (&a.title, &a.id).cmp(&(&b.title, &b.id)));
                } else {
                    items.retain(|item| item.id != gif.id);
                }
            }
        } else {
            view.favorites_error = Some("Could not load favorites");
        }
        action_result
    }

    pub fn pick(&mut self, id: &str) -> Option<StagedGif> {
        let view = self.view.as_ref()?;
        let item = view.visible().find(|gif| gif.id == id)?.clone();
        let image = view.image(&item.preview);
        self.close();
        Some(StagedGif {
            item,
            image,
            preview_attempted: false,
        })
    }

    pub fn show_previews(&mut self, mut urls: Vec<String>) {
        let Some(view) = &mut self.view else {
            return;
        };
        let eligible: HashSet<_> = view.visible().map(|gif| gif.preview.as_str()).collect();
        urls.retain(|url| eligible.contains(url.as_str()));
        if view.preview_urls == urls {
            return;
        }
        let visible: HashSet<_> = urls.iter().collect();
        Arc::make_mut(&mut view.images).retain(|url, _| visible.contains(url));
        view.preview_urls = urls;
    }

    pub fn fill_previews(&mut self, client: &SpkClient) {
        let Some(view) = &self.view else {
            return;
        };
        for url in &view.preview_urls {
            if self.previews.len() >= 4 {
                break;
            }
            if view.images.contains_key(url) || !self.loading_previews.insert(url.clone()) {
                continue;
            }
            let url = url.clone();
            let client = client.clone();
            self.previews.spawn(async move {
                let image = load_image(&client, &url, PICKER_PREVIEW_SIZE).await;
                (url, image)
            });
        }
    }

    pub fn preview_complete(&mut self, (url, image): (String, PreviewImage)) {
        self.loading_previews.remove(&url);
        let Some(view) = &mut self.view else {
            return;
        };
        if view.preview_urls.contains(&url) {
            let retained: usize = view.images().map(|image| image.pixel_bytes()).sum();
            let image =
                image.filter(|image| retained + image.pixel_bytes() <= MAX_PICKER_PREVIEW_BYTES);
            Arc::make_mut(&mut view.images).insert(url, image);
        }
    }
}

impl ChatSession {
    pub fn open_gifs(&mut self, client: &SpkClient) {
        if self.selected.as_ref().is_some_and(ChatView::can_send) {
            self.gifs.show(client);
        }
    }

    pub fn fill_staged_gif_preview(&mut self, client: &SpkClient) {
        let requested = self.selected.as_ref().and_then(|view| {
            let gif = self.drafts.get(&view.channel_id)?.gif.as_ref()?;
            (gif.image.is_none() && !gif.preview_attempted)
                .then(|| (view.channel_id.clone(), gif.item.preview.clone()))
        });
        if requested == self.pending_staged_gif_preview {
            return;
        }
        self.staged_gif_preview = JoinSet::new();
        self.pending_staged_gif_preview = requested.clone();
        if let Some((channel, url)) = requested {
            let client = client.clone();
            self.staged_gif_preview.spawn(async move {
                let image = load_image(&client, &url, PICKER_PREVIEW_SIZE).await;
                (channel, url, image)
            });
        }
    }

    pub fn staged_gif_preview_complete(&mut self, (channel, url, image): StagedGifPreview) {
        self.pending_staged_gif_preview = None;
        if let Some(gif) = self
            .drafts
            .get_mut(&channel)
            .and_then(|draft| draft.gif.as_mut())
            && gif.item.preview == url
        {
            gif.image = image;
            gif.preview_attempted = true;
        }
    }

    pub fn set_gif(&mut self, id: Option<&str>, channel: Option<&str>) {
        let Some(view) = self.selected.as_ref().filter(|view| {
            view.can_send() && channel.is_none_or(|channel| view.channel_id == channel)
        }) else {
            return;
        };
        let gif = if let Some(id) = id {
            let Some(gif) = self.gifs.pick(id) else {
                return;
            };
            Some(gif)
        } else {
            None
        };
        let channel = view.channel_id.clone();
        self.shelve_draft_restoration(&channel);
        self.drafts.entry(channel).or_default().gif = gif;
    }
}
