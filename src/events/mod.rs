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

    /// `WHATSAPP_AUTO_DOWNLOAD_MEDIA`.
    fn auto_download(&self) -> bool;

    /// Download and save incoming media; returns the stored path
    /// (`statics/media/<unix>-<uuid><ext>`).
    fn save_media<'a>(
        &'a self,
        media: MediaRef<'a>,
    ) -> impl Future<Output = anyhow::Result<String>> + Send + 'a;

    /// Whether `user` (or its alternate address) currently administers
    /// `group`. A failed lookup is not an admin.
    fn is_group_admin(
        &self,
        group: &Jid,
        user: &Jid,
        alt: Option<&Jid>,
    ) -> impl Future<Output = bool> + Send {
        let _ = (group, user, alt);
        async { false }
    }
}

/// Downloadable media inside a message.
#[derive(Debug, Clone, Copy)]
pub enum MediaRef<'a> {
    Image(&'a proto::message::ImageMessage),
    Video(&'a proto::message::VideoMessage),
    Audio(&'a proto::message::AudioMessage),
    Document(&'a proto::message::DocumentMessage),
    Sticker(&'a proto::message::StickerMessage),
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
    /// Alternate address of the sender (PN for a LID sender and vice versa).
    pub sender_alt: Option<Jid>,
    /// Stanza `edit="8"`: a group admin deleting someone else's message.
    pub admin_revoke: bool,
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
    media_fields(r, msg, &mut p).await;
    other_types(msg, &mut p);
    (MESSAGE, p)
}

/// GOWA `buildMediaFields`. With auto-download: a path string, or
/// `{path, caption}` when there is a caption (audio/sticker: path only).
/// Without: `{url, ...}`. A failed download leaves the key out.
async fn media_fields<R: Resolver>(r: &R, msg: &proto::Message, p: &mut Map<String, Value>) {
    async fn put<R: Resolver>(
        r: &R,
        p: &mut Map<String, Value>,
        key: &str,
        media: MediaRef<'_>,
        caption: Option<&str>,
        fallback: Value,
    ) {
        if !r.auto_download() {
            p.insert(key.into(), fallback);
            return;
        }
        match r.save_media(media).await {
            Ok(path) => {
                let value = match caption.filter(|c| !c.is_empty()) {
                    Some(c) => json!({ "path": path, "caption": c }),
                    None => path.into(),
                };
                p.insert(key.into(), value);
            }
            Err(error) => tracing::error!(%error, "failed to download {key}"),
        }
    }

    if let Some(a) = msg.audio_message.as_option() {
        let fallback = json!({ "url": a.url.clone().unwrap_or_default() });
        put(r, p, "audio", MediaRef::Audio(a), None, fallback).await;
    }
    if let Some(d) = msg.document_message.as_option() {
        let fallback = json!({
            "url": d.url.clone().unwrap_or_default(),
            "filename": d.file_name.clone().unwrap_or_default(),
        });
        put(
            r,
            p,
            "document",
            MediaRef::Document(d),
            d.caption.as_deref(),
            fallback,
        )
        .await;
    }
    if let Some(i) = msg.image_message.as_option() {
        let fallback = json!({
            "url": i.url.clone().unwrap_or_default(),
            "caption": i.caption.clone().unwrap_or_default(),
        });
        put(
            r,
            p,
            "image",
            MediaRef::Image(i),
            i.caption.as_deref(),
            fallback,
        )
        .await;
    }
    if let Some(s) = msg.sticker_message.as_option() {
        let fallback = json!({ "url": s.url.clone().unwrap_or_default() });
        put(r, p, "sticker", MediaRef::Sticker(s), None, fallback).await;
    }
    if let Some(v) = msg.video_message.as_option() {
        let fallback = json!({
            "url": v.url.clone().unwrap_or_default(),
            "caption": v.caption.clone().unwrap_or_default(),
        });
        put(
            r,
            p,
            "video",
            MediaRef::Video(v),
            v.caption.as_deref(),
            fallback,
        )
        .await;
    }
    if let Some(v) = msg.ptv_message.as_option() {
        let fallback = json!({
            "url": v.url.clone().unwrap_or_default(),
            "caption": v.caption.clone().unwrap_or_default(),
        });
        put(
            r,
            p,
            "video_note",
            MediaRef::Video(v),
            v.caption.as_deref(),
            fallback,
        )
        .await;
    }
}

/// Phone number from the first `TEL` line of a vCard (GOWA `ExtractPhoneFromVCard`).
fn vcard_phone(vcard: &str) -> Option<String> {
    vcard
        .lines()
        .find(|l| l.to_ascii_uppercase().starts_with("TEL"))
        .and_then(|l| l.rsplit_once(':'))
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn contact_json(c: &proto::message::ContactMessage) -> Value {
    let vcard = c.vcard.clone().unwrap_or_default();
    let mut v = json!({
        "displayName": c.display_name.clone().unwrap_or_default(),
        "vcard": vcard,
    });
    if let Some(phone) = vcard_phone(&vcard) {
        v["phone_number"] = phone.into();
    }
    v
}

/// Go `encoding/json` of a proto struct: camelCase keys, empty fields omitted.
fn insert_some<T: serde::Serialize>(v: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        let value = json!(value);
        let empty = matches!(&value, Value::String(s) if s.is_empty())
            || value == json!(0)
            || value == json!(0.0)
            || value == json!(false);
        if !empty {
            v.insert(key.into(), value);
        }
    }
}

fn b64(bytes: &Option<Vec<u8>>) -> Option<String> {
    use base64::Engine;
    bytes
        .as_ref()
        .filter(|b| !b.is_empty())
        .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
}

/// GOWA `buildOtherMessageTypes` (contact, contacts array, locations).
fn other_types(msg: &proto::Message, p: &mut Map<String, Value>) {
    if let Some(c) = msg.contact_message.as_option() {
        p.insert("contact".into(), contact_json(c));
    }
    if let Some(arr) = msg.contacts_array_message.as_option() {
        let list: Vec<Value> = arr.contacts.iter().map(contact_json).collect();
        p.insert("contacts_array".into(), Value::Array(list));
    }
    if let Some(l) = msg.location_message.as_option() {
        let mut v = Map::new();
        insert_some(&mut v, "degreesLatitude", l.degrees_latitude);
        insert_some(&mut v, "degreesLongitude", l.degrees_longitude);
        insert_some(&mut v, "name", l.name.clone());
        insert_some(&mut v, "address", l.address.clone());
        insert_some(&mut v, "URL", l.url.clone());
        insert_some(&mut v, "isLive", l.is_live);
        insert_some(&mut v, "accuracyInMeters", l.accuracy_in_meters);
        insert_some(&mut v, "comment", l.comment.clone());
        insert_some(&mut v, "JPEGThumbnail", b64(&l.jpeg_thumbnail));
        p.insert("location".into(), Value::Object(v));
    }
    if let Some(l) = msg.live_location_message.as_option() {
        let mut v = Map::new();
        insert_some(&mut v, "degreesLatitude", l.degrees_latitude);
        insert_some(&mut v, "degreesLongitude", l.degrees_longitude);
        insert_some(&mut v, "accuracyInMeters", l.accuracy_in_meters);
        insert_some(&mut v, "speedInMps", l.speed_in_mps);
        insert_some(
            &mut v,
            "degreesClockwiseFromMagneticNorth",
            l.degrees_clockwise_from_magnetic_north,
        );
        insert_some(&mut v, "caption", l.caption.clone());
        insert_some(&mut v, "sequenceNumber", l.sequence_number);
        insert_some(&mut v, "timeOffset", l.time_offset);
        insert_some(&mut v, "JPEGThumbnail", b64(&l.jpeg_thumbnail));
        p.insert("live_location".into(), Value::Object(v));
    }
}

pub const CHAT_PRESENCE: &str = "chat_presence";
pub const DEVICE_TEMPORARY_BAN: &str = "device.temporary_ban";
pub const MESSAGE_DELETED: &str = "message.deleted";
pub const PRESENCE: &str = "presence";
pub const CALL_OFFER: &str = "call.offer";
pub const GROUP_PARTICIPANTS: &str = "group.participants";
pub const GROUP_INFO: &str = "group.info";
pub const GROUP_JOINED: &str = "group.joined";

/// Typing/recording state (GOWA `createChatPresencePayload`).
#[derive(Debug, Clone)]
pub struct ChatPresenceView {
    pub chat: Jid,
    pub sender: Jid,
    pub is_group: bool,
    /// `composing` or `paused`.
    pub state: &'static str,
    /// `""` or `"audio"`.
    pub media: &'static str,
}

pub async fn chat_presence_envelope<R: Resolver>(
    r: &R,
    c: &ChatPresenceView,
    device_id: &str,
    now: i64,
) -> Map<String, Value> {
    let mut p = Map::new();
    if c.sender.is_lid() {
        p.insert("from_lid".into(), wa::jid::non_ad_string(&c.sender).into());
    }
    p.insert(
        "from".into(),
        normalize(r, &c.sender).await.to_string().into(),
    );
    p.insert(
        "sender_display_name".into(),
        r.display_name(&c.sender, false, "").await.into(),
    );
    p.insert("chat_id".into(), wa::jid::non_ad_string(&c.chat).into());
    p.insert("state".into(), c.state.into());
    p.insert("media".into(), c.media.into());
    p.insert("is_group".into(), c.is_group.into());

    let mut env = Map::new();
    env.insert("event".into(), CHAT_PRESENCE.into());
    env.insert("timestamp".into(), rfc3339(now).into());
    if !device_id.is_empty() {
        env.insert("device_id".into(), device_id.into());
    }
    env.insert("payload".into(), Value::Object(p));
    env
}

/// whatsmeow `TempBanReason` descriptions.
pub fn temp_ban_reason(code: i32) -> &'static str {
    match code {
        101 => "you sent too many messages to people who don't have you in their address books",
        102 => "too many people blocked you",
        103 => "you created too many groups with people who don't have you in their address books",
        104 => "you sent the same message to too many people",
        106 => "you sent too many messages to a broadcast list",
        _ => "you may have violated the terms of service (unknown error)",
    }
}

/// GOWA `temporaryBanPayload`.
pub fn temporary_ban_payload(code: i32, expire_secs: i64, now: i64) -> Value {
    let mut v = json!({
        "code": code,
        "reason": temp_ban_reason(code),
        "expires_in_seconds": expire_secs,
    });
    if expire_secs > 0 {
        v["expires_at"] = rfc3339(now + expire_secs).into();
    }
    v
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

/// A message deleted "for me", with the stored copy that was removed.
#[derive(Debug, Clone)]
pub struct DeletedView {
    pub id: String,
    /// Who the event names (participant, else the account, else the chat).
    pub from: Jid,
    /// The chat as the event named it (may be a LID).
    pub event_chat: Jid,
    pub chat_id: String,
    pub content: String,
    pub sender: String,
    pub timestamp: i64,
    pub was_from_me: bool,
    pub media_type: String,
    pub filename: String,
}

/// `message.deleted` (GOWA `createDeletePayload`). Unlike most events it has
/// no top-level `timestamp`; that lives in the payload.
pub async fn message_deleted_envelope<R: Resolver>(
    r: &R,
    d: &DeletedView,
    device_id: &str,
    now: i64,
) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("deleted_message_id".into(), d.id.clone().into());
    p.insert("timestamp".into(), rfc3339(now).into());
    if d.from.is_lid() {
        p.insert("from_lid".into(), wa::jid::non_ad_string(&d.from).into());
    }
    p.insert(
        "from".into(),
        normalize(r, &d.from).await.to_string().into(),
    );
    p.insert(
        "sender_display_name".into(),
        r.display_name(&d.from, false, "").await.into(),
    );
    if d.event_chat.is_lid() {
        p.insert(
            "chat_lid".into(),
            wa::jid::non_ad_string(&d.event_chat).into(),
        );
    }
    p.insert("chat_id".into(), d.chat_id.clone().into());
    p.insert("original_content".into(), d.content.clone().into());
    p.insert("original_sender".into(), d.sender.clone().into());
    p.insert("original_timestamp".into(), rfc3339(d.timestamp).into());
    p.insert("was_from_me".into(), d.was_from_me.into());
    if !d.media_type.is_empty() {
        p.insert("original_media_type".into(), d.media_type.clone().into());
        p.insert("original_filename".into(), d.filename.clone().into());
    }
    envelope(MESSAGE_DELETED, device_id, p)
}

/// Online/offline of a subscribed contact (GOWA `createPresencePayload`).
/// `last_seen` is omitted when unknown (hidden by the contact).
pub async fn presence_envelope<R: Resolver>(
    r: &R,
    from: &Jid,
    available: bool,
    last_seen: Option<i64>,
    device_id: &str,
    now: i64,
) -> Map<String, Value> {
    let mut p = Map::new();
    if from.is_lid() {
        p.insert("from_lid".into(), wa::jid::non_ad_string(from).into());
    }
    p.insert("from".into(), normalize(r, from).await.to_string().into());
    p.insert(
        "state".into(),
        if available {
            "available"
        } else {
            "unavailable"
        }
        .into(),
    );
    if let Some(seen) = last_seen {
        p.insert("last_seen".into(), rfc3339(seen).into());
    }
    group_envelope(PRESENCE, device_id, now, Value::Object(p))
}

/// Incoming call (GOWA `createCallOfferPayload`).
#[derive(Debug, Clone)]
pub struct CallOfferView {
    pub call_id: String,
    pub from: Jid,
    pub auto_rejected: bool,
    pub platform: String,
    pub version: String,
    pub group: Option<Jid>,
}

pub async fn call_offer_envelope<R: Resolver>(
    r: &R,
    c: &CallOfferView,
    device_id: &str,
    ts: i64,
) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("call_id".into(), c.call_id.clone().into());
    if c.from.is_lid() {
        p.insert("from_lid".into(), wa::jid::non_ad_string(&c.from).into());
    }
    p.insert(
        "from".into(),
        normalize(r, &c.from).await.to_string().into(),
    );
    p.insert(
        "sender_display_name".into(),
        r.display_name(&c.from, false, "").await.into(),
    );
    p.insert("auto_rejected".into(), c.auto_rejected.into());
    if !c.platform.is_empty() {
        p.insert("remote_platform".into(), c.platform.clone().into());
    }
    if !c.version.is_empty() {
        p.insert("remote_version".into(), c.version.clone().into());
    }
    if let Some(group) = &c.group {
        p.insert("group_jid".into(), wa::jid::non_ad_string(group).into());
    }
    group_envelope(CALL_OFFER, device_id, ts, Value::Object(p))
}

fn group_envelope(event: &str, device_id: &str, ts: i64, payload: Value) -> Map<String, Value> {
    let mut env = Map::new();
    env.insert("event".into(), event.into());
    env.insert("payload".into(), payload);
    env.insert("timestamp".into(), rfc3339(ts).into());
    if !device_id.is_empty() {
        env.insert("device_id".into(), device_id.into());
    }
    env
}

/// JIDs as GOWA `jidsToStrings`: LID → phone number when known, non-AD.
async fn jid_strings<R: Resolver>(r: &R, jids: &[Jid]) -> Vec<String> {
    let mut out = Vec::with_capacity(jids.len());
    for jid in jids {
        out.push(normalize(r, jid).await.to_string());
    }
    out
}

/// `group.participants` (`join` | `leave` | `promote` | `demote`).
pub async fn group_participants_envelope<R: Resolver>(
    r: &R,
    group: &Jid,
    kind: &str,
    jids: &[Jid],
    device_id: &str,
    ts: i64,
) -> Map<String, Value> {
    let payload = json!({
        "chat_id": group.to_non_ad().to_string(),
        "type": kind,
        "jids": jid_strings(r, jids).await,
    });
    group_envelope(GROUP_PARTICIPANTS, device_id, ts, payload)
}

/// `group.info` for a subject change. `set_by` prefers the phone-number JID
/// WhatsApp sends next to a LID (GOWA `groupNameSetBy`); omitted when unknown.
pub async fn group_info_envelope<R: Resolver>(
    r: &R,
    group: &Jid,
    name: &str,
    set_by: Option<(&Jid, Option<&Jid>)>,
    device_id: &str,
    ts: i64,
) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("chat_id".into(), group.to_non_ad().to_string().into());
    p.insert("name".into(), name.into());
    if let Some((owner, owner_pn)) = set_by {
        let who = match owner_pn {
            Some(pn) => pn.to_non_ad(),
            None => normalize(r, owner).await,
        };
        p.insert("name_set_by".into(), who.to_string().into());
    }
    group_envelope(GROUP_INFO, device_id, ts, Value::Object(p))
}

/// `group.joined`: this account was added to (or created) a group.
pub fn group_joined_envelope(
    group: &Jid,
    own: &Jid,
    reason: &str,
    group_name: &str,
    device_id: &str,
    ts: i64,
) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("chat_id".into(), group.to_non_ad().to_string().into());
    p.insert("type".into(), "join".into());
    p.insert("jids".into(), json!([own.to_non_ad().to_string()]));
    p.insert("reason".into(), reason.into());
    if !group_name.is_empty() {
        p.insert("group_name".into(), group_name.into());
    }
    group_envelope(GROUP_JOINED, device_id, ts, Value::Object(p))
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
