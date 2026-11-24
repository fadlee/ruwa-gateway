//! `/send/*` logic, mirroring GOWA `usecase/send.go` and
//! `validations/send_validation.go`.

use std::{collections::BTreeMap, sync::Arc};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    wa::{
        Jid,
        jid::{self, parse_jid},
        message as msgx, proto,
        sdk::{
            prelude::{Client, SendError, SendOptions, SendRequest},
            waproto::buffa::MessageField,
        },
    },
};

/// Allowed disappearing-message durations, in seconds.
const VALID_DURATIONS: [i64; 4] = [0, 86_400, 604_800, 7_776_000];
const EVERYONE: &str = "@everyone";

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct MessageRequest {
    pub phone: String,
    pub message: String,
    pub reply_message_id: Option<String>,
    pub is_forwarded: bool,
    pub duration: Option<i64>,
    pub mentions: Vec<String>,
    pub allow_reshare: bool,
}

/// GOWA `domains/send.GenericResponse`.
#[derive(Debug, Default, Serialize)]
pub struct SendResponse {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    pub status: String,
    /// Set when the request was scheduled instead of sent.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub schedule_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub scheduled_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub next_run_at: String,
}

/// ozzo-validation style `Required` errors: `"a: cannot be blank; b: cannot be blank."`.
pub(crate) fn required(fields: &[(&str, &str)]) -> ApiResult<()> {
    let missing: BTreeMap<&str, &str> = fields
        .iter()
        .filter(|(_, v)| v.trim().is_empty())
        .map(|(k, _)| (*k, "cannot be blank"))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let joined = missing
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(ApiError::validation(format!("{joined}.")))
}

/// GOWA `validatePhoneNumber`.
pub(crate) fn validate_phone_number(phone: &str) -> ApiResult<()> {
    let phone = phone.trim();
    let phone = phone.strip_prefix('+').unwrap_or(phone);
    if phone.is_empty() {
        return Err(ApiError::validation("phone number cannot be empty"));
    }
    if phone.starts_with('0') {
        return Err(ApiError::validation(
            "phone number must be in international format (should not start with 0). For Indonesian numbers, use 62xxx format instead of 08xxx",
        ));
    }
    Ok(())
}

pub(crate) fn validate_duration(duration: Option<i64>) -> ApiResult<()> {
    match duration {
        Some(d) if !VALID_DURATIONS.contains(&d) => Err(ApiError::validation(
            "duration must be one of: 0 (no expiry), 86400 (24h), 604800 (7d), 7776000 (90d)",
        )),
        _ => Ok(()),
    }
}

pub(crate) fn validate_allow_reshare(allow: bool, phone: &str) -> ApiResult<()> {
    if allow && phone.trim() != "status@broadcast" {
        return Err(ApiError::validation(
            "allow_reshare is only supported when phone is status@broadcast",
        ));
    }
    Ok(())
}

pub(crate) fn validate_mentions(mentions: &[String]) -> ApiResult<()> {
    for mention in mentions.iter().filter(|m| *m != EVERYONE) {
        if validate_phone_number(mention).is_err() {
            return Err(ApiError::validation(format!(
                "mention {mention}: phone number must be in international format"
            )));
        }
    }
    Ok(())
}

/// GOWA `ValidateSendMessage` (run after `SanitizePhone`).
pub fn validate_message(req: &MessageRequest) -> ApiResult<()> {
    required(&[("phone", &req.phone), ("message", &req.message)])?;
    validate_phone_number(&req.phone)?;
    validate_duration(req.duration)?;
    validate_allow_reshare(req.allow_reshare, &req.phone)?;
    validate_mentions(&req.mentions)
}

/// Identity of the occurrence a scheduled send is attempting, kept across
/// retries and crashes (GOWA `ContextWithSendMessageID` / `ContextWithPollSecret`).
#[derive(Debug, Clone)]
pub struct Pinned {
    pub message_id: String,
    pub poll_secret: Option<Vec<u8>>,
}

tokio::task_local! {
    /// Set while validating a request that is about to be scheduled: the send
    /// runs its own validation, then stops at the login check instead of sending.
    pub(crate) static DRY_RUN: ();
    /// Set while a scheduled occurrence is dispatched.
    pub(crate) static PINNED: Pinned;
}

pub(crate) const DRY_RUN_CODE: &str = "SCHEDULE_DRY_RUN";

/// What a dry run reports once validation has passed.
pub(crate) fn dry_run_ok() -> ApiError {
    ApiError::new(axum::http::StatusCode::ACCEPTED, DRY_RUN_CODE, "validated")
}

/// GOWA `MustLogin`.
pub(crate) fn must_login(device: &DeviceInstance) -> ApiResult<Arc<Client>> {
    if DRY_RUN.try_with(|()| ()).is_ok() {
        // Validation passed; scheduling does not need a live session.
        return Err(dry_run_ok());
    }
    let client = device.client().ok_or_else(ApiError::not_connected)?;
    if !client.is_socket_connected() {
        return Err(ApiError::not_connected());
    }
    if !client.is_logged_in() {
        return Err(ApiError::not_logged_in());
    }
    Ok(client)
}

/// GOWA `ValidateJidWithLogin` (minus `MustLogin`, done once per request).
pub(crate) async fn resolve_recipient(
    client: &Client,
    raw: &str,
    account_validation: bool,
) -> ApiResult<Jid> {
    let parsed = parse_jid(raw).map_err(ApiError::internal)?;
    if parsed.is_lid() {
        if let Ok(Some(entry)) = client.get_lid_pn_entry(&parsed.to_non_ad()).await {
            return Ok(Jid::pn(&*entry.phone_number));
        }
        return Ok(parsed);
    }
    if !account_validation || !jid::is_pn_user(&parsed) {
        return Ok(parsed);
    }
    let bare = Jid::pn(parsed.user.trim_start_matches('+'));
    let results = client
        .contacts()
        .is_on_whatsapp(std::slice::from_ref(&bare))
        .await
        .map_err(|e| {
            ApiError::internal(format!("failed to check {} on WhatsApp: {e}", bare.user))
        })?;
    match results.into_iter().find(|r| r.is_registered) {
        Some(found) => Ok(found.jid.to_non_ad()),
        None => Err(ApiError::invalid_jid(format!(
            "Phone {raw} is not on whatsapp"
        ))),
    }
}

/// Text `@<digits>` mentions plus explicit ones (GOWA `resolveMentions`).
pub(crate) async fn resolve_mentions(
    client: &Client,
    text: &str,
    explicit: &[String],
    recipient: &Jid,
    account_validation: bool,
) -> Vec<String> {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"@(\d+)").unwrap());
    let mut out: Vec<String> = Vec::new();
    let push = |jid: String, out: &mut Vec<String>| {
        if !out.contains(&jid) {
            out.push(jid);
        }
    };
    for cap in RE.captures_iter(text) {
        if let Ok(jid) = resolve_recipient(client, &cap[1], account_validation).await {
            push(jid.to_string(), &mut out);
        }
    }
    for mention in explicit {
        if mention == EVERYONE {
            if jid::is_group(recipient)
                && let Ok(meta) = client.groups().fetch_metadata(recipient).await
            {
                for p in meta.participants {
                    push(p.jid.to_string(), &mut out);
                }
            }
            continue;
        }
        if let Ok(jid) = resolve_recipient(client, mention, account_validation).await {
            push(jid.to_string(), &mut out);
        }
    }
    out
}

/// GOWA `mergeReplyContext`: quote a stored message; unknown ids are ignored.
pub(crate) async fn merge_reply(
    device: &DeviceInstance,
    ci: &mut proto::ContextInfo,
    reply_id: Option<&str>,
    recipient: &Jid,
) {
    let Some(id) = reply_id.filter(|id| !id.is_empty()) else {
        return;
    };
    let quoted = match device
        .chats()
        .get_message(&device.storage_partition(), id)
        .await
    {
        Ok(Some(quoted)) => quoted,
        Ok(None) => {
            tracing::warn!(
                id,
                "reply message not found in storage, continuing without reply context"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(id, %error, "error retrieving reply message, continuing without reply context");
            return;
        }
    };
    ci.stanza_id = Some(id.to_owned());
    ci.participant = Some(quoted.sender.clone());
    ci.quoted_message = MessageField::some(msgx::conversation(quoted.content.clone()));
    if quoted.chat_jid.ends_with("@g.us") && quoted.chat_jid != recipient.to_non_ad().to_string() {
        ci.remote_jid = Some(quoted.chat_jid.clone());
    }
}

/// Keys a request's stored chat is looked up under (GOWA `chatKeys`): an
/// `@lid` request reads its own row first, anything else the resolved JID.
pub(crate) fn chat_keys(requested: &str, resolved: &Jid) -> Vec<String> {
    let resolved = resolved.to_string();
    if requested.ends_with("@lid") {
        vec![requested.to_owned(), resolved]
    } else {
        vec![resolved, requested.to_owned()]
    }
}

/// GOWA `getDefaultEphemeralExpiration`: the timer stored for this chat.
pub(crate) async fn default_expiration(device: &DeviceInstance, keys: &[String]) -> u32 {
    match device
        .chats()
        .first_chat(&device.storage_partition(), keys)
        .await
    {
        Ok(Some(chat)) => chat.ephemeral_expiration.max(0) as u32,
        _ => 0,
    }
}

/// Go `time.Time.String()` for a UTC instant.
pub(crate) fn go_time_string(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y-%m-%d %H:%M:%S +0000 UTC").to_string()
}

pub async fn send_text(
    device: &DeviceInstance,
    mut req: MessageRequest,
    account_validation: bool,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    validate_message(&req)?;
    let client = must_login(device)?;
    let recipient = resolve_recipient(&client, &req.phone, account_validation).await?;

    let mut ci = proto::ContextInfo::default();
    if req.is_forwarded {
        ci.is_forwarded = Some(true);
        ci.forwarding_score = Some(100);
    }
    // An explicit 0 turns expiry off; no duration uses the chat's timer.
    ci.expiration = Some(match req.duration {
        Some(duration) => duration as u32,
        None => default_expiration(device, &chat_keys(&req.phone, &recipient)).await,
    });
    let mentions = resolve_mentions(
        &client,
        &req.message,
        &req.mentions,
        &recipient,
        account_validation,
    )
    .await;
    if !mentions.is_empty() {
        ci.mentioned_jid = mentions;
    }
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient).await;
    // TODO(Fase 4): status reshare flag for `allow_reshare` on status@broadcast.

    let message = msgx::extended_text(req.message.clone(), ci);
    let message_id = deliver(device, &client, &recipient, message, &req.message).await?;
    Ok(SendResponse {
        status: format!(
            "Message sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
        ..Default::default()
    })
}

/// Send, then store the message in chat storage (GOWA `wrapSendMessage`).
/// `text` is what GOWA stores as the content (body, caption, or a label for
/// media). A storage failure is logged; the message has already gone out.
pub(crate) async fn deliver(
    device: &DeviceInstance,
    client: &Client,
    recipient: &Jid,
    message: proto::Message,
    text: &str,
) -> ApiResult<String> {
    let mut request = SendRequest::new(recipient, message.clone());
    if let Ok(pinned) = PINNED.try_with(Clone::clone)
        && let Ok(id) = crate::wa::sdk::MessageId::new(&pinned.message_id)
    {
        request = request.with_options(SendOptions::default().with_message_id(id));
    }
    let sent = client.send(request).await.map_err(send_error)?;
    let message_id = sent.message_id.to_string();
    // whatsmeow waits for the server ack, so a nack (463 …) fails the request.
    if let Some(Some(code)) = device.acks.wait(&message_id, ACK_WAIT).await {
        return Err(nack_error(&code));
    }
    let own = client
        .pn()
        .map(|pn| pn.to_non_ad().to_string())
        .or_else(|| device.jid())
        .unwrap_or_default();
    if let Err(error) = crate::device::store_sent(
        device.chats(),
        &device.storage_partition(),
        &own,
        recipient,
        &message_id,
        text,
        crate::device::now(),
        &message,
    )
    .await
    {
        tracing::warn!(id = %message_id, %error, "failed to store sent message");
    }
    Ok(message_id)
}

/// Stand-in for the server ack timestamp GOWA prints in `status`.
pub(crate) fn server_time() -> String {
    go_time_string(chrono::Utc::now())
}

/// Context shared by non-text sends: forwarded flag, and an expiration
/// only when `duration > 0` (GOWA media/contact/location/link).
pub(crate) fn base_context(is_forwarded: bool, duration: Option<i64>) -> proto::ContextInfo {
    let mut ci = proto::ContextInfo::default();
    if is_forwarded {
        ci.is_forwarded = Some(true);
        ci.forwarding_score = Some(100);
    }
    if let Some(d) = duration.filter(|d| *d > 0) {
        ci.expiration = Some(d as u32);
    }
    ci
}

/// How long a send waits for the server's verdict before assuming success.
const ACK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// GOWA `ErrWaReachoutTimelock` text.
const REACHOUT_TIMELOCK: &str = "WhatsApp rejected this send with error 463 (reach-out timelock). This is WhatsApp's server-side anti-spam restriction on starting new chats and cannot be bypassed by the API. It usually means there is no prior conversation with this recipient, or the sending account is temporarily restricted from reaching new contacts. To get through: ask the recipient to message you first, reply within an already-established conversation, or open and send the chat once from the phone linked to this session, then reduce your sending rate and wait for the restriction to clear. Newly-linked or low-activity numbers are affected most.";

/// A server nack of a sent message: 463 → `WA_REACHOUT_TIMELOCK` (429), any
/// other code → whatsmeow's `server returned error N` (500).
pub(crate) fn nack_error(code: &str) -> ApiError {
    if code == "463" {
        return ApiError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "WA_REACHOUT_TIMELOCK",
            REACHOUT_TIMELOCK,
        );
    }
    ApiError::internal(format!("server returned error {code}"))
}

pub(crate) fn send_error(error: SendError) -> ApiError {
    match error {
        SendError::NotLoggedIn => ApiError::not_logged_in(),
        other => ApiError::internal(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(phone: &str, message: &str) -> MessageRequest {
        MessageRequest {
            phone: phone.into(),
            message: message.into(),
            ..Default::default()
        }
    }

    #[test]
    fn required_fields_like_ozzo() {
        assert_eq!(
            validate_message(&req("", "")).unwrap_err().message,
            "message: cannot be blank; phone: cannot be blank."
        );
        assert_eq!(
            validate_message(&req("628@s.whatsapp.net", ""))
                .unwrap_err()
                .message,
            "message: cannot be blank."
        );
    }

    #[test]
    fn local_number_rejected() {
        let err = validate_message(&req("0812@s.whatsapp.net", "hi")).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message
                .starts_with("phone number must be in international format")
        );
    }

    #[test]
    fn duration_whitelist() {
        let mut r = req("628@s.whatsapp.net", "hi");
        r.duration = Some(86_400);
        assert!(validate_message(&r).is_ok());
        r.duration = Some(5);
        assert!(validate_message(&r).is_err());
    }

    #[test]
    fn reshare_only_for_status() {
        let mut r = req("628@s.whatsapp.net", "hi");
        r.allow_reshare = true;
        assert!(validate_message(&r).is_err());
        r.phone = "status@broadcast".into();
        assert!(validate_message(&r).is_ok());
    }

    #[test]
    fn mentions_validated() {
        let mut r = req("628@s.whatsapp.net", "hi");
        r.mentions = vec!["@everyone".into(), "628".into()];
        assert!(validate_message(&r).is_ok());
        r.mentions.push("0812".into());
        assert_eq!(
            validate_message(&r).unwrap_err().message,
            "mention 0812: phone number must be in international format"
        );
    }

    #[test]
    fn go_time_format() {
        let t = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert_eq!(go_time_string(t), "2023-11-14 22:13:20 +0000 UTC");
    }

    #[test]
    fn nack_463_is_reachout_timelock() {
        let e = nack_error("463");
        assert_eq!(e.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.code, "WA_REACHOUT_TIMELOCK");
        assert!(
            e.message
                .starts_with("WhatsApp rejected this send with error 463")
        );
        let other = nack_error("479");
        assert_eq!(other.message, "server returned error 479");
    }
}
