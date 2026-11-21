use super::*;
use uuid::Uuid;

const SOUND_MIME: &str = "audio/wav";
pub const MAX_SOUND_BYTES: usize = 3 * 1024 * 1024;

impl SpkClient {
    pub async fn add_soundboard_clip(
        &self,
        name: &str,
        icon: &str,
        duration_ms: u32,
        wav: Vec<u8>,
    ) -> Result<(), Failure> {
        #[derive(Deserialize)]
        struct Upload {
            id: Uuid,
            url: String,
            headers: HashMap<String, String>,
        }

        let size = wav.len();
        let upload: Upload = Self::send(
            self.request(Method::POST, "/api/sounds/presign")
                .json(&serde_json::json!({"size": size})),
        )
        .await?
        .json()
        .await
        .map_err(|_| Failure::InvalidResponse)?;
        let url = Url::parse(&upload.url)
            .ok()
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        if upload.headers.len() != 1
            || upload.headers.get("content-type").map(String::as_str) != Some(SOUND_MIME)
        {
            return Err(Failure::InvalidResponse);
        }
        Self::send(
            self.storage
                .put(url)
                .header(header::CONTENT_TYPE, SOUND_MIME)
                .body(wav),
        )
        .await
        .map_err(|_| Failure::Connection)?;
        Self::send(
            self.request(Method::POST, "/api/sounds")
                .json(&serde_json::json!({
                    "id": upload.id,
                    "name": name,
                    "icon": icon,
                    "durationMs": duration_ms,
                    "size": size,
                })),
        )
        .await?;
        Ok(())
    }

    pub async fn edit_soundboard_clip(
        &self,
        id: Uuid,
        name: &str,
        icon: &str,
    ) -> Result<(), Failure> {
        Self::send(
            self.request(Method::PATCH, &format!("/api/sounds/{id}"))
                .json(&serde_json::json!({"name": name, "icon": icon})),
        )
        .await?;
        Ok(())
    }

    pub async fn delete_soundboard_clip(&self, id: Uuid) -> Result<(), Failure> {
        Self::send(self.request(Method::DELETE, &format!("/api/sounds/{id}"))).await?;
        Ok(())
    }
}
