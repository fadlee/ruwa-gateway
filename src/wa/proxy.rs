//! `WHATSAPP_PROXY` (whatsmeow `SetProxyAddress`): routes the WhatsApp
//! WebSocket and media up/downloads through a SOCKS5 or HTTP(S) proxy.
//! URLs supplied by API callers (media, link previews) are still fetched
//! directly, so the private-address check applies to the address dialed.

use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use percent_encoding::percent_decode_str;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

use super::sdk::{
    async_trait,
    http::{HttpClient, UreqHttpClient},
    transport::{
        BoxedStream, Connector, StreamDialer, TokioWebSocketTransportFactory, default_tls_connector,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `socks5` / `socks5h`; the target hostname is always resolved by the
    /// proxy (like Go's `x/net/proxy`), so local DNS is never consulted.
    Socks5,
    Http,
    /// TLS to the proxy, then `CONNECT`.
    Https,
}

/// A parsed proxy URL. `Debug`/`Display` never print the credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyUrl {
    scheme: Scheme,
    host: String,
    port: u16,
    auth: Option<(String, String)>,
}

impl ProxyUrl {
    /// Parse `scheme://[user[:pass]@]host[:port]`. Errors never echo the URL,
    /// which may carry credentials.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let url = url::Url::parse(raw).map_err(|e| format!("invalid proxy URL: {e}"))?;
        let scheme = match url.scheme() {
            "socks5" | "socks5h" => Scheme::Socks5,
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            other => {
                return Err(format!(
                    "unsupported proxy scheme {other:?} (use socks5, http or https)"
                ));
            }
        };
        let host = match url.host() {
            Some(url::Host::Domain(d)) if !d.is_empty() => d.to_owned(),
            Some(url::Host::Ipv4(ip)) => ip.to_string(),
            Some(url::Host::Ipv6(ip)) => ip.to_string(),
            _ => return Err("invalid proxy URL: missing host".into()),
        };
        let port = url.port().unwrap_or(match scheme {
            Scheme::Socks5 => 1080,
            Scheme::Http => 80,
            Scheme::Https => 443,
        });
        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        let auth = (!url.username().is_empty() || url.password().is_some()).then(|| {
            (
                decode(url.username()),
                decode(url.password().unwrap_or_default()),
            )
        });
        if let (Scheme::Socks5, Some((user, pass))) = (scheme, &auth)
            && (user.len() > 255 || pass.len() > 255)
        {
            return Err("SOCKS5 username and password are limited to 255 bytes".into());
        }
        Ok(Self {
            scheme,
            host,
            port,
            auth,
        })
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// `host:port` with IPv6 in brackets.
    fn authority(host: &str, port: u16) -> String {
        if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        }
    }

    /// ureq's form. `socks5h` so ureq also leaves DNS to the proxy.
    fn ureq_url(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Socks5 => "socks5h",
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        let enc = |s: &str| {
            percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
        };
        let auth = match &self.auth {
            Some((u, p)) => format!("{}:{}@", enc(u), enc(p)),
            None => String::new(),
        };
        format!(
            "{scheme}://{auth}{}",
            Self::authority(&self.host, self.port)
        )
    }
}

impl fmt::Display for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.scheme {
            Scheme::Socks5 => "socks5",
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        let auth = if self.auth.is_some() { "***@" } else { "" };
        write!(
            f,
            "{scheme}://{auth}{}",
            Self::authority(&self.host, self.port)
        )
    }
}

impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyUrl({self})")
    }
}

/// The SDK pieces a bot needs to go through the proxy.
pub struct Proxy {
    url: Arc<ProxyUrl>,
    http: Arc<dyn HttpClient>,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Proxy({})", self.url)
    }
}

impl Proxy {
    pub fn new(url: ProxyUrl) -> anyhow::Result<Self> {
        let http = Arc::new(http_client(&url)?);
        Ok(Self {
            url: Arc::new(url),
            http,
        })
    }

    pub fn url(&self) -> &ProxyUrl {
        &self.url
    }

    /// The SDK's default WebSocket factory, dialing through the proxy.
    pub fn transport(&self) -> TokioWebSocketTransportFactory {
        TokioWebSocketTransportFactory::new().with_dialer(ProxyDialer::new(self.url.clone()))
    }

    /// Media and version-fetch client, shared by every bot.
    pub fn http_client(&self) -> Arc<dyn HttpClient> {
        self.http.clone()
    }
}

/// The SDK's default agent settings (`ureq-client` `build_agent`) plus the proxy.
fn http_client(url: &ProxyUrl) -> anyhow::Result<UreqHttpClient> {
    let proxy = ureq::Proxy::new(&url.ureq_url())
        .map_err(|e| anyhow!("proxy {url} rejected by HTTP client: {e}"))?;
    let agent: ureq::Agent = ureq::config::Config::builder()
        .input_buffer_size(16 * 1024)
        .output_buffer_size(16 * 1024)
        .max_idle_connections(3)
        .max_idle_connections_per_host(2)
        .timeout_connect(Some(Duration::from_secs(10)))
        .proxy(Some(proxy))
        .build()
        .into();
    Ok(UreqHttpClient::with_agent(agent))
}

/// Opens the TCP (or TLS, for `https`) connection to the proxy and asks it
/// for a tunnel to WhatsApp; the SDK then runs TLS + WebSocket over it.
pub struct ProxyDialer {
    url: Arc<ProxyUrl>,
    tls: Option<Connector>,
}

impl ProxyDialer {
    pub fn new(url: Arc<ProxyUrl>) -> Self {
        let tls = (url.scheme == Scheme::Https).then(default_tls_connector);
        Self { url, tls }
    }
}

#[async_trait]
impl StreamDialer for ProxyDialer {
    async fn dial(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        let p = &self.url;
        let tcp = TcpStream::connect((p.host.as_str(), p.port))
            .await
            .with_context(|| format!("connect to proxy {p}"))?;
        let _ = tcp.set_nodelay(true);
        let auth = p.auth.as_ref().map(|(u, pw)| (u.as_str(), pw.as_str()));
        match p.scheme {
            Scheme::Socks5 => {
                let mut s = tcp;
                socks5_connect(&mut s, host, port, auth)
                    .await
                    .with_context(|| format!("SOCKS5 proxy {p}"))?;
                Ok(Box::new(s))
            }
            Scheme::Http => {
                let mut s = tcp;
                http_connect(&mut s, host, port, auth)
                    .await
                    .with_context(|| format!("HTTP proxy {p}"))?;
                Ok(Box::new(s))
            }
            Scheme::Https => {
                let tls = self.tls.as_ref().expect("https proxy has a TLS connector");
                let mut s = tls
                    .wrap(&p.host, tcp)
                    .await
                    .with_context(|| format!("TLS to proxy {p}"))?;
                http_connect(&mut s, host, port, auth)
                    .await
                    .with_context(|| format!("HTTPS proxy {p}"))?;
                Ok(Box::new(s))
            }
        }
    }
}

/// RFC 1928 `CONNECT` with optional RFC 1929 username/password auth. The
/// target is sent as a domain name unless it is an IP literal.
pub async fn socks5_connect<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    host: &str,
    port: u16,
    auth: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    if auth.is_some() {
        s.write_all(&[5, 2, 0x00, 0x02]).await?;
    } else {
        s.write_all(&[5, 1, 0x00]).await?;
    }
    let mut choice = [0u8; 2];
    s.read_exact(&mut choice).await?;
    if choice[0] != 5 {
        bail!("not a SOCKS5 server");
    }
    match (choice[1], auth) {
        (0x00, _) => {}
        (0x02, Some((user, pass))) => {
            let mut req = vec![1, user.len() as u8];
            req.extend_from_slice(user.as_bytes());
            req.push(pass.len() as u8);
            req.extend_from_slice(pass.as_bytes());
            s.write_all(&req).await?;
            let mut status = [0u8; 2];
            s.read_exact(&mut status).await?;
            if status[1] != 0 {
                bail!("authentication failed");
            }
        }
        (0xFF, _) | (0x02, None) => bail!("no acceptable authentication method"),
        (m, _) => bail!("unsupported authentication method {m:#04x}"),
    }

    let mut req = vec![5, 1, 0];
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        req.push(1);
        req.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = host.parse::<Ipv6Addr>() {
        req.push(4);
        req.extend_from_slice(&ip.octets());
    } else {
        let name = host.as_bytes();
        if name.len() > 255 {
            bail!("target hostname too long");
        }
        req.push(3);
        req.push(name.len() as u8);
        req.extend_from_slice(name);
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;

    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[0] != 5 {
        bail!("malformed reply");
    }
    if head[1] != 0 {
        bail!(
            "connect to {host}:{port} refused: {}",
            socks5_reply(head[1])
        );
    }
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => s.read_u8().await? as usize,
        t => bail!("malformed reply address type {t}"),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await?;
    Ok(())
}

fn socks5_reply(code: u8) -> &'static str {
    match code {
        1 => "general failure",
        2 => "connection not allowed by ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "unknown error",
    }
}

/// Largest proxy response head we accept before giving up.
const MAX_CONNECT_HEAD: usize = 16 * 1024;

/// HTTP `CONNECT` tunnel. Reads the response head byte by byte so no tunnel
/// bytes are consumed.
pub async fn http_connect<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    host: &str,
    port: u16,
    auth: Option<(&str, &str)>,
) -> anyhow::Result<()> {
    let target = ProxyUrl::authority(host, port);
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some((user, pass)) = auth {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;
    s.flush().await?;

    let mut head = Vec::with_capacity(256);
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_CONNECT_HEAD {
            bail!("response header too large");
        }
        let b = s
            .read_u8()
            .await
            .context("connection closed during CONNECT")?;
        head.push(b);
    }
    let text = String::from_utf8_lossy(&head);
    let status_line = text.lines().next().unwrap_or_default().trim();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(c) if (200..300).contains(&c) && status_line.starts_with("HTTP/") => Ok(()),
        _ => bail!("CONNECT to {target} failed: {status_line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn parses_and_redacts() {
        let p = ProxyUrl::parse("socks5://us%40er:p%3Ass@proxy.example.com:1081").unwrap();
        assert_eq!(p.scheme, Scheme::Socks5);
        assert_eq!(p.host, "proxy.example.com");
        assert_eq!(p.port, 1081);
        assert_eq!(p.auth, Some(("us@er".into(), "p:ss".into())));
        assert_eq!(p.to_string(), "socks5://***@proxy.example.com:1081");
        assert!(!format!("{p:?}").contains("p:ss"));
        assert_eq!(
            p.ureq_url(),
            "socks5h://us%40er:p%3Ass@proxy.example.com:1081"
        );

        let h = ProxyUrl::parse("http://[::1]").unwrap();
        assert_eq!((h.scheme, h.port), (Scheme::Http, 80));
        assert_eq!(h.to_string(), "http://[::1]:80");
        assert_eq!(ProxyUrl::parse("https://p").unwrap().port, 443);
        assert_eq!(ProxyUrl::parse("socks5h://p").unwrap().port, 1080);
    }

    #[test]
    fn rejects_bad_urls_without_leaking_them() {
        for bad in [
            "ftp://u:secret@h:1",
            "user:secret@host:1080",
            "socks5://",
            "http://u:secret@:8080",
            "not a url",
        ] {
            let err = ProxyUrl::parse(bad).unwrap_err();
            assert!(!err.contains("secret"), "{bad}: {err}");
        }
    }

    #[test]
    fn builds_http_client_for_every_scheme() {
        for url in ["socks5://u:p@h:1", "http://h:8080", "https://h"] {
            Proxy::new(ProxyUrl::parse(url).unwrap()).unwrap();
        }
    }

    #[tokio::test]
    async fn socks5_with_auth_and_domain() {
        let (mut client, mut server) = duplex(1024);
        let srv = tokio::spawn(async move {
            let mut b = [0u8; 4];
            server.read_exact(&mut b).await.unwrap();
            assert_eq!(b, [5, 2, 0, 2]);
            server.write_all(&[5, 2]).await.unwrap();
            let mut a = [0u8; 2 + 1 + 1 + 2];
            server.read_exact(&mut a).await.unwrap();
            assert_eq!(&a, &[1, 1, b'u', 2, b'p', b'w']);
            server.write_all(&[1, 0]).await.unwrap();
            let mut r = vec![0u8; 4 + 1 + 16 + 2];
            server.read_exact(&mut r).await.unwrap();
            // Bound to a domain, to exercise the variable-length skip.
            server
                .write_all(&[5, 0, 0, 3, 3, b'a', b'b', b'c', 0x01, 0xbb])
                .await
                .unwrap();
            server.write_all(b"TUNNEL").await.unwrap();
            r
        });
        socks5_connect(&mut client, "web.whatsapp.com", 443, Some(("u", "pw")))
            .await
            .unwrap();
        let mut rest = [0u8; 6];
        client.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"TUNNEL");
        let req = srv.await.unwrap();
        assert_eq!(&req[..5], &[5, 1, 0, 3, 16]);
        assert_eq!(&req[5..21], b"web.whatsapp.com");
        assert_eq!(&req[21..], &[0x01, 0xbb]);
    }

    #[tokio::test]
    async fn socks5_reports_refusal_and_bad_auth() {
        let (mut client, mut server) = duplex(1024);
        tokio::spawn(async move {
            let mut b = [0u8; 3];
            server.read_exact(&mut b).await.unwrap();
            server.write_all(&[5, 0]).await.unwrap();
            let mut r = [0u8; 4 + 4 + 2];
            server.read_exact(&mut r).await.unwrap();
            assert_eq!(&r[..8], &[5, 1, 0, 1, 1, 2, 3, 4]);
            server
                .write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let err = socks5_connect(&mut client, "1.2.3.4", 443, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("connection refused"), "{err}");

        let (mut client, mut server) = duplex(1024);
        tokio::spawn(async move {
            let mut b = [0u8; 3];
            server.read_exact(&mut b).await.unwrap();
            server.write_all(&[5, 0xFF]).await.unwrap();
        });
        let err = socks5_connect(&mut client, "h", 1, None).await.unwrap_err();
        assert!(err.to_string().contains("no acceptable"), "{err}");
    }

    #[tokio::test]
    async fn http_connect_sends_auth_and_keeps_tunnel_bytes() {
        let (mut client, mut server) = duplex(4096);
        let srv = tokio::spawn(async move {
            let mut buf = Vec::new();
            while !buf.ends_with(b"\r\n\r\n") {
                buf.push(server.read_u8().await.unwrap());
            }
            server
                .write_all(b"HTTP/1.1 200 Connection established\r\nVia: x\r\n\r\nTUNNEL")
                .await
                .unwrap();
            String::from_utf8(buf).unwrap()
        });
        http_connect(&mut client, "2001:db8::1", 443, Some(("u", "p")))
            .await
            .unwrap();
        let mut rest = [0u8; 6];
        client.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"TUNNEL");
        let req = srv.await.unwrap();
        assert!(
            req.starts_with("CONNECT [2001:db8::1]:443 HTTP/1.1\r\n"),
            "{req}"
        );
        assert!(req.contains("Proxy-Authorization: Basic dTpw\r\n"), "{req}");
    }

    #[tokio::test]
    async fn http_connect_rejects_non_2xx() {
        let (mut client, mut server) = duplex(4096);
        tokio::spawn(async move {
            let mut buf = Vec::new();
            while !buf.ends_with(b"\r\n\r\n") {
                buf.push(server.read_u8().await.unwrap());
            }
            server
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let err = http_connect(&mut client, "h", 443, None).await.unwrap_err();
        assert!(err.to_string().contains("407"), "{err}");
    }

    /// End to end through a real socket: dialer → fake HTTP proxy.
    #[tokio::test]
    async fn dialer_tunnels_through_http_proxy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            while !buf.ends_with(b"\r\n\r\n") {
                buf.push(sock.read_u8().await.unwrap());
            }
            assert!(buf.starts_with(b"CONNECT web.whatsapp.com:443 "));
            sock.write_all(b"HTTP/1.0 200 OK\r\n\r\nhello")
                .await
                .unwrap();
        });
        let url = ProxyUrl::parse(&format!("http://{addr}")).unwrap();
        let dialer = ProxyDialer::new(Arc::new(url));
        let mut stream = dialer.dial("web.whatsapp.com", 443).await.unwrap();
        let mut got = [0u8; 5];
        stream.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
    }
}
