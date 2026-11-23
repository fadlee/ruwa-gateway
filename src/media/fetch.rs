//! Fetching user-supplied URLs (`image_url`, `file_url`, link previews).
//!
//! Like GOWA's `newUntrustedHTTPClient`, requests to loopback, private,
//! link-local and other non-public addresses are refused, so the API cannot
//! be used to probe the host's network. Redirects are limited and each hop's
//! host is re-checked.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use regex::Regex;
use reqwest::{Url, header::CONTENT_TYPE};

use super::Upload;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HTML: usize = 2_000_000;
const MAX_PREVIEW_IMAGE: usize = 5_000_000;
const MAX_REDIRECTS: usize = 5;

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT
        || (o[0] == 198 && (18..20).contains(&o[1])) // benchmarking
        || o[0] >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // unique local
        || (s[0] & 0xffc0) == 0xfe80 // link local
        || (s[0] == 0x2001 && s[1] == 0x0db8)) // documentation
}

async fn check_host(url: &Url) -> anyhow::Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("unsupported URL scheme {}", url.scheme());
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("URL has no host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<_> = tokio::net::lookup_host((host.trim_matches(['[', ']']), port))
        .await?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|a| !is_public_ip(a.ip())) {
        anyhow::bail!("refusing to fetch non-public address {host}");
    }
    Ok(())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Mozilla/5.0 (compatible; ruwa-gateway)")
        .build()
        .expect("http client")
}

/// GET with manual redirects so each hop is checked.
async fn get(url: &str, max: usize) -> anyhow::Result<(Url, String, Vec<u8>)> {
    let http = client();
    let mut url = Url::parse(url)?;
    for _ in 0..=MAX_REDIRECTS {
        check_host(&url).await?;
        let mut res = http.get(url.clone()).send().await?;
        if res.status().is_redirection() {
            let location = res
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("redirect without location"))?;
            url = url.join(location)?;
            continue;
        }
        if !res.status().is_success() {
            anyhow::bail!("HTTP {} fetching {url}", res.status().as_u16());
        }
        if res.content_length().is_some_and(|n| n as usize > max) {
            anyhow::bail!("file size exceeds the maximum limit of {max} bytes");
        }
        let content_type = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > max {
                anyhow::bail!("file size exceeds the maximum limit of {max} bytes");
            }
        }
        return Ok((url, content_type, body));
    }
    anyhow::bail!("too many redirects")
}

/// Download a file for sending; the name comes from the final URL path.
pub async fn download(url: &str, max: usize) -> anyhow::Result<Upload> {
    let (final_url, content_type, bytes) = get(url, max).await?;
    let filename = final_url
        .path_segments()
        .and_then(|mut s| s.next_back().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .map(|s| {
            percent_encoding::percent_decode_str(&s)
                .decode_utf8_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "file".to_owned());
    Ok(Upload {
        filename,
        content_type,
        bytes,
    })
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LinkPreview {
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
}

/// Parse `og:*` / `<title>` / `description` metadata from HTML.
pub fn parse_preview(html: &str, base: &Url) -> LinkPreview {
    static META: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r#"(?is)<meta\s+[^>]*>"#).unwrap());
    static ATTR: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?is)([a-z:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
    });
    static TITLE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap());

    let mut p = LinkPreview::default();
    let mut plain_description = String::new();
    for tag in META.find_iter(html) {
        let mut key = String::new();
        let mut content = String::new();
        for a in ATTR.captures_iter(tag.as_str()) {
            let value = a
                .get(2)
                .or(a.get(3))
                .map(|m| m.as_str())
                .unwrap_or_default();
            match a[1].to_ascii_lowercase().as_str() {
                "property" | "name" => key = value.to_ascii_lowercase(),
                "content" => content = decode_entities(value),
                _ => {}
            }
        }
        match key.as_str() {
            "og:title" if p.title.is_empty() => p.title = content,
            "og:description" if p.description.is_empty() => p.description = content,
            "description" if plain_description.is_empty() => plain_description = content,
            "og:image" if p.image_url.is_none() => {
                p.image_url = base.join(&content).ok().map(|u| u.to_string())
            }
            _ => {}
        }
    }
    if p.title.is_empty()
        && let Some(t) = TITLE.captures(html)
    {
        p.title = decode_entities(t[1].trim());
    }
    if p.description.is_empty() {
        p.description = plain_description;
    }
    p
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Fetch a page's preview and a JPEG thumbnail of its image. Failures to
/// fetch the image are not errors; failures to fetch the page are (GOWA
/// `GetMetaDataFromURL`).
pub async fn link_preview(url: &str) -> anyhow::Result<(LinkPreview, Option<Vec<u8>>)> {
    let (final_url, _, html) = get(url, MAX_HTML).await?;
    let preview = parse_preview(&String::from_utf8_lossy(&html), &final_url);
    let thumb = match &preview.image_url {
        Some(img) => get(img, MAX_PREVIEW_IMAGE)
            .await
            .ok()
            .and_then(|(_, _, bytes)| super::image::thumbnail_of(&bytes)),
        None => None,
    };
    Ok((preview, thumb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ip_rules() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[tokio::test]
    async fn refuses_private_targets() {
        let err = download("http://127.0.0.1:9/x.png", 10).await.unwrap_err();
        assert!(err.to_string().contains("non-public"));
        let err = download("file:///etc/passwd", 10).await.unwrap_err();
        assert!(err.to_string().contains("scheme"));
    }

    #[test]
    fn parses_og_tags() {
        let html = r#"<html><head><title>Fallback</title>
            <meta property="og:title" content="Hello &amp; welcome">
            <meta name="description" content="plain">
            <meta content='/img.png' property='og:image'></head></html>"#;
        let p = parse_preview(html, &Url::parse("https://ex.com/a/b").unwrap());
        assert_eq!(p.title, "Hello & welcome");
        assert_eq!(p.description, "plain");
        assert_eq!(p.image_url.as_deref(), Some("https://ex.com/img.png"));
        let p = parse_preview("<title> T </title>", &Url::parse("https://ex.com").unwrap());
        assert_eq!(p.title, "T");
    }
}

#[cfg(test)]
mod network_tests {
    /// Hits the public internet; run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_link_preview() {
        let (p, _) = super::link_preview("https://example.com").await.unwrap();
        assert_eq!(p.title, "Example Domain");
        let f = super::download("https://example.com/", 1_000_000)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&f.bytes).contains("Example Domain"));
    }
}
