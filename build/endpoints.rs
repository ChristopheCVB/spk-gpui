use url::{Host, Url};

pub fn spk(value: &str, allow_loopback: bool) -> Result<(Url, Url), &'static str> {
    let url = parse(value, "https", "http", allow_loopback)?;
    let (_, authority_and_path) = value.split_once("://").ok_or("expected an absolute URL")?;
    if let Some((_, path)) = authority_and_path.split_once('/')
        && !path.is_empty()
    {
        return Err("expected an origin with no path beyond /");
    }

    let mut websocket = url.clone();
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    websocket
        .set_scheme(scheme)
        .map_err(|_| "could not derive the SPK WebSocket URL")?;
    websocket.set_path("/_ws");
    Ok((url, websocket))
}

pub fn livekit(value: &str, allow_loopback: bool) -> Result<Url, &'static str> {
    parse(value, "wss", "ws", allow_loopback)
}

fn parse(
    value: &str,
    secure_scheme: &str,
    loopback_scheme: &str,
    allow_loopback: bool,
) -> Result<Url, &'static str> {
    let (_, authority_and_path) = value.split_once("://").ok_or("expected an absolute URL")?;
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.is_empty() || value.contains('\\') || value.chars().any(invalid_url_char) {
        return Err("expected an absolute URL with a host");
    }

    let url = Url::parse(value).map_err(|_| "expected a valid URL")?;
    let host = url.host().ok_or("expected a URL with a host")?;
    if authority.contains('@') || url.query().is_some() || url.fragment().is_some() {
        return Err("credentials, queries, and fragments are forbidden");
    }

    let is_loopback = match host {
        Host::Domain(name) => name == "localhost",
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    };
    if url.scheme() == secure_scheme
        || (allow_loopback && is_loopback && url.scheme() == loopback_scheme)
    {
        Ok(url)
    } else if secure_scheme == "https" {
        Err("expected HTTPS, or HTTP on loopback in a debug build")
    } else {
        Err("expected WSS, or WS on loopback in a debug build")
    }
}

fn invalid_url_char(character: char) -> bool {
    character.is_whitespace() || character.is_control()
}
