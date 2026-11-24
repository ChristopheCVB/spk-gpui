use tempfile::NamedTempFile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::*;

pub const MAX_ATTACHMENT_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_ATTACHMENT_PREVIEW_BYTES: usize = 25 * 1024 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3600);
const TRANSFER_CHUNK_BYTES: u64 = 64 * 1024;
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
        size: u64,
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
        size: u64,
        input: impl AsyncRead + Unpin + Send + 'static,
        progress: impl FnMut(u8) + Send + 'static,
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
        let parts = futures::stream::try_unfold(
            (input, size, progress),
            move |(mut input, remaining, mut progress)| async move {
                if remaining == 0 {
                    return Ok(None);
                }
                let length = remaining.min(TRANSFER_CHUNK_BYTES);
                let mut chunk = vec![0; length as usize];
                input.read_exact(&mut chunk).await?;
                let remaining = remaining - length;
                progress(((size - remaining) * 100 / size) as u8);
                Ok::<_, std::io::Error>(Some((chunk, (input, remaining, progress))))
            },
        );
        // Storage can fail independently of the authenticated SPK session.
        Self::send(
            self.storage
                .put(url)
                .timeout(TRANSFER_TIMEOUT)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CONTENT_LENGTH, size)
                .body(reqwest::Body::wrap_stream(parts)),
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
        let url = self.attachment_url(id).await?;
        Self::limited_bytes(
            self.storage.get(url).timeout(Duration::from_secs(300)),
            MAX_ATTACHMENT_PREVIEW_BYTES,
        )
        .await
        .map_err(|_| Failure::Connection)
    }

    pub async fn download_attachment(
        &self,
        id: &str,
        size: u64,
        destination: &NamedTempFile,
    ) -> Result<(), Failure> {
        if size == 0 || size > MAX_ATTACHMENT_BYTES {
            return Err(Failure::InvalidResponse);
        }
        let url = self.attachment_url(id).await?;
        let mut response = Self::send(self.storage.get(url).timeout(TRANSFER_TIMEOUT))
            .await
            .map_err(|_| Failure::Connection)?;
        if response
            .content_length()
            .is_some_and(|length| length != size)
        {
            return Err(Failure::InvalidResponse);
        }
        let mut output =
            tokio::fs::File::from_std(destination.reopen().map_err(|_| Failure::Connection)?);
        let mut remaining = size;
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Connection)? {
            remaining = remaining
                .checked_sub(chunk.len() as u64)
                .ok_or(Failure::InvalidResponse)?;
            output
                .write_all(&chunk)
                .await
                .map_err(|_| Failure::Connection)?;
        }
        if remaining != 0 {
            return Err(Failure::InvalidResponse);
        }
        output.flush().await.map_err(|_| Failure::Connection)
    }

    async fn attachment_url(&self, id: &str) -> Result<Url, Failure> {
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
        Url::parse(&result.url)
            .ok()
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)
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
