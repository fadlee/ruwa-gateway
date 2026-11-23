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

#[derive(Clone)]
pub struct Dispatcher {
    config: WebhookConfig,
    http: reqwest::Client,
}

impl Dispatcher {
    pub fn new(config: WebhookConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .danger_accept_invalid_certs(config.insecure_skip_verify)
            .build()?;
        Ok(Self { config, http })
    }

    pub fn is_enabled(&self) -> bool {
        !self.config.urls.is_empty()
    }

    /// Filter, sign, and deliver `envelope` in the background.
    /// `envelope` must already carry `event`, `device_id`, `payload`.
    pub fn send(&self, event: &str, envelope: Map<String, Value>) {
        if !self.should_send(event, &envelope) {
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
        let signature = sign(&body, &self.config.secret);
        for url in self.config.urls.clone() {
            let http = self.http.clone();
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

    fn should_send(&self, event: &str, envelope: &Map<String, Value>) -> bool {
        self.is_enabled()
            && event_allowed(&self.config.events, event)
            && !jid_ignored(&self.config.ignore_jids, envelope)
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
