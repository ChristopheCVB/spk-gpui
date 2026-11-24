use super::*;

#[derive(Serialize)]
pub struct WebhookSettings {
    pub name: String,
    pub icon: String,
    pub private: bool,
}

impl WebhookSettings {
    pub fn validated(mut self) -> Result<Self, &'static str> {
        self.name.retain(|ch| !ch.is_ascii_control());
        // Match JavaScript trim, including BOM and excluding the Unicode NEXT LINE character.
        self.name = self
            .name
            .trim_matches(|ch: char| ch == '\u{feff}' || (ch.is_whitespace() && ch != '\u{85}'))
            .to_owned();
        if !(1..=32).contains(&self.name.encode_utf16().count()) {
            return Err("Webhook names must be 1–32 characters.");
        }
        if !(1..=64).contains(&self.icon.len()) || !has_icon_name_characters(&self.icon) {
            return Err("Choose a valid webhook icon.");
        }
        Ok(self)
    }
}

pub struct WebhookUrl(String);

impl WebhookUrl {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(super) fn parse(raw: String) -> Result<Self, Failure> {
        let url = Url::parse(&raw).map_err(|_| Failure::InvalidResponse)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || raw
                .chars()
                .any(|ch| ch.is_ascii_control() || ch.is_whitespace())
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(Self(raw))
    }
}

impl SpkClient {
    pub async fn create_webhook(
        &self,
        settings: WebhookSettings,
    ) -> Result<(Channel, WebhookUrl), Failure> {
        #[derive(Deserialize)]
        struct CreatedWebhook {
            channel: Channel,
            url: String,
        }

        let result: CreatedWebhook =
            Self::send(self.request(Method::POST, "/api/webhooks").json(&settings))
                .await?
                .json()
                .await
                .map_err(|_| Failure::InvalidResponse)?;

        if !result.channel.is_webhook() || result.channel.id.is_empty() {
            return Err(Failure::InvalidResponse);
        }
        Ok((result.channel, WebhookUrl::parse(result.url)?))
    }

    pub async fn edit_webhook(
        &self,
        id: &str,
        settings: WebhookSettings,
    ) -> Result<Channel, Failure> {
        let feed: Channel = Self::send(
            self.webhook_request(Method::PATCH, id, false)?
                .json(&settings),
        )
        .await?
        .json()
        .await
        .map_err(|_| Failure::InvalidResponse)?;
        if !feed.is_webhook() || feed.id != id {
            return Err(Failure::InvalidResponse);
        }
        Ok(feed)
    }

    pub async fn rotate_webhook(&self, id: &str) -> Result<WebhookUrl, Failure> {
        #[derive(Deserialize)]
        struct RotatedWebhook {
            url: String,
        }

        let result: RotatedWebhook = Self::send(self.webhook_request(Method::POST, id, true)?)
            .await?
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        WebhookUrl::parse(result.url)
    }

    pub async fn delete_webhook(&self, id: &str) -> Result<(), Failure> {
        #[derive(Deserialize)]
        struct DeletedWebhook {
            ok: bool,
        }

        let result: DeletedWebhook = Self::send(self.webhook_request(Method::DELETE, id, false)?)
            .await?
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        result.ok.then_some(()).ok_or(Failure::InvalidResponse)
    }

    fn webhook_request(
        &self,
        method: Method,
        id: &str,
        secret: bool,
    ) -> Result<reqwest::RequestBuilder, Failure> {
        if matches!(id, "" | "." | "..") {
            return Err(Failure::InvalidResponse);
        }
        let mut url = self
            .base
            .join("/api/webhooks/")
            .expect("fixed SPK endpoint path");
        {
            let mut segments = url.path_segments_mut().expect("SPK URL has a host");
            segments.pop_if_empty().push(id);
            if secret {
                segments.push("secret");
            }
        }
        Ok(self
            .http
            .request(method, url)
            .header(header::ORIGIN, &self.origin))
    }
}
