use super::*;

#[derive(Clone, Deserialize, Serialize)]
pub struct GifItem {
    pub id: String,
    pub title: String,
    pub url: String,
    pub preview: String,
    pub width: u16,
    pub height: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GifPage {
    pub items: Vec<GifItem>,
    pub has_next: bool,
}

impl GifItem {
    fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 128
            && self
                .id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
            && gif_media_url(&self.url).is_some()
            && gif_media_url(&self.preview).is_some()
    }
}

pub fn gif_media_url(raw: &str) -> Option<Url> {
    if raw.len() > 4096 || raw.chars().any(char::is_whitespace) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    // The loopback fixture serves both authenticated API requests and anonymous previews.
    #[cfg(test)]
    if url.scheme() == "http" && signed_storage_url(&url) {
        return Some(url);
    }
    public_media_url(&url).then_some(url)
}

pub fn gif_message_url(raw: &str) -> Option<Url> {
    let url = gif_media_url(raw)?;
    url.path()
        .to_ascii_lowercase()
        .ends_with(".gif")
        .then_some(url)
}

impl SpkClient {
    pub async fn gifs(&self, query: &str, page: u8) -> Result<GifPage, Failure> {
        let mut url = self.base.join("/api/gifs").expect("fixed SPK path");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("page", &page.to_string());
        let mut response: GifPage =
            Self::send(self.http.get(url).header(header::ORIGIN, &self.origin))
                .await?
                .json()
                .await
                .map_err(|_| Failure::InvalidResponse)?;
        response.items.retain(GifItem::valid);
        Ok(response)
    }

    pub async fn gif_favorites(&self) -> Result<Vec<GifItem>, Failure> {
        let mut favorites: Vec<GifItem> = self.get("/api/gifs/favorites").await?;
        favorites.retain(GifItem::valid);
        Ok(favorites)
    }

    pub async fn update_gif_favorite(&self, gif: GifItem, saved: bool) -> Result<GifItem, Failure> {
        // IDs accepted by GifItem::valid contain only unreserved path characters.
        let route = format!("/api/gifs/favorites/{}", gif.id);
        if saved {
            let response: GifItem = Self::send(self.request(Method::PUT, &route).json(&gif))
                .await?
                .json()
                .await
                .map_err(|_| Failure::InvalidResponse)?;
            return (response.id == gif.id && response.valid())
                .then_some(response)
                .ok_or(Failure::InvalidResponse);
        }
        Self::send(self.request(Method::DELETE, &route)).await?;
        Ok(gif)
    }

    pub async fn gif_preview(&self, raw: &str) -> Result<Vec<u8>, Failure> {
        let url = gif_media_url(raw).ok_or(Failure::InvalidResponse)?;
        Self::limited_bytes(self.public_media.get(url), 2 * 1024 * 1024).await
    }
}
