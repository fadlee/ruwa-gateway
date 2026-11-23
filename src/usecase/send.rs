//! `/send/*` logic, mirroring GOWA `usecase/send.go` and
//! `validations/send_validation.go`.

use std::{collections::BTreeMap, sync::Arc};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    device::{DeviceInstance, RecentMessage},
    error::{ApiError, ApiResult},
    wa::{
        Jid,
        jid::{self, parse_jid},
        message as msgx, proto,
        sdk::{
            prelude::{Client, SendError, SendRequest},
            waproto::buffa::MessageField,
        },
    },
};

/// Allowed disappearing-message durations, in seconds.
const VALID_DURATIONS: [i64; 4] = [0, 86_400, 604_800, 7_776_000];
const EVERYONE: &str = "@everyone";

#[derive(Debug, Default, Deserialize)]
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

/// GOWA `MustLogin`.
pub(crate) fn must_login(device: &DeviceInstance) -> ApiResult<Arc<Client>> {
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

/// GOWA `mergeReplyContext`: quote a known message; unknown ids are ignored.
pub(crate) fn merge_reply(
    device: &DeviceInstance,
    ci: &mut proto::ContextInfo,
    reply_id: Option<&str>,
    recipient: &Jid,
) {
    let Some(id) = reply_id.filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(quoted) = device.recall(id) else {
        tracing::warn!(
            id,
            "reply message not found, continuing without reply context"
        );
        return;
    };
    ci.stanza_id = Some(id.to_owned());
    ci.participant = Some(quoted.sender.clone());
    ci.quoted_message = MessageField::some(msgx::conversation(quoted.text.clone()));
    if quoted.chat.ends_with("@g.us") && quoted.chat != recipient.to_non_ad().to_string() {
        ci.remote_jid = Some(quoted.chat.clone());
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
    if let Some(duration) = req.duration {
        ci.expiration = Some(duration as u32);
    }
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
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient);
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
    })
}

/// Send and remember the message (for replies). `text` is what GOWA stores
/// as the message content (body, caption, or an emoji label for media).
pub(crate) async fn deliver(
    device: &DeviceInstance,
    client: &Client,
    recipient: &Jid,
    message: proto::Message,
    text: &str,
) -> ApiResult<String> {
    let sent = client
        .send(SendRequest::new(recipient, message))
        .await
        .map_err(send_error)?;
    let message_id = sent.message_id.to_string();
    device.remember(
        message_id.clone(),
        RecentMessage {
            chat: recipient.to_non_ad().to_string(),
            sender: device.jid().unwrap_or_default(),
            text: text.to_owned(),
        },
    );
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
}
