//! `/newsletter/*` (GOWA `usecase/newsletter.go`, `validations/newsletter_validation.go`).

use std::{collections::BTreeMap, path::Path};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{send::must_login, user::newsletter_value};
use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    wa::{
        Jid, jid, message as msgx,
        sdk::{NewsletterError, NewsletterMessage, download::DownloadLimits},
    },
};

const SUFFIX: &str = "@newsletter";
const INVITE_PREFIX: &str = "https://whatsapp.com/channel/";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct IdRequest {
    pub newsletter_id: String,
}

/// ozzo `Required` + `validateNewsletterJIDShape` on `newsletter_id`.
fn id_errors(id: &str, shape: bool) -> BTreeMap<&'static str, String> {
    let mut errors = BTreeMap::new();
    if id.is_empty() {
        errors.insert("newsletter_id", "cannot be blank".to_owned());
    } else if shape && !id.ends_with(SUFFIX) {
        errors.insert("newsletter_id", format!("must end with {SUFFIX}"));
    }
    errors
}

/// `ValidateJidWithLogin` for a newsletter id: login first, then parse.
fn target(
    device: &DeviceInstance,
    id: &str,
) -> ApiResult<(std::sync::Arc<crate::wa::sdk::prelude::Client>, Jid)> {
    let client = must_login(device)?;
    let jid = jid::parse_jid(id).map_err(ApiError::invalid_jid)?;
    Ok((client, jid))
}

fn failed(error: NewsletterError) -> ApiError {
    ApiError::internal(error)
}

/// `POST /newsletter/follow`.
pub async fn follow(device: &DeviceInstance, req: IdRequest) -> ApiResult<()> {
    super::chat::ozzo(id_errors(&req.newsletter_id, true))?;
    let (client, jid) = target(device, &req.newsletter_id)?;
    client.newsletter().join(&jid).await.map_err(failed)?;
    Ok(())
}

/// `POST /newsletter/unfollow` (GOWA checks presence only, not the shape).
pub async fn unfollow(device: &DeviceInstance, req: IdRequest) -> ApiResult<()> {
    super::chat::ozzo(id_errors(&req.newsletter_id, false))?;
    let (client, jid) = target(device, &req.newsletter_id)?;
    client.newsletter().leave(&jid).await.map_err(failed)?;
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct InfoQuery {
    pub newsletter_id: String,
    pub invite: String,
}

/// `GET /newsletter/info`: exactly one of `newsletter_id` / `invite`.
pub async fn info(device: &DeviceInstance, q: InfoQuery) -> ApiResult<Value> {
    let (id, invite) = (q.newsletter_id.trim(), q.invite.trim());
    if id.is_empty() == invite.is_empty() {
        return Err(ApiError::validation(
            "exactly one of newsletter_id or invite must be provided.",
        ));
    }
    let mut errors = BTreeMap::new();
    if !q.newsletter_id.is_empty() && !q.newsletter_id.ends_with(SUFFIX) {
        errors.insert("newsletter_id", format!("must end with {SUFFIX}"));
    }
    super::chat::ozzo(errors)?;
    let metadata = if !invite.is_empty() {
        let client = must_login(device)?;
        let code = invite.strip_prefix(INVITE_PREFIX).unwrap_or(invite);
        client
            .newsletter()
            .get_metadata_by_invite(code)
            .await
            .map_err(failed)?
    } else {
        let (client, jid) = target(device, &q.newsletter_id)?;
        client
            .newsletter()
            .get_metadata(&jid)
            .await
            .map_err(failed)?
    };
    Ok(newsletter_value(&metadata))
}

/// Query ints the way Fiber binds them: a non-number is a 500.
fn query_int(field: &str, raw: &str, default: i64) -> ApiResult<i64> {
    if raw.is_empty() {
        return Ok(default);
    }
    raw.parse().map_err(|_| {
        ApiError::internal(format!(
            "failed to decode query: {field}: strconv.ParseInt: parsing {raw:?}: invalid syntax"
        ))
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct MessagesQuery {
    pub newsletter_id: String,
    pub count: String,
    pub before: String,
}

/// One message as GOWA's `domainNewsletter.Message`.
fn message_value(m: &NewsletterMessage) -> Value {
    let mut v = serde_json::Map::new();
    v.insert("server_id".into(), json!(m.server_id));
    if let Some(id) = &m.message_id {
        let id = id.to_string();
        if !id.is_empty() {
            v.insert("message_id".into(), id.into());
        }
    }
    if let Some(kind) = m.message_type_raw.as_deref().filter(|k| !k.is_empty()) {
        v.insert("type".into(), kind.into());
    }
    if m.timestamp > 0 {
        v.insert(
            "timestamp".into(),
            crate::events::rfc3339(m.timestamp as i64).into(),
        );
    }
    v.insert("views_count".into(), json!(m.views_count.unwrap_or(0)));
    v.insert(
        "forwards_count".into(),
        json!(m.forwards_count.unwrap_or(0)),
    );
    if !m.reactions.is_empty() {
        let counts: serde_json::Map<String, Value> = m
            .reactions
            .iter()
            .map(|r| (r.code.clone(), json!(r.count)))
            .collect();
        v.insert("reaction_counts".into(), Value::Object(counts));
    }
    if let Some(msg) = &m.message {
        let text = msgx::storage_text(msg);
        if !text.is_empty() {
            v.insert("text".into(), text.into());
        }
    }
    Value::Object(v)
}

/// `GET /newsletter/messages`.
pub async fn messages(device: &DeviceInstance, q: MessagesQuery) -> ApiResult<Value> {
    let count = query_int("count", &q.count, 0)?;
    let before = query_int("before", &q.before, 0)?;
    let count = if count == 0 { 50 } else { count };
    let mut errors = id_errors(&q.newsletter_id, true);
    if count < 1 {
        errors.insert("count", "must be no less than 1".into());
    } else if count > 100 {
        errors.insert("count", "must be no greater than 100".into());
    }
    if before < 0 {
        errors.insert("before", "must be no less than 0".into());
    }
    super::chat::ozzo(errors)?;
    let (client, jid) = target(device, &q.newsletter_id)?;
    let found = client
        .newsletter()
        .get_messages(jid, count as u32, (before != 0).then_some(before as u64))
        .await
        .map_err(failed)?;
    Ok(json!({ "data": found.iter().map(message_value).collect::<Vec<_>>() }))
}

#[derive(Debug, Default, Serialize)]
pub struct MediaDownload {
    pub server_id: i64,
    pub message_id: String,
    pub status: String,
    pub media_type: String,
    pub mime_type: String,
    pub filename: String,
    pub file_path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file_url: String,
    pub file_size: i64,
}

use serde::Serialize;

/// `GET /newsletter/messages/{server_id}/download`: fetch the one message,
/// save its media under `<media_dir>/<channel id>/<YYYY-MM-DD>/`.
pub async fn download_media(
    device: &DeviceInstance,
    raw_server_id: &str,
    newsletter_id: &str,
    media_dir: &Path,
) -> ApiResult<MediaDownload> {
    let server_id: i64 = raw_server_id
        .parse()
        .map_err(|_| ApiError::validation("server_id: must be a valid integer."))?;
    let mut errors = id_errors(newsletter_id, true);
    if server_id < 1 {
        errors.insert("server_id", "must be no less than 1".into());
    }
    super::chat::ozzo(errors)?;
    let (client, jid) = target(device, newsletter_id)?;
    let found = client
        .newsletter()
        .get_messages(jid.clone(), 1, Some(server_id as u64 + 1))
        .await
        .map_err(|e| ApiError::internal(format!("failed to get newsletter message: {e}")))?;
    let message = found
        .iter()
        .find(|m| m.server_id == server_id as u64)
        .ok_or_else(|| {
            ApiError::internal(format!(
                "newsletter message with server ID {server_id} not found"
            ))
        })?;
    let not_media = || {
        ApiError::internal(format!(
            "newsletter message {server_id} does not contain downloadable media"
        ))
    };
    let msg = message.message.as_ref().ok_or_else(not_media)?;
    let (source, media_type, mime, filename): (
        &dyn crate::wa::sdk::download::Downloadable,
        _,
        _,
        _,
    ) = if let Some(x) = msg.image_message.as_option() {
        (x, "image", x.mimetype.clone(), "")
    } else if let Some(x) = msg.video_message.as_option() {
        (x, "video", x.mimetype.clone(), "")
    } else if let Some(x) = msg.ptv_message.as_option() {
        (x, "video_note", x.mimetype.clone(), "")
    } else if let Some(x) = msg.audio_message.as_option() {
        (x, "audio", x.mimetype.clone(), "")
    } else if let Some(x) = msg.document_message.as_option() {
        (
            x,
            "document",
            x.mimetype.clone(),
            x.file_name.as_deref().unwrap_or(""),
        )
    } else if let Some(x) = msg.sticker_message.as_option() {
        (x, "sticker", x.mimetype.clone(), "")
    } else {
        return Err(not_media());
    };

    let channel_dir: String = jid.user.chars().filter(char::is_ascii_digit).collect();
    let day = chrono::DateTime::from_timestamp(message.timestamp as i64, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string();
    let dir = media_dir.join(channel_dir).join(day);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::internal(format!("failed to create media directory: {e}")))?;
    let bytes = client
        .download(source, DownloadLimits::default())
        .await
        .map_err(|e| ApiError::internal(format!("failed to download newsletter media: {e}")))?;
    let mime = mime.unwrap_or_default();
    let name = format!(
        "{}-{}{}",
        crate::device::now(),
        uuid::Uuid::new_v4(),
        crate::media::extension_for(filename, &mime)
    );
    let path = dir.join(&name);
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| ApiError::internal(format!("failed to download newsletter media: {e}")))?;
    let file_path = path.to_string_lossy().into_owned();
    Ok(MediaDownload {
        server_id,
        message_id: message
            .message_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        status: format!("Media downloaded successfully to {file_path}"),
        media_type: media_type.into(),
        mime_type: mime,
        filename: name,
        file_path,
        file_url: String::new(),
        file_size: bytes.len() as i64,
    })
}

fn status_value(s: &crate::device::live_updates::Status) -> Value {
    let mut v = json!({
        "newsletter_id": s.newsletter,
        "duration_seconds": s.duration_secs,
        "expires_at": crate::events::rfc3339(s.expires_at),
        "renewed_at": crate::events::rfc3339(s.renewed_at),
    });
    if !s.last_error.is_empty() {
        v["last_error"] = s.last_error.clone().into();
    }
    v
}

/// `GET /newsletter/live-updates`: always an array, sorted by channel.
pub fn live_updates_list(device: &DeviceInstance) -> Value {
    json!({ "data": device.live_updates.list().iter().map(status_value).collect::<Vec<_>>() })
}

/// `POST /newsletter/live-updates`.
pub async fn live_updates_subscribe(
    device: &std::sync::Arc<DeviceInstance>,
    req: IdRequest,
) -> ApiResult<Value> {
    super::chat::ozzo(id_errors(&req.newsletter_id, true))?;
    let (_, jid) = target(device, &req.newsletter_id)?;
    let status = device
        .live_updates
        .subscribe(device, jid)
        .await
        .map_err(ApiError::internal)?;
    Ok(status_value(&status))
}

/// `DELETE /newsletter/live-updates?newsletter_id=…`.
pub fn live_updates_unsubscribe(device: &DeviceInstance, id: &str) -> ApiResult<()> {
    super::chat::ozzo(id_errors(id, true))?;
    let jid =
        jid::parse_jid(id).map_err(|_| ApiError::validation("newsletter_id: invalid JID."))?;
    if device.live_updates.unsubscribe(&jid) {
        Ok(())
    } else {
        Err(ApiError::validation(format!(
            "newsletter_id: no live updates subscription for {jid}."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validation_messages() {
        let msg = |e| super::super::chat::ozzo(e).unwrap_err().message;
        assert_eq!(msg(id_errors("", true)), "newsletter_id: cannot be blank.");
        assert_eq!(
            msg(id_errors("628@s.whatsapp.net", true)),
            "newsletter_id: must end with @newsletter."
        );
        // Unfollow checks presence only.
        assert!(super::super::chat::ozzo(id_errors("628@s.whatsapp.net", false)).is_ok());
        assert!(super::super::chat::ozzo(id_errors("1@newsletter", true)).is_ok());
    }

    #[test]
    fn query_ints_like_fiber() {
        assert_eq!(query_int("count", "", 7).unwrap(), 7);
        assert_eq!(query_int("count", "12", 0).unwrap(), 12);
        assert!(query_int("count", "x", 0).is_err());
    }
}
