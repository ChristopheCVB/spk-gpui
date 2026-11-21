use std::{io, net::IpAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

pub(crate) fn public_media_url(url: &Url) -> bool {
    url.as_str().len() <= 4096
        && url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && match url.host() {
            Some(Host::Domain(name)) => {
                let name = name.trim_end_matches('.');
                name.contains('.') && !name.ends_with(".local") && !name.ends_with(".localhost")
            }
            Some(Host::Ipv4(ip)) => public_ip(ip.into()),
            Some(Host::Ipv6(ip)) => public_ip(ip.into()),
            None => false,
        }
}

pub(crate) struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .collect::<Vec<_>>();
            if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Media host is not public",
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => !matches!(
            ip.octets(),
            [0 | 10 | 127 | 224..=255, ..]
                | [100, 64..=127, ..]
                | [169, 254, ..]
                | [172, 16..=31, ..]
                | [192, 0, 0 | 2, _]
                | [192, 168, ..]
                | [198, 18 | 19, ..]
                | [198, 51, 100, _]
                | [203, 0, 113, _]
        ),
        IpAddr::V6(ip) => {
            let [prefix, next, ..] = ip.segments();
            (0x2000..=0x3fff).contains(&prefix)
                && !matches!((prefix, next), (0x2001, 0 | 0x0db8) | (0x2002, _))
        }
    }
}
