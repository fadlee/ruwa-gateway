//! Webhook delivery, mirroring GOWA `infrastructure/whatsapp/webhook.go` and
//! `webhook_forward.go`:
//! - body is the JSON envelope; keys are sorted (Go marshals maps sorted, and
//!   serde_json's default `Map` is a `BTreeMap`),
//! - `X-Hub-Signature-256: sha256=<hex HMAC-SHA256(body, secret)>`,
//! - 5 attempts per URL, backoff 1s doubling, 10s request timeout,
//! - event allow-list (`WHATSAPP_WEBHOOK_EVENTS`, case-insensitive),
//! - JID ignore list (`WHATSAPP_WEBHOOK_IGNORE_JIDS`, `@suffix` wildcards).

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;

use crate::storage::registry::DeviceWebhook;

pub const SIGNATURE_HEADER: &str = "X-Hub-Signature-256";
const MAX_ATTEMPTS: u32 = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Payload keys checked against the ignore list (GOWA `payloadIdentityKeys`).
const IDENTITY_KEYS: [&str; 6] = [
    "chat_id",
    "from",
    "chat_lid",
    "from_lid",
    "sender_alt",
    "recipient_alt",
];

#[derive(Debug, Clone, Default)]
pub struct WebhookConfig {
    pub urls: Vec<String>,
    pub secret: String,
    pub events: Vec<String>,
    pub ignore_jids: Vec<String>,
    pub insecure_skip_verify: bool,
}

/// Effective delivery target for one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub urls: Vec<String>,
    pub secret: String,
    pub events: Vec<String>,
    pub insecure_skip_verify: bool,
}

#[derive(Clone)]
pub struct Dispatcher {
    config: WebhookConfig,
    http: reqwest::Client,
    http_insecure: reqwest::Client,
}

impl Dispatcher {
    pub fn new(config: WebhookConfig) -> anyhow::Result<Self> {
        let build = |insecure| {
            reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .danger_accept_invalid_certs(insecure)
                .build()
        };
        Ok(Self {
            config,
            http: build(false)?,
            http_insecure: build(true)?,
        })
    }

    /// GOWA `forwardPayloadWithWebhookConfig`: a device override applies only
    /// when it has a URL; its secret and event list fall back to the global
    /// ones when empty.
    pub fn target(&self, device: &DeviceWebhook) -> Target {
        let Some(url) = device
            .webhook_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
        else {
            return Target {
                urls: self.config.urls.clone(),
                secret: self.config.secret.clone(),
                events: self.config.events.clone(),
                insecure_skip_verify: self.config.insecure_skip_verify,
            };
        };
        let secret = if device.webhook_secret.is_empty() {
            self.config.secret.clone()
        } else {
            device.webhook_secret.clone()
        };
        let events = if device.webhook_events.trim().is_empty() {
            self.config.events.clone()
        } else {
            device
                .webhook_events
                .split(',')
                .map(|e| e.trim().to_owned())
                .collect()
        };
        Target {
            urls: vec![url.trim().to_owned()],
            secret,
            events,
            insecure_skip_verify: device.webhook_insecure_skip_verify,
        }
    }

    /// Filter, sign, and deliver `envelope` in the background.
    /// `envelope` must already carry `event`, `device_id`, `payload`.
    pub fn send(&self, event: &str, envelope: Map<String, Value>, device: &DeviceWebhook) {
        let target = self.target(device);
        if target.urls.is_empty()
            || !event_allowed(&target.events, event)
            || jid_ignored(&self.config.ignore_jids, &envelope)
        {
            tracing::debug!(event, "webhook skipped by filter");
            return;
        }
        let body = match serde_json::to_vec(&envelope) {
            Ok(body) => body,
            Err(error) => {
                tracing::error!(%error, "webhook marshal failed");
                return;
            }
        };
        let signature = sign(&body, &target.secret);
        let http = if target.insecure_skip_verify {
            &self.http_insecure
        } else {
            &self.http
        };
        for url in target.urls {
            let http = http.clone();
            let body = body.clone();
            let signature = signature.clone();
            let event = event.to_owned();
            tokio::spawn(async move {
                if let Err(error) = deliver(&http, &url, body, &signature).await {
                    tracing::warn!(%url, event, %error, "webhook delivery failed");
                }
            });
        }
    }
}

async fn deliver(
    http: &reqwest::Client,
    url: &str,
    body: Vec<u8>,
    signature: &str,
) -> anyhow::Result<()> {
    let mut sleep = Duration::from_secs(1);
    let mut last_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let result = http
            .post(url)
            .header("Content-Type", "application/json")
            .header(SIGNATURE_HEADER, format!("sha256={signature}"))
            .body(body.clone())
            .send()
            .await;
        match result {
            Ok(res) if res.status().is_success() => {
                tracing::info!(url, attempt, "webhook delivered");
                return Ok(());
            }
            Ok(res) => {
                last_error = Some(anyhow::anyhow!(
                    "webhook returned status {}",
                    res.status().as_u16()
                ))
            }
            Err(error) => last_error = Some(error.into()),
        }
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(sleep).await;
            sleep *= 2;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("webhook failed")))
}

/// Hex HMAC-SHA256 of `body` with `secret`.
pub fn sign(body: &[u8], secret: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Empty allow-list means every event.
pub fn event_allowed(allowed: &[String], event: &str) -> bool {
    let mut list = allowed
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .peekable();
    list.peek().is_none() || list.any(|e| e.eq_ignore_ascii_case(event))
}

/// GOWA `MatchesIgnoredJID`: exact match, or suffix match for `@...` patterns.
pub fn matches_ignored_jid(jid: &str, ignore: &[String]) -> bool {
    if jid.is_empty() {
        return false;
    }
    ignore
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .any(|p| {
            if p.starts_with('@') {
                jid.ends_with(p)
            } else {
                jid == p
            }
        })
}

fn jid_ignored(ignore: &[String], envelope: &Map<String, Value>) -> bool {
    if ignore.is_empty() {
        return false;
    }
    let Some(Value::Object(payload)) = envelope.get("payload") else {
        return false;
    };
    IDENTITY_KEYS.iter().any(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|jid| matches_ignored_jid(jid, ignore))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn signature_matches_known_vector() {
        // echo -n '{"a":1}' | openssl dgst -sha256 -hmac secret
        assert_eq!(
            sign(br#"{"a":1}"#, "secret"),
            "aa9e2e3575f5d7098b6caccd790888c36d5fdb63342a73bada2d6a51747a8494"
        );
    }

    fn dispatcher() -> Dispatcher {
        Dispatcher::new(WebhookConfig {
            urls: vec!["https://global".into()],
            secret: "g".into(),
            events: vec!["message".into()],
            ignore_jids: vec![],
            insecure_skip_verify: false,
        })
        .unwrap()
    }

    #[test]
    fn device_without_url_uses_global() {
        let d = dispatcher();
        let hook = DeviceWebhook {
            webhook_secret: "dev".into(),
            ..Default::default()
        };
        assert_eq!(d.target(&hook).urls, vec!["https://global"]);
        assert_eq!(d.target(&hook).secret, "g");
    }

    #[test]
    fn device_url_overrides_with_fallbacks() {
        let d = dispatcher();
        let mut hook = DeviceWebhook {
            webhook_url: Some("https://dev".into()),
            ..Default::default()
        };
        let t = d.target(&hook);
        assert_eq!(
            (t.urls, t.secret, t.events),
            (
                vec!["https://dev".to_owned()],
                "g".to_owned(),
                vec!["message".to_owned()]
            )
        );
        hook.webhook_secret = "s".into();
        hook.webhook_events = "message.ack, group.info".into();
        hook.webhook_insecure_skip_verify = true;
        let t = d.target(&hook);
        assert_eq!(t.secret, "s");
        assert_eq!(t.events, vec!["message.ack", "group.info"]);
        assert!(t.insecure_skip_verify);
    }

    #[test]
    fn empty_allow_list_allows_all() {
        assert!(event_allowed(&[], "message"));
        assert!(event_allowed(&[" ".into()], "message"));
    }

    #[test]
    fn allow_list_is_case_insensitive() {
        let list = vec!["Message".to_owned(), " message.ack ".to_owned()];
        assert!(event_allowed(&list, "message"));
        assert!(event_allowed(&list, "message.ack"));
        assert!(!event_allowed(&list, "group.info"));
    }

    #[test]
    fn ignore_wildcard_and_exact() {
        let ignore = vec!["@g.us".to_owned(), "628@s.whatsapp.net".to_owned()];
        assert!(matches_ignored_jid("123@g.us", &ignore));
        assert!(matches_ignored_jid("628@s.whatsapp.net", &ignore));
        assert!(!matches_ignored_jid("629@s.whatsapp.net", &ignore));
        assert!(!matches_ignored_jid("", &ignore));
    }

    #[test]
    fn ignore_checks_nested_payload() {
        let env = json!({"event": "message", "payload": {"chat_id": "1@g.us"}});
        let Value::Object(map) = env else {
            unreachable!()
        };
        assert!(jid_ignored(&["@g.us".into()], &map));
        assert!(!jid_ignored(&["@lid".into()], &map));
    }
}
