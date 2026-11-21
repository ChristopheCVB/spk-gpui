use std::{sync::Arc, time::Duration};

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

fn avatar_storage_url(url: &Url) -> bool {
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

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: Option<String>,
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_key: Option<String>,
}

impl Profile {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.email)
    }
}

#[derive(Clone, Deserialize)]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub display_name: Option<String>,
    pub avatar_key: Option<String>,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub user_id: String,
    pub channel_id: String,
    #[serde(flatten)]
    pub flags: VoiceFlags,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum Frame {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "voice:presence", rename_all = "camelCase")]
    Presence {
        user_id: String,
        #[serde(deserialize_with = "Option::deserialize")]
        channel_id: Option<String>,
        #[serde(flatten)]
        flags: VoiceFlags,
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
        watching: [&'a str; 0],
    },
    #[serde(rename = "voice:ready")]
    Ready { sid: &'a str },
    #[serde(rename = "voice:state")]
    State {
        sid: &'a str,
        #[serde(flatten)]
        flags: VoiceFlags,
    },
    #[serde(rename = "voice:leave")]
    Leave { sid: &'a str },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum VoiceJoin<'a> {
    Initial { nonce: &'a str },
    Reclaim { ready: bool },
}

type SocketWrite = (String, oneshot::Sender<()>);

#[derive(Clone)]
pub struct VoiceSocket(async_channel::Sender<SocketWrite>);

impl VoiceSocket {
    pub fn same_connection(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }

    pub async fn send(&self, frame: VoiceFrame<'_>) -> Result<(), Failure> {
        let text = serde_json::to_string(&frame).expect("voice frames are serializable");
        let (written, receipt) = oneshot::channel();
        self.0
            .try_send((text, written))
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
    avatar_storage: Client,
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
        let avatar_storage = Client::builder()
            .redirect(Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| Failure::Connection)?;

        Ok(Self {
            http,
            avatar_storage,
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

    pub async fn sign_out(&self) -> Result<(), Failure> {
        Self::send(self.request(Method::POST, "/api/auth/sign-out")).await?;
        Ok(())
    }

    pub async fn profile(&self) -> Result<Profile, Failure> {
        self.get("/api/profile/me").await
    }

    pub async fn channels(&self) -> Result<Vec<Channel>, Failure> {
        self.get("/api/channels").await
    }

    pub async fn users(&self) -> Result<Vec<User>, Failure> {
        self.get("/api/users").await
    }

    pub async fn voice_presence(&self) -> Result<Vec<Member>, Failure> {
        self.get("/api/voice").await
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
        let response = self
            .http
            .get(url)
            .header(header::ORIGIN, &self.origin)
            .send()
            .await
            .map_err(|_| Failure::Connection)?;
        if !response.status().is_redirection() {
            return Err(Failure::Http(response.status()));
        }
        let url = response
            .headers()
            .get(header::LOCATION)
            .and_then(|location| Url::parse(location.to_str().ok()?).ok())
            .filter(avatar_storage_url)
            .ok_or(Failure::InvalidResponse)?;
        // The storage URL carries its own authorization and can use a different host.
        let mut response = Self::send(self.avatar_storage.get(url)).await?;
        let mut bytes = Vec::new();
        if response
            .content_length()
            .is_some_and(|len| len > MAX_AVATAR_BYTES as u64)
        {
            return Err(Failure::InvalidResponse);
        }
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Connection)? {
            if chunk.len() > MAX_AVATAR_BYTES - bytes.len() {
                return Err(Failure::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
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
                                if written.is_closed() {
                                    continue;
                                }
                                if !matches!(
                                    timeout(REQUEST_TIMEOUT, socket.send(Message::text(text))).await,
                                    Ok(Ok(()))
                                ) {
                                    break;
                                }
                                let _ = written.send(());
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
