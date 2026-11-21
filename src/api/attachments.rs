use super::*;

pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 10;

#[derive(Clone, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
    pub spoiler: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttachmentRef {
    pub id: String,
    pub spoiler: bool,
}

#[derive(Deserialize)]
pub struct AttachmentUpload {
    pub id: String,
    url: String,
    headers: HashMap<String, String>,
}

impl SpkClient {
    pub async fn presign_attachment(
        &self,
        filename: &str,
        mime: &str,
        size: usize,
    ) -> Result<AttachmentUpload, Failure> {
        Self::send(
            self.request(Method::POST, "/api/attachments/presign")
                .json(&serde_json::json!({ "filename": filename, "mime": mime, "size": size })),
        )
        .await?
        .json()
        .await
        .map_err(|_| Failure::InvalidResponse)
    }

    pub async fn put_attachment(
        &self,
        upload: &AttachmentUpload,
        mime: &str,
        bytes: Vec<u8>,
        mut progress: impl FnMut(u8) + Send + 'static,
    ) -> Result<(), Failure> {
        let url = Url::parse(&upload.url)
            .ok()
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        if upload.headers.len() != 1
            || upload.headers.get("content-type").map(String::as_str) != Some(mime)
        {
            return Err(Failure::InvalidResponse);
        }
        let bytes = bytes::Bytes::from(bytes);
        let len = bytes.len();
        let parts = (0..len).step_by(64 * 1024).map(move |start| {
            let end = (start + 64 * 1024).min(len);
            progress((end * 100 / len) as u8);
            Ok::<_, std::io::Error>(bytes.slice(start..end))
        });
        // Storage can fail independently of the authenticated SPK session.
        Self::send(
            self.storage
                .put(url)
                .timeout(Duration::from_secs(300))
                .header(header::CONTENT_TYPE, mime)
                .header(header::CONTENT_LENGTH, len)
                .body(reqwest::Body::wrap_stream(futures::stream::iter(parts))),
        )
        .await
        .map_err(|_| Failure::Connection)?;
        Ok(())
    }

    pub async fn cancel_attachment(&self, id: &str) -> Result<(), Failure> {
        let mut url = self.attachment_endpoint(id)?;
        url.path_segments_mut()
            .expect("SPK URL has a host")
            .push("cancel");
        Self::send(self.http.post(url).header(header::ORIGIN, &self.origin)).await?;
        Ok(())
    }

    pub async fn attachment_bytes(&self, id: &str) -> Result<Vec<u8>, Failure> {
        #[derive(Deserialize)]
        struct SignedGet {
            url: String,
        }
        let mut url = self.attachment_endpoint(id)?;
        url.query_pairs_mut().append_pair("json", "1");
        let result = Self::send(self.http.get(url).header(header::ORIGIN, &self.origin))
            .await?
            .json::<SignedGet>()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        let url = Url::parse(&result.url)
            .ok()
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        Self::limited_bytes(
            self.storage.get(url).timeout(Duration::from_secs(300)),
            MAX_ATTACHMENT_BYTES,
        )
        .await
        .map_err(|_| Failure::Connection)
    }

    fn attachment_endpoint(&self, id: &str) -> Result<Url, Failure> {
        if matches!(id, "" | "." | "..") {
            return Err(Failure::InvalidResponse);
        }
        let mut url = self.base.join("/api/attachments/").expect("fixed SPK path");
        url.path_segments_mut()
            .expect("SPK URL has a host")
            .pop_if_empty()
            .push(id);
        Ok(url)
    }
}
