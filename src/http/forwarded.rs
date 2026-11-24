//! Public origin (`scheme://host`) for URLs handed back to clients
//! (`qr_link`, `file_url`), mirroring Fiber's `c.Scheme()` / `c.Host()` with
//! `TrustProxy` + `APP_TRUSTED_PROXIES`: forwarded headers count only when the
//! TCP peer is a trusted proxy, so a direct client cannot spoof the host.

use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, Uri, header, request::Parts},
};
use ipnet::IpNet;

use super::AppState;

/// `scheme://host` as seen by the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin(pub String);

impl PublicOrigin {
    /// `{origin}{base}{path}`; `path` starts with `/`.
    pub fn url(&self, base: &str, path: &str) -> String {
        format!("{}{base}{path}", self.0)
    }
}

impl FromRequestParts<AppState> for PublicOrigin {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        Ok(origin(
            &parts.headers,
            &parts.uri,
            peer,
            &state.settings.trusted_proxies,
        ))
    }
}

/// Fiber `IsProxyTrusted`: no peer (in-process tests) is never trusted.
pub fn is_trusted(peer: Option<IpAddr>, trusted: &[IpNet]) -> bool {
    peer.map(|ip| ip.to_canonical())
        .is_some_and(|ip| trusted.iter().any(|net| net.contains(&ip)))
}

pub fn origin(
    headers: &HeaderMap,
    uri: &Uri,
    peer: Option<IpAddr>,
    trusted: &[IpNet],
) -> PublicOrigin {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let direct_host = || {
        get(header::HOST.as_str())
            .map(str::to_owned)
            .or_else(|| uri.authority().map(|a| a.to_string()))
            .unwrap_or_else(|| "localhost".to_owned())
    };
    if !is_trusted(peer, trusted) {
        return PublicOrigin(format!("http://{}", direct_host()));
    }
    let host = get("x-forwarded-host")
        .map(|h| h.split(',').next().unwrap_or(h).trim().to_owned())
        .unwrap_or_else(direct_host);
    PublicOrigin(format!("{}://{host}", forwarded_scheme(&get)))
}

/// Fiber `Scheme()`: `X-Forwarded-Proto`/`-Protocol` (first value),
/// `X-Forwarded-Ssl: on`, `X-Url-Scheme`; only `http`/`https` are accepted.
fn forwarded_scheme<'a>(get: &impl Fn(&str) -> Option<&'a str>) -> &'static str {
    let canonical = |v: &str| match v.split(',').next().unwrap_or(v).trim() {
        s if s.eq_ignore_ascii_case("https") => Some("https"),
        s if s.eq_ignore_ascii_case("http") => Some("http"),
        _ => None,
    };
    get("x-forwarded-proto")
        .and_then(canonical)
        .or_else(|| get("x-forwarded-protocol").and_then(canonical))
        .or_else(|| {
            get("x-forwarded-ssl")
                .filter(|v| v.eq_ignore_ascii_case("on"))
                .map(|_| "https")
        })
        .or_else(|| get("x-url-scheme").and_then(canonical))
        .unwrap_or("http")
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    const PROXIED: &[(&str, &str)] = &[
        ("host", "internal:3000"),
        ("x-forwarded-proto", "https"),
        ("x-forwarded-host", "wa.example.com, other"),
    ];

    #[test]
    fn untrusted_peer_ignores_forwarded_headers() {
        let h = headers(PROXIED);
        let uri = Uri::from_static("/app/login");
        let peer = Some("203.0.113.9".parse().unwrap());
        for trusted in [nets(&[]), nets(&["10.0.0.0/8"])] {
            assert_eq!(origin(&h, &uri, peer, &trusted).0, "http://internal:3000");
        }
        // No peer address (in-process router): never trusted.
        assert_eq!(
            origin(&h, &uri, None, &nets(&["0.0.0.0/0"])).0,
            "http://internal:3000"
        );
    }

    #[test]
    fn trusted_peer_uses_forwarded_headers() {
        let h = headers(PROXIED);
        let uri = Uri::from_static("/app/login");
        let o = origin(
            &h,
            &uri,
            Some("10.1.2.3".parse().unwrap()),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(o.0, "https://wa.example.com");
        assert_eq!(
            o.url("/gowa", "/statics/x.png"),
            "https://wa.example.com/gowa/statics/x.png"
        );
        // IPv4-mapped IPv6 peers (dual-stack listener) match IPv4 ranges.
        let mapped = Some("::ffff:10.1.2.3".parse().unwrap());
        assert!(is_trusted(mapped, &nets(&["10.0.0.0/8"])));
    }

    #[test]
    fn scheme_fallbacks_like_fiber() {
        let uri = Uri::from_static("/");
        let peer = Some("127.0.0.1".parse().unwrap());
        let trusted = nets(&["127.0.0.1/32"]);
        let scheme = |pairs| origin(&headers(pairs), &uri, peer, &trusted).0;
        assert_eq!(scheme(&[("host", "h")]), "http://h");
        assert_eq!(
            scheme(&[("host", "h"), ("x-forwarded-ssl", "on")]),
            "https://h"
        );
        assert_eq!(
            scheme(&[("host", "h"), ("x-url-scheme", "HTTPS")]),
            "https://h"
        );
        assert_eq!(
            scheme(&[("host", "h"), ("x-forwarded-proto", "https,http")]),
            "https://h"
        );
        // Anything but http/https is rejected (it is spliced into a URL).
        assert_eq!(
            scheme(&[("host", "h"), ("x-forwarded-proto", "javascript")]),
            "http://h"
        );
    }
}
