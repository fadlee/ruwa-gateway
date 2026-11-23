//! Maps WhatsApp events to GOWA webhook envelopes (docs/SPEC.md §5.2).
//!
//! Inputs are small views (`IncomingMessage`, `ReceiptView`) built by the
//! device layer, so payload building is testable without a live client.
//! Ports GOWA `infrastructure/whatsapp/event_message.go`, `event_receipt.go`
//! and `event_device.go`.

use std::{collections::HashSet, future::Future, sync::Arc};

use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::wa::{self, Jid, message as msgx, proto};

pub const MESSAGE: &str = "message";
pub const MESSAGE_REACTION: &str = "message.reaction";
pub const MESSAGE_REVOKED: &str = "message.revoked";
pub const MESSAGE_EDITED: &str = "message.edited";
pub const MESSAGE_ACK: &str = "message.ack";
pub const DEVICE_LOGGED_OUT: &str = "device.logged_out";

/// Async lookups the payload builder needs from the live session.
pub trait Resolver: Send + Sync {
    /// Phone-number JID for a LID, when the mapping is known.
    fn pn_for_lid(&self, lid: &Jid) -> impl Future<Output = Option<Jid>> + Send;

    /// GOWA `sender_display_name`. `push_name` is the live push name, if any.
    fn display_name(
        &self,
        sender: &Jid,
        is_from_me: bool,
        push_name: &str,
    ) -> impl Future<Output = String> + Send;
}

/// One decrypted inbound (or self-synced) message.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub id: String,
    pub chat: Jid,
    pub sender: Jid,
    pub is_from_me: bool,
    pub push_name: String,
    pub timestamp: i64,
    pub verified_name: Option<String>,
    pub verified_level: Option<String>,
    pub is_business: bool,
    pub message: Arc<proto::Message>,
}

/// One delivery/read receipt.
#[derive(Debug, Clone)]
pub struct ReceiptView {
    pub chat: Jid,
    pub sender: Jid,
    pub is_from_me: bool,
    pub message_ids: Vec<String>,
    /// Wire receipt type (`delivery`, `read`, `read-self`, `played`, ...).
    pub receipt_type: String,
    pub timestamp: i64,
}

pub fn rfc3339(unix_secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(unix_secs, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `{event, device_id, payload}` (GOWA `forwardMessageToWebhook`).
pub fn envelope(event: &str, device_id: &str, payload: Map<String, Value>) -> Map<String, Value> {
    let mut env = Map::new();
    env.insert("event".into(), event.into());
    env.insert("device_id".into(), device_id.into());
    env.insert("payload".into(), Value::Object(payload));
    env
}

async fn normalize<R: Resolver>(r: &R, jid: &Jid) -> Jid {
    if jid.is_lid()
        && let Some(pn) = r.pn_for_lid(jid).await
    {
        return pn.to_non_ad();
    }
    jid.to_non_ad()
}

/// `chat_id` / `chat_lid` / `from` / `from_lid` (GOWA `buildFromFields`).
async fn from_fields<R: Resolver>(r: &R, chat: &Jid, sender: &Jid, p: &mut Map<String, Value>) {
    let chat = chat.to_non_ad();
    if chat.is_lid() {
        p.insert("chat_lid".into(), chat.to_string().into());
    }
    p.insert(
        "chat_id".into(),
        normalize(r, &chat).await.to_string().into(),
    );
    if sender.is_lid() {
        p.insert("from_lid".into(), wa::jid::non_ad_string(sender).into());
    }
    p.insert("from".into(), normalize(r, sender).await.to_string().into());
}

/// Build `(event_name, payload)` for a message (GOWA `buildEventPayload`).
pub async fn message_payload<R: Resolver>(
    r: &R,
    m: &IncomingMessage,
) -> (&'static str, Map<String, Value>) {
    let mut p = Map::new();
    let msg = msgx::unwrap(&m.message);

    p.insert("id".into(), m.id.clone().into());
    p.insert("timestamp".into(), rfc3339(m.timestamp).into());
    p.insert("is_from_me".into(), m.is_from_me.into());
    from_fields(r, &m.chat, &m.sender, &mut p).await;
    p.insert(
        "sender_display_name".into(),
        r.display_name(&m.sender, m.is_from_me, &m.push_name)
            .await
            .into(),
    );
    if !m.push_name.is_empty() {
        p.insert("from_name".into(), m.push_name.clone().into());
    }
    if !m.is_from_me && m.is_business {
        p.insert("is_business".into(), true.into());
        if let Some(name) = m.verified_name.as_deref().filter(|n| !n.is_empty()) {
            p.insert("verified_name".into(), name.into());
        }
        if let Some(level) = m.verified_level.as_deref().filter(|l| !l.is_empty()) {
            p.insert("verified_level".into(), level.into());
        }
    }

    if let Some(pm) = msg.protocol_message.as_option() {
        use proto::message::protocol_message::Type;
        match pm.r#type {
            Some(Type::Revoke) => {
                if let Some(key) = pm.key.as_option() {
                    p.insert(
                        "revoked_message_id".into(),
                        key.id.clone().unwrap_or_default().into(),
                    );
                    p.insert(
                        "revoked_from_me".into(),
                        key.from_me.unwrap_or(false).into(),
                    );
                    if let Some(chat) = key.remote_jid.as_deref().filter(|c| !c.is_empty()) {
                        p.insert("revoked_chat".into(), chat.into());
                    }
                }
                return (MESSAGE_REVOKED, p);
            }
            Some(Type::MessageEdit) => {
                if let Some(key) = pm.key.as_option() {
                    p.insert(
                        "original_message_id".into(),
                        key.id.clone().unwrap_or_default().into(),
                    );
                }
                if let Some(edited) = pm.edited_message.as_option()
                    && let Some(body) = msgx::text(edited)
                {
                    p.insert("body".into(), body.into());
                }
                return (MESSAGE_EDITED, p);
            }
            _ => {}
        }
    }

    if let Some(reaction) = msg.reaction_message.as_option() {
        p.insert(
            "reaction".into(),
            reaction.text.clone().unwrap_or_default().into(),
        );
        if let Some(key) = reaction.key.as_option() {
            p.insert(
                "reacted_message_id".into(),
                key.id.clone().unwrap_or_default().into(),
            );
        }
        return (MESSAGE_REACTION, p);
    }

    // Body: text, else media caption (GOWA `buildMessageBody`).
    if let Some(body) = msgx::text(msg).or_else(|| msgx::caption(msg)) {
        p.insert("body".into(), replace_lid_mentions(r, body).await.into());
    }
    if let Some(ci) = msgx::context_info(msg) {
        if let Some(id) = ci.stanza_id.as_deref().filter(|s| !s.is_empty()) {
            p.insert("replied_to_id".into(), id.into());
        }
        if let Some(quoted) = ci.quoted_message.as_option().and_then(msgx::any_text) {
            p.insert("quoted_body".into(), quoted.into());
        }
        if let Some(sender) = ci
            .participant
            .as_deref()
            .and_then(|s| wa::jid::parse_jid(s).ok())
        {
            if sender.is_lid() {
                p.insert(
                    "replied_to_sender_lid".into(),
                    wa::jid::non_ad_string(&sender).into(),
                );
            }
            p.insert(
                "replied_to_sender".into(),
                normalize(r, &sender).await.to_string().into(),
            );
        }
        mention_fields(r, &ci.mentioned_jid, &mut p).await;
    }
    if msgx::is_view_once(&m.message) {
        p.insert("view_once".into(), true.into());
    }
    if msgx::is_forwarded(&m.message) {
        p.insert("forwarded".into(), true.into());
    }
    (MESSAGE, p)
}

/// `mentions` / `mentions_lid` (GOWA `buildMentionFields`).
async fn mention_fields<R: Resolver>(r: &R, raw: &[String], p: &mut Map<String, Value>) {
    let (mut mentions, mut lids) = (Vec::new(), Vec::new());
    let (mut seen, mut seen_lid) = (HashSet::new(), HashSet::new());
    for jid in raw
        .iter()
        .filter(|j| !j.is_empty())
        .filter_map(|j| j.parse::<Jid>().ok())
    {
        if jid.is_lid() {
            let lid = wa::jid::non_ad_string(&jid);
            if seen_lid.insert(lid.clone()) {
                lids.push(lid);
            }
        }
        let resolved = normalize(r, &jid).await.to_string();
        if seen.insert(resolved.clone()) {
            mentions.push(resolved);
        }
    }
    if !mentions.is_empty() {
        p.insert("mentions".into(), json!(mentions));
    }
    if !lids.is_empty() {
        p.insert("mentions_lid".into(), json!(lids));
    }
}

/// Rewrite `@<lid>` tags to `@<phone>` when the mapping is known
/// (GOWA `replaceLIDMentions`, regex `\B@\w+`).
async fn replace_lid_mentions<R: Resolver>(r: &R, text: &str) -> String {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"@\w+").unwrap());
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in RE.find_iter(text) {
        // `\B@`: the `@` must not follow a word character.
        let preceded_by_word = text[..m.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&text[last..m.start()]);
        let tag = m.as_str();
        let replacement = if preceded_by_word {
            None
        } else {
            let lid = Jid::lid(&tag[1..]);
            r.pn_for_lid(&lid).await.map(|pn| format!("@{}", pn.user))
        };
        out.push_str(replacement.as_deref().unwrap_or(tag));
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// GOWA `shouldForwardReceipt` (with `WHATSAPP_WEBHOOK_ALL_DEVICE_RECEIPTS` off).
pub fn should_forward_receipt(r: &ReceiptView) -> bool {
    if r.sender.device == 0 {
        return true;
    }
    match r.receipt_type.as_str() {
        "read" | "played" => true,
        "read-self" => r.is_from_me,
        _ => false,
    }
}

fn receipt_description(receipt_type: &str) -> &'static str {
    match receipt_type {
        "delivered" => {
            "means the message was delivered to the device (but the user might not have noticed)."
        }
        "sender" => "sent by your other devices when a message you sent is delivered to them.",
        "retry" => "the message was delivered to the device, but decrypting the message failed.",
        "read" => "the user opened the chat and saw the message.",
        "read-self" => {
            "the current user read a message from a different device, and has read receipts disabled in privacy settings."
        }
        "played" => {
            "This is dispatched for both incoming and outgoing messages when played. If the current user opened the media,\n\tit means the media should be removed from all devices. If a recipient opened the media, it's just a notification\n\tfor the sender that the media was viewed."
        }
        "played-self" => {
            "probably means the current user opened a view-once media message from a different device,\n\tand has read receipts disabled in privacy settings."
        }
        _ => "unknown receipt type",
    }
}

/// Full `message.ack` envelope (GOWA `createReceiptPayload`). Unlike message
/// events it carries a top-level `timestamp`.
pub async fn receipt_envelope<R: Resolver>(
    r: &R,
    rc: &ReceiptView,
    device_id: &str,
) -> Map<String, Value> {
    let mut p = Map::new();
    if !rc.message_ids.is_empty() {
        p.insert("ids".into(), json!(rc.message_ids));
    }
    from_fields(r, &rc.chat, &rc.sender, &mut p).await;
    p.insert(
        "sender_display_name".into(),
        r.display_name(&rc.sender, false, "").await.into(),
    );
    let receipt_type = match rc.receipt_type.as_str() {
        "" | "delivery" => "delivered",
        other => other,
    };
    p.insert("receipt_type".into(), receipt_type.into());
    p.insert(
        "receipt_type_description".into(),
        receipt_description(receipt_type).into(),
    );

    let mut env = Map::new();
    env.insert("event".into(), MESSAGE_ACK.into());
    env.insert("timestamp".into(), rfc3339(rc.timestamp).into());
    if !device_id.is_empty() {
        env.insert("device_id".into(), device_id.into());
    }
    env.insert("payload".into(), Value::Object(p));
    env
}

/// whatsmeow's `ConnectFailureReason` descriptions, which GOWA forwards.
pub fn connect_failure_reason(code: i32) -> &'static str {
    match code {
        401 => "logged out from another device",
        402 => "account temporarily banned",
        403 => "primary device was logged out",
        406 => "logged out for unknown reason",
        405 => "client is out of date",
        409 => "client user agent was rejected",
        413 => "messenger crypto auth token has expired",
        414 => "messenger crypto auth token is invalid",
        _ => "unknown error",
    }
}

/// `device.*` envelope (GOWA `forwardDeviceEvent`): always carries
/// `session_id` and `timestamp`; `device_id` only when the JID is known.
pub fn device_envelope(
    event: &str,
    session_id: &str,
    device_jid: Option<&str>,
    payload: Value,
    now: i64,
) -> Map<String, Value> {
    let mut env = Map::new();
    env.insert("event".into(), event.into());
    env.insert("payload".into(), payload);
    env.insert("timestamp".into(), rfc3339(now).into());
    env.insert("session_id".into(), session_id.into());
    if let Some(jid) = device_jid.filter(|j| !j.is_empty()) {
        env.insert("device_id".into(), jid.into());
    }
    env
}

pub fn logged_out_payload(code: i32, on_connect: bool) -> Value {
    json!({
        "code": code,
        "reason": connect_failure_reason(code),
        "on_connect": on_connect,
    })
}

#[cfg(test)]
mod tests;
