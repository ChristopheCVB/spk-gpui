mod account_admin;
mod attachments;
mod channel_admin;
mod chat_actions;
mod gifs;
mod public_media;
mod search;
mod soundboard_admin;
mod webhooks;

pub(crate) use account_admin::account_link_token;
pub use account_admin::{
    AccountRole, AdminDirectory, AdminUser, Invite, InviteStatus, IssuedAccountLink,
};
pub use attachments::{Attachment, AttachmentRef, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS};
pub use channel_admin::{ChannelKind, ChannelMutation};
pub use chat_actions::{LinkPreview, MessageAction, MessageChange, Reaction};
pub use gifs::{GifItem, GifPage, image_message_url};
pub(crate) use public_media::{PublicDns, public_media_url};
pub use search::{
    MessageTarget, SearchHit, SearchPage, SearchScope, conversation_link, conversation_permalink,
};
pub use soundboard_admin::MAX_SOUND_BYTES;
pub use webhooks::{WebhookSettings, WebhookUrl};

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::{SinkExt, StreamExt};
use reqwest::{Client, Method, StatusCode, cookie::CookieStore as _, header, redirect::Policy};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    sync::oneshot,
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message, client::IntoClientRequest},
};
use url::{Host, Url};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_AVATAR_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_AVATAR_UPLOAD: usize = 5 * 1024 * 1024;

fn signed_storage_url(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    match (url.scheme(), url.host()) {
        ("https", Some(_)) => true,
        ("http", Some(host)) if cfg!(debug_assertions) => match host {
            Host::Domain(name) => name == "localhost",
            Host::Ipv4(ip) => ip.is_loopback(),
            Host::Ipv6(ip) => ip.is_loopback(),
        },
        _ => false,
    }
}

#[derive(Debug)]
pub enum Failure {
    Http(StatusCode),
    Connection,
    InvalidResponse,
}

impl Failure {
    pub fn unauthorized(&self) -> bool {
        matches!(self, Self::Http(StatusCode::UNAUTHORIZED))
    }
}

#[derive(Deserialize)]
pub struct AccountLinkCheck {
    pub valid: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: Option<String>,
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_key: Option<String>,
    #[serde(default)]
    pub role: String,
}

impl Profile {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.email)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogPage {
    pub commits: Vec<ChangelogEntry>,
    pub head: Option<String>,
    pub next_page: Option<u32>,
}

#[derive(Deserialize)]
pub struct ChangelogEntry {
    pub sha: String,
    pub message: String,
    pub date: String,
}

fn commit_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    pub other_user_id: Option<String>,
    pub last_activity_at: Option<i64>,
    pub icon: Option<String>,
    pub owner_id: Option<String>,
    #[serde(default)]
    pub private: bool,
}

impl Channel {
    pub fn is_conversation(&self) -> bool {
        self.kind == "text"
            || self.is_webhook()
            || (self.kind == "dm" && self.other_user_id.is_some())
    }

    pub fn is_webhook(&self) -> bool {
        self.kind == "webhook"
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub display_name: Option<String>,
    pub avatar_key: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub banned: bool,
}

impl User {
    pub fn name(&self) -> Option<&str> {
        self.display_name.as_deref().filter(|name| !name.is_empty())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VoiceFlags {
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
    #[serde(default)]
    pub speaking: bool,
    #[serde(
        rename = "radioListening",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub radio_listening: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct VideoPresence {
    pub cam: bool,
    pub screen: bool,
    pub watching: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub user_id: String,
    pub channel_id: String,
    #[serde(flatten)]
    pub flags: VoiceFlags,
    #[serde(flatten)]
    pub video: VideoPresence,
}

#[derive(Clone, Deserialize)]
pub struct RadioStation {
    pub id: String,
    pub name: String,
    pub country: String,
    pub codec: String,
    pub url: Option<String>,
    pub available: Option<bool>,
}

#[derive(Deserialize)]
pub struct RadioStationName {
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioSummary {
    pub channel_id: String,
    pub station: Option<RadioStationName>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub channel_id: String,
    pub user_id: String,
    pub body: String,
    pub embed: Option<ChatEmbed>,
    pub created_at: i64,
    pub reply_to: Option<ReplyRef>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub edited_at: Option<i64>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub previews: Vec<LinkPreview>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Deserialize)]
pub struct ChatEmbed {
    pub title: Option<String>,
    pub description: Option<String>,
    url: Option<String>,
    pub color: Option<EmbedColor>,
    #[serde(default)]
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
    pub timestamp: Option<String>,
}

impl ChatEmbed {
    pub fn title_link(&self) -> Option<Url> {
        let url = Url::parse(self.url.as_deref()?).ok()?;
        matches!(url.scheme(), "http" | "https").then_some(url)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EmbedColor {
    Success,
    Error,
    Warning,
    Info,
    #[serde(other)]
    Neutral,
}

#[derive(Clone, Deserialize)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub inline: bool,
}

impl ChatMessage {
    pub fn reacted_with(&self, emoji: &str, user_id: Option<&str>) -> bool {
        user_id.is_some_and(|user_id| {
            self.reactions.iter().any(|reaction| {
                reaction.emoji == emoji && reaction.user_ids.iter().any(|id| id == user_id)
            })
        })
    }

    pub fn pings(&self, user_id: &str) -> bool {
        self.user_id != user_id
            && (self
                .reply_to
                .as_ref()
                .is_some_and(|reply| reply.user_id == user_id)
                || mentions(&self.body, user_id))
    }
}

fn mentions(body: &str, user_id: &str) -> bool {
    let token = format!("<@{user_id}>");
    if !body.contains(&token)
        || user_id.is_empty()
        || !user_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return false;
    }

    // Match SPK mention tokens outside fenced and inline code.
    let mut fence = None;
    let mut lines = body.split('\n').peekable();
    while let Some(line) = lines.next() {
        if let Some(marker) = fence {
            if line.strip_prefix(marker).is_some_and(|rest: &str| {
                rest.trim_end_matches([' ', '\t', '\r'])
                    .bytes()
                    .all(|byte| matches!(byte, b'`' | b'~'))
            }) {
                fence = None;
            }
            continue;
        }
        if let Some(marker @ (b'`' | b'~')) = line.as_bytes().first().copied() {
            let count = line.bytes().take_while(|&byte| byte == marker).count();
            if count >= 3 && lines.peek().is_some() {
                fence = Some(&line[..count]);
                continue;
            }
        }
        let mut spans = line.split('`');
        if spans.next().is_some_and(|text| text.contains(&token)) {
            return true;
        }
        while let Some(code) = spans.next() {
            if spans.next().unwrap_or(code).contains(&token) {
                return true;
            }
        }
    }
    false
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplyRef {
    pub id: String,
    pub user_id: String,
    pub preview: String,
    pub deleted: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<ChatMessage>,
    pub has_more_before: bool,
    #[serde(default)]
    pub has_more_after: bool,
    pub first_unread_id: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
pub struct UnreadState {
    pub unread: usize,
    // SPK records direct Mentions and Replies in the same count.
    pub mentions: usize,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserPresence {
    Online,
    Idle,
    Busy,
    #[default]
    Offline,
}

impl UserPresence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Idle => "Away",
            Self::Busy => "Busy",
            Self::Offline => "Offline",
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    #[default]
    Online,
    Away,
    Busy,
    #[serde(rename = "offline")]
    Invisible,
}

impl UserStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Away => "Away",
            Self::Busy => "Busy",
            Self::Invisible => "Invisible",
        }
    }
}

pub enum PresenceReport {
    Status(UserStatus),
    Activity(bool),
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum Frame {
    #[serde(rename = "unread:update", rename_all = "camelCase")]
    UnreadUpdate {
        channel_id: String,
        #[serde(flatten)]
        state: UnreadState,
    },
    #[serde(rename = "message:new")]
    MessageNew {
        message: Box<ChatMessage>,
        nonce: Option<String>,
    },
    #[serde(rename = "message:delete", rename_all = "camelCase")]
    MessageDelete { channel_id: String, id: String },
    #[serde(rename = "message:edit")]
    MessageEdit { message: Box<ChatMessage> },
    #[serde(rename = "message:previews", rename_all = "camelCase")]
    MessagePreviews {
        channel_id: String,
        message_id: String,
        previews: Vec<LinkPreview>,
    },
    #[serde(rename = "reaction:update", rename_all = "camelCase")]
    ReactionUpdate {
        channel_id: String,
        message_id: String,
        reactions: Vec<Reaction>,
    },
    #[serde(rename = "pin:update", rename_all = "camelCase")]
    PinUpdate {
        channel_id: String,
        message_id: String,
        pinned: bool,
    },
    #[serde(rename = "message:error")]
    MessageError {
        nonce: Option<String>,
        error: String,
    },
    #[serde(rename = "typing", rename_all = "camelCase")]
    Typing { channel_id: String, user_id: String },
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "presence:update", rename_all = "camelCase")]
    UserPresence {
        user_id: String,
        status: UserPresence,
    },
    #[serde(rename = "voice:presence", rename_all = "camelCase")]
    Presence {
        user_id: String,
        #[serde(deserialize_with = "Option::deserialize")]
        channel_id: Option<String>,
        #[serde(flatten)]
        flags: VoiceFlags,
        #[serde(flatten)]
        video: VideoPresence,
    },
    #[serde(rename = "voice:joined", rename_all = "camelCase")]
    VoiceJoined {
        sid: String,
        channel_id: String,
        nonce: Option<String>,
    },
    #[serde(rename = "voice:kicked")]
    VoiceKicked { sid: String },
    #[serde(rename = "voice:move", rename_all = "camelCase")]
    VoiceMove { sid: String, channel_id: String },
    #[serde(rename = "voice:disconnect")]
    VoiceDisconnected { sid: String },
    #[serde(rename = "radio:snapshot")]
    RadioSnapshot { channels: Vec<RadioSummary> },
    #[serde(rename = "radio:update")]
    RadioUpdate(RadioSummary),
    #[serde(rename = "voice:sound", rename_all = "camelCase")]
    VoiceSound { user_id: String, sound_id: String },
    #[serde(rename = "voice:radio:state", rename_all = "camelCase")]
    RadioState {
        sid: String,
        channel_id: String,
        revision: u64,
        station: Option<RadioStation>,
    },
    #[serde(rename = "voice:radio:error", rename_all = "camelCase")]
    RadioError {
        sid: String,
        channel_id: Option<String>,
        error: String,
    },
    #[serde(rename = "channels:changed")]
    ChannelsChanged,
    #[serde(rename = "users:changed")]
    UsersChanged,
    #[serde(rename = "banned")]
    Banned,
    #[serde(rename = "signed-out")]
    SignedOut,
    #[serde(other)]
    Ignored,
}

#[derive(Clone, Copy)]
pub enum Connection {
    Connecting,
    Online,
    Reconnecting,
    Forbidden,
}

pub enum SocketEvent {
    Connection(Connection),
    Online(VoiceSocket),
    Frame(Frame),
    Unauthorized,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundboardClip {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub duration_ms: u32,
}

#[derive(Clone)]
pub enum VoiceModeration {
    Move(String),
    Disconnect,
}

#[derive(Deserialize)]
pub struct VoiceToken {
    pub token: String,
    pub url: String,
}

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum VoiceFrame<'a> {
    #[serde(rename = "voice:join", rename_all = "camelCase")]
    Join {
        sid: &'a str,
        channel_id: &'a str,
        #[serde(flatten)]
        mode: VoiceJoin<'a>,
        muted: bool,
        deafened: bool,
        watching: &'a [String],
    },
    #[serde(rename = "voice:ready")]
    Ready { sid: &'a str },
    #[serde(rename = "voice:state")]
    State {
        sid: &'a str,
        #[serde(flatten)]
        flags: VoiceFlags,
        #[serde(flatten)]
        video: &'a VideoPresence,
    },
    #[serde(rename = "voice:radio:set", rename_all = "camelCase")]
    RadioSet { sid: &'a str, station_id: &'a str },
    #[serde(rename = "voice:radio:stop")]
    RadioStop { sid: &'a str },
    #[serde(rename = "voice:sound", rename_all = "camelCase")]
    Sound { sid: &'a str, sound_id: &'a str },
    #[serde(rename = "voice:leave")]
    Leave { sid: &'a str },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum VoiceJoin<'a> {
    Initial { nonce: &'a str },
    Reclaim { ready: bool },
}

type SocketWrite = (String, Option<oneshot::Sender<()>>);

#[derive(Clone)]
pub struct VoiceSocket(async_channel::Sender<SocketWrite>);

impl VoiceSocket {
    pub fn send_message(
        &self,
        channel_id: &str,
        body: &str,
        nonce: &str,
        reply_to_id: Option<&str>,
        attachments: &[AttachmentRef],
    ) -> bool {
        let frame = serde_json::json!({
            "type": "message:send",
            "channelId": channel_id,
            "body": body,
            "nonce": nonce,
            "replyToId": reply_to_id,
            "attachments": attachments,
        });
        self.0.try_send((frame.to_string(), None)).is_ok()
    }

    pub fn typing(&self, channel_id: &str) -> bool {
        let frame = serde_json::json!({"type": "typing", "channelId": channel_id});
        self.0.try_send((frame.to_string(), None)).is_ok()
    }

    pub async fn send_presence(&self, report: PresenceReport) -> Result<(), Failure> {
        let frame = match report {
            PresenceReport::Status(status) => serde_json::json!({
                "type": "presence:set",
                "status": status,
            }),
            PresenceReport::Activity(active) => serde_json::json!({
                "type": if active { "presence:active" } else { "presence:idle" },
            }),
        };
        self.0
            .send((frame.to_string(), None))
            .await
            .map_err(|_| Failure::Connection)
    }

    pub fn same_connection(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }

    pub async fn send(&self, frame: VoiceFrame<'_>) -> Result<(), Failure> {
        let text = serde_json::to_string(&frame).expect("voice frames are serializable");
        let (written, receipt) = oneshot::channel();
        self.0
            .try_send((text, Some(written)))
            .map_err(|_| Failure::Connection)?;
        timeout(REQUEST_TIMEOUT, receipt)
            .await
            .map_err(|_| Failure::Connection)?
            .map_err(|_| Failure::Connection)
    }
}

#[derive(Clone)]
pub struct SpkClient {
    http: Client,
    storage: Client,
    public_media: Client,
    base: Url,
    origin: String,
    cookies: Arc<CookieStoreMutex>,
}

impl SpkClient {
    pub fn new(base: Url, cookies: CookieStore) -> Result<Self, Failure> {
        let origin = base.origin().ascii_serialization();
        let cookies = Arc::new(CookieStoreMutex::new(cookies));
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .cookie_provider(cookies.clone())
            .build()
            .map_err(|_| Failure::Connection)?;
        let storage = Client::builder()
            .redirect(Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| Failure::Connection)?;

        let public_media = Client::builder()
            .https_only(!cfg!(test))
            .no_proxy()
            .referer(false)
            .dns_resolver(Arc::new(PublicDns))
            .redirect(Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| Failure::Connection)?;

        Ok(Self {
            http,
            storage,
            public_media,
            base,
            origin,
            cookies,
        })
    }

    pub fn cookies(&self) -> &CookieStoreMutex {
        &self.cookies
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = self.base.join(path).expect("fixed SPK endpoint path");
        self.http
            .request(method, url)
            .header(header::ORIGIN, &self.origin)
    }

    async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, Failure> {
        let response = request.send().await.map_err(|_| Failure::Connection)?;
        if !response.status().is_success() {
            return Err(Failure::Http(response.status()));
        }
        Ok(response)
    }

    pub async fn sign_in(&self, email: String, password: String) -> Result<(), Failure> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Credentials {
            email: String,
            password: String,
            remember_me: bool,
        }

        Self::send(
            self.request(Method::POST, "/api/auth/sign-in/email")
                .json(&Credentials {
                    email,
                    password,
                    remember_me: true,
                }),
        )
        .await?;
        Ok(())
    }

    pub async fn check_invite(&self, token: &str) -> Result<AccountLinkCheck, Failure> {
        self.check_account_link("/api/invites/check", token).await
    }

    pub async fn check_password_reset(&self, token: &str) -> Result<AccountLinkCheck, Failure> {
        self.check_account_link("/api/reset-password/check", token)
            .await
    }

    async fn check_account_link(
        &self,
        path: &str,
        token: &str,
    ) -> Result<AccountLinkCheck, Failure> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", token)
            .finish();
        self.get(&format!("{path}?{query}")).await
    }

    pub async fn sign_up(&self, token: &str, email: &str, password: &str) -> Result<(), Failure> {
        let name = email.split('@').next().unwrap_or_default();
        let mut token =
            header::HeaderValue::from_str(token).map_err(|_| Failure::InvalidResponse)?;
        token.set_sensitive(true);
        Self::send(
            self.request(Method::POST, "/api/auth/sign-up/email")
                .header("x-invite-token", token)
                .json(&serde_json::json!({"email": email, "password": password, "name": name})),
        )
        .await?;
        Ok(())
    }

    pub async fn reset_password(&self, token: &str, password: &str) -> Result<(), Failure> {
        Self::send(
            self.request(Method::POST, "/api/reset-password")
                .json(&serde_json::json!({"token": token, "password": password})),
        )
        .await?;
        Ok(())
    }

    pub async fn set_nickname(&self, nickname: &str) -> Result<String, Failure> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Nickname {
            display_name: String,
        }

        Self::send(
            self.request(Method::POST, "/api/profile/nickname")
                .json(&serde_json::json!({"displayName": nickname})),
        )
        .await?
        .json::<Nickname>()
        .await
        .map(|result| result.display_name)
        .map_err(|_| Failure::InvalidResponse)
    }

    pub async fn sign_out(&self) -> Result<(), Failure> {
        Self::send(self.request(Method::POST, "/api/auth/sign-out")).await?;
        Ok(())
    }

    pub async fn profile(&self) -> Result<Profile, Failure> {
        self.get("/api/profile/me").await
    }

    pub async fn upload_avatar(
        &self,
        bytes: Vec<u8>,
        mime: &'static str,
    ) -> Result<String, Failure> {
        #[derive(Deserialize)]
        struct Upload {
            key: String,
            url: String,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Avatar {
            avatar_key: String,
        }

        let upload: Upload = Self::send(
            self.request(Method::POST, "/api/profile/avatar-upload")
                .json(&serde_json::json!({ "size": bytes.len(), "type": mime })),
        )
        .await?
        .json()
        .await
        .map_err(|_| Failure::InvalidResponse)?;
        let url = Url::parse(&upload.url)
            .ok()
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        if upload.key.is_empty() {
            return Err(Failure::InvalidResponse);
        }
        // The storage response cannot expire the authenticated SPK session.
        Self::send(
            self.storage
                .put(url)
                .header(header::CONTENT_TYPE, mime)
                .body(bytes),
        )
        .await
        .map_err(|_| Failure::InvalidResponse)?;
        let avatar: Avatar = Self::send(
            self.request(Method::POST, "/api/profile/avatar")
                .json(&serde_json::json!({ "key": upload.key })),
        )
        .await?
        .json()
        .await
        .map_err(|_| Failure::InvalidResponse)?;
        Ok(avatar.avatar_key)
    }

    pub async fn admin_profile_email(&self, id: &str) -> Result<Option<String>, Failure> {
        #[derive(Deserialize)]
        struct Directory {
            users: Vec<Entry>,
        }

        #[derive(Deserialize)]
        struct Entry {
            id: String,
            email: String,
        }

        let directory: Directory = self.get("/api/admin/users").await?;
        Ok(directory
            .users
            .into_iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.email))
    }

    pub async fn changelog(&self, page: u32, head: Option<&str>) -> Result<ChangelogPage, Failure> {
        let mut url = self.base.join("/api/changelog").expect("fixed SPK path");
        url.query_pairs_mut().append_pair("page", &page.to_string());
        if let Some(head) = head {
            url.query_pairs_mut().append_pair("head", head);
        }
        let response = Self::send(self.http.get(url).header(header::ORIGIN, &self.origin)).await?;
        let result: ChangelogPage = response
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        if result.head.as_deref().is_some_and(|head| !commit_sha(head))
            || result
                .next_page
                .is_some_and(|next| next <= page || result.head.is_none())
            || result.commits.iter().any(|entry| !commit_sha(&entry.sha))
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(result)
    }

    pub async fn channels(&self) -> Result<Vec<Channel>, Failure> {
        self.get("/api/channels").await
    }

    pub async fn open_direct_message(&self, user_id: &str) -> Result<Channel, Failure> {
        let response = Self::send(
            self.request(Method::POST, "/api/dms")
                .json(&serde_json::json!({ "userId": user_id })),
        )
        .await?;
        let channel: Channel = response
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        if channel.kind != "dm"
            || channel.id.is_empty()
            || channel.other_user_id.as_deref() != Some(user_id)
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(channel)
    }

    pub async fn messages(
        &self,
        channel_id: &str,
        before: Option<&str>,
    ) -> Result<MessagePage, Failure> {
        self.message_page(channel_id, before.map(|id| ("before", id)))
            .await
    }

    pub async fn messages_around(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<MessagePage, Failure> {
        self.message_page(channel_id, Some(("around", message_id)))
            .await
    }

    pub async fn messages_after(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<MessagePage, Failure> {
        self.message_page(channel_id, Some(("after", message_id)))
            .await
    }

    async fn message_page(
        &self,
        channel_id: &str,
        cursor: Option<(&str, &str)>,
    ) -> Result<MessagePage, Failure> {
        let mut url = self.base.join("/api/channels/").expect("fixed SPK path");
        url.path_segments_mut()
            .expect("validated SPK URL")
            .pop_if_empty()
            .extend([channel_id, "messages"]);
        if let Some((key, id)) = cursor {
            url.query_pairs_mut().append_pair(key, id);
        } else {
            url.query_pairs_mut().append_pair("unread", "1");
        }
        let response = Self::send(self.http.get(url).header(header::ORIGIN, &self.origin)).await?;
        let page: MessagePage = response
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        if page
            .messages
            .iter()
            .any(|message| message.channel_id != channel_id)
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(page)
    }

    pub async fn update_read_point(
        &self,
        channel_id: &str,
        message_id: Option<&str>,
    ) -> Result<UnreadState, Failure> {
        let mut url = self.base.join("/api/channels/").expect("fixed SPK path");
        let endpoint = if message_id.is_some() {
            "unread"
        } else {
            "read"
        };
        url.path_segments_mut()
            .expect("validated SPK URL")
            .pop_if_empty()
            .extend([channel_id, endpoint]);
        let mut request = self.http.post(url).header(header::ORIGIN, &self.origin);
        if let Some(message_id) = message_id {
            request = request.json(&serde_json::json!({ "messageId": message_id }));
        }
        Self::send(request)
            .await?
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)
    }

    pub async fn unread(&self) -> Result<std::collections::HashMap<String, UnreadState>, Failure> {
        self.get("/api/unread").await
    }

    pub async fn users(&self) -> Result<Vec<User>, Failure> {
        self.get("/api/users").await
    }

    pub async fn user_presence(&self) -> Result<HashMap<String, UserPresence>, Failure> {
        self.get("/api/users/presence").await
    }

    pub async fn voice_presence(&self) -> Result<Vec<Member>, Failure> {
        self.get("/api/voice").await
    }

    pub async fn radio_stations(&self, query: &str) -> Result<Vec<RadioStation>, Failure> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", query)
            .finish();
        self.get(&format!("/api/radio/stations?{query}")).await
    }

    pub async fn radio_favorites(&self) -> Result<Vec<RadioStation>, Failure> {
        self.get("/api/radio/favorites").await
    }

    pub async fn set_radio_favorite(
        &self,
        id: &str,
        saved: bool,
    ) -> Result<Option<RadioStation>, Failure> {
        let id = uuid::Uuid::parse_str(id).map_err(|_| Failure::InvalidResponse)?;
        let path = format!("/api/radio/favorites/{id}");
        if saved {
            Self::send(self.request(Method::PUT, &path))
                .await?
                .json()
                .await
                .map(Some)
                .map_err(|_| Failure::InvalidResponse)
        } else {
            Self::send(self.request(Method::DELETE, &path)).await?;
            Ok(None)
        }
    }

    pub async fn soundboard_clips(&self) -> Result<Vec<SoundboardClip>, Failure> {
        self.get("/api/sounds").await
    }

    pub async fn soundboard_file(&self, id: uuid::Uuid) -> Result<Vec<u8>, Failure> {
        #[derive(Deserialize)]
        struct SignedFile {
            url: String,
        }

        let file: SignedFile = self.get(&format!("/api/sounds/{id}/file")).await?;
        let url = Url::parse(&file.url)
            .ok()
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        // A storage response cannot expire the authenticated SPK session.
        self.storage_bytes(url, MAX_SOUND_BYTES)
            .await
            .map_err(|_| Failure::InvalidResponse)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Failure> {
        Self::send(self.request(Method::GET, path))
            .await?
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)
    }

    pub async fn avatar(&self, user_id: &str, key: &str) -> Result<Vec<u8>, Failure> {
        let mut url = self
            .base
            .join("/api/avatar/")
            .expect("fixed SPK endpoint path");
        if matches!(user_id, "" | "." | "..") {
            return Err(Failure::InvalidResponse);
        }
        url.path_segments_mut()
            .expect("SPK URL has a host")
            .pop_if_empty()
            .push(user_id);
        url.query_pairs_mut().append_pair("v", key);
        self.storage_redirect(
            self.http.get(url).header(header::ORIGIN, &self.origin),
            MAX_AVATAR_BYTES,
        )
        .await
    }

    async fn storage_redirect(
        &self,
        request: reqwest::RequestBuilder,
        max_bytes: usize,
    ) -> Result<Vec<u8>, Failure> {
        let response = request.send().await.map_err(|_| Failure::Connection)?;
        if !response.status().is_redirection() {
            return Err(Failure::Http(response.status()));
        }
        let url = response
            .headers()
            .get(header::LOCATION)
            .and_then(|location| Url::parse(location.to_str().ok()?).ok())
            .filter(signed_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        // The storage URL carries its own authorization and can use a different host.
        self.storage_bytes(url, max_bytes).await
    }

    async fn storage_bytes(&self, url: Url, max_bytes: usize) -> Result<Vec<u8>, Failure> {
        Self::limited_bytes(self.storage.get(url), max_bytes).await
    }

    async fn limited_bytes(
        request: reqwest::RequestBuilder,
        max_bytes: usize,
    ) -> Result<Vec<u8>, Failure> {
        let mut response = Self::send(request).await?;
        let mut bytes = Vec::new();
        if response
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(Failure::InvalidResponse);
        }
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Connection)? {
            if chunk.len() > max_bytes - bytes.len() {
                return Err(Failure::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub async fn moderate_voice(
        &self,
        user_id: &str,
        action: &VoiceModeration,
    ) -> Result<(), Failure> {
        let (path, body) = match action {
            VoiceModeration::Move(channel_id) => (
                "/api/admin/voice/move",
                serde_json::json!({ "userId": user_id, "channelId": channel_id }),
            ),
            VoiceModeration::Disconnect => (
                "/api/admin/voice/disconnect",
                serde_json::json!({ "userId": user_id }),
            ),
        };
        Self::send(self.request(Method::POST, path).json(&body)).await?;
        Ok(())
    }

    pub async fn voice_token(&self, channel_id: &str) -> Result<VoiceToken, Failure> {
        Self::send(
            self.request(Method::POST, "/api/voice/token")
                .json(&serde_json::json!({ "channelId": channel_id })),
        )
        .await?
        .json()
        .await
        .map_err(|_| Failure::InvalidResponse)
    }

    pub async fn socket(&self, endpoint: &str, events: async_channel::Sender<SocketEvent>) {
        // The cookie jar applies the HTTP scheme's Secure rules to the equivalent WS request.
        let cookie_url = self.base.join("/_ws").expect("fixed SPK socket path");
        let mut delay = Duration::from_secs(1);
        loop {
            let mut request = endpoint
                .into_client_request()
                .expect("validated SPK socket URL");
            let headers = request.headers_mut();
            headers.insert(
                header::ORIGIN,
                self.origin.parse().expect("validated SPK origin"),
            );
            if let Some(mut cookies) = self.cookies.cookies(&cookie_url) {
                cookies.set_sensitive(true);
                headers.insert(header::COOKIE, cookies);
            }

            let state = match timeout(REQUEST_TIMEOUT, connect_async(request)).await {
                Ok(Ok((mut socket, _))) => {
                    delay = Duration::from_secs(1);
                    let (writes, outgoing) = async_channel::bounded::<SocketWrite>(4);
                    if events
                        .send(SocketEvent::Online(VoiceSocket(writes)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    loop {
                        let message = tokio::select! {
                            message = socket.next() => match message {
                                Some(Ok(message)) => message,
                                _ => break,
                            },
                            Ok((text, written)) = outgoing.recv() => {
                                if written.as_ref().is_some_and(oneshot::Sender::is_closed) {
                                    continue;
                                }
                                if !matches!(
                                    timeout(REQUEST_TIMEOUT, socket.send(Message::text(text))).await,
                                    Ok(Ok(()))
                                ) {
                                    break;
                                }
                                if let Some(written) = written {
                                    let _ = written.send(());
                                }
                                continue;
                            }
                        };
                        match message {
                            Message::Text(text) => match serde_json::from_str(text.as_str()) {
                                Ok(Frame::Ping) => {
                                    if !matches!(
                                        timeout(
                                            REQUEST_TIMEOUT,
                                            socket.send(Message::text("ping"))
                                        )
                                        .await,
                                        Ok(Ok(()))
                                    ) {
                                        break;
                                    }
                                }
                                Ok(Frame::Ignored) | Err(_) => {}
                                Ok(frame) => {
                                    if events.send(SocketEvent::Frame(frame)).await.is_err() {
                                        return;
                                    }
                                }
                            },
                            Message::Ping(_) => {
                                if !matches!(
                                    timeout(REQUEST_TIMEOUT, socket.flush()).await,
                                    Ok(Ok(()))
                                ) {
                                    break;
                                }
                            }
                            Message::Close(_) => {
                                let _ = timeout(REQUEST_TIMEOUT, socket.flush()).await;
                                break;
                            }
                            _ => {}
                        }
                    }
                    Connection::Reconnecting
                }
                Ok(Err(WsError::Http(response)))
                    if response.status() == StatusCode::UNAUTHORIZED =>
                {
                    let _ = events.send(SocketEvent::Unauthorized).await;
                    return;
                }
                Ok(Err(WsError::Http(response))) if response.status() == StatusCode::FORBIDDEN => {
                    Connection::Forbidden
                }
                _ => Connection::Reconnecting,
            };

            if events.send(SocketEvent::Connection(state)).await.is_err() {
                return;
            }
            sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(15));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_selection_is_specific_to_the_message_emoji_and_user() {
        let messages: Vec<ChatMessage> = serde_json::from_value(serde_json::json!([
            {
                "id": "first", "channelId": "chat", "userId": "author",
                "body": "", "createdAt": 0,
                "reactions": [
                    {"emoji": "👍", "userIds": ["me", "other"]},
                    {"emoji": "❤️", "userIds": ["other"]}
                ]
            },
            {
                "id": "second", "channelId": "chat", "userId": "author",
                "body": "", "createdAt": 1,
                "reactions": [{"emoji": "👍", "userIds": ["other"]}]
            }
        ]))
        .unwrap();

        assert!(messages[0].reacted_with("👍", Some("me")));
        assert!(!messages[0].reacted_with("❤️", Some("me")));
        assert!(!messages[0].reacted_with("👍", None));
        assert!(!messages[1].reacted_with("👍", Some("me")));
    }
}
