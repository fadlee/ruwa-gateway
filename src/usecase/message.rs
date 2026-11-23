//! `/message/{id}/*` logic (GOWA `usecase/message.go`,
//! `validations/message_validation.go`). Forward and download live in
//! [`super::forward`] and [`super::download`].

use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    storage::chat::Message,
    usecase::{
        send::{must_login, resolve_recipient, server_time},
        send_media::{is_url, link_preview_message},
    },
    wa::{
        Jid, Server, jid, message as msgx, proto,
        sdk::{
            MessageRef,
            prelude::Client,
            send::{EditOptions, RevokeType},
            wacore::types::message_ref::MessageId,
            waproto::buffa::MessageField,
        },
    },
};

/// GOWA `domains/message.GenericResponse` (`message_id` always present).
#[derive(Debug, Default, Serialize)]
pub struct GenericResponse {
    pub message_id: String,
    pub status: String,
}

/// Body of revoke/delete/read/played/star/download: only `phone`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PhoneRequest {
    pub phone: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReactionRequest {
    pub phone: String,
    pub emoji: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdateRequest {
    pub phone: String,
    pub message: String,
    pub link: String,
    pub media_type: String,
}

/// ozzo `ValidateStruct` errors, keys sorted: `"a: msg; b: msg."`.
pub(crate) fn ozzo(errors: BTreeMap<&str, &str>) -> ApiResult<()> {
    if errors.is_empty() {
        return Ok(());
    }
    let joined = errors
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(ApiError::validation(format!("{joined}.")))
}

fn require(fields: &[(&'static str, &str)]) -> ApiResult<()> {
    ozzo(
        fields
            .iter()
            .filter(|(_, v)| v.trim().is_empty())
            .map(|(k, _)| (*k, "cannot be blank"))
            .collect(),
    )
}

/// `MustLogin` + `ValidateJidWithLogin` on the (sanitised) phone.
async fn target(
    device: &DeviceInstance,
    phone: &str,
    account_validation: bool,
) -> ApiResult<(Arc<Client>, Jid)> {
    let client = must_login(device)?;
    let jid = resolve_recipient(&client, phone, account_validation).await?;
    Ok((client, jid))
}

/// This device's copy of the message in `chat` (GOWA `storedMessageForAction`).
async fn stored(device: &DeviceInstance, chat: &Jid, id: &str) -> ApiResult<Option<Message>> {
    device
        .chats()
        .get_message_in_chat(
            &device.storage_partition(),
            &chat.to_non_ad().to_string(),
            id,
        )
        .await
        .map_err(ApiError::internal)
}

fn own_jid(client: &Client) -> Option<Jid> {
    client.pn().map(|j| j.to_non_ad())
}

/// LID → phone number when the mapping is known.
async fn to_pn(client: &Client, jid: Jid) -> Jid {
    if jid.is_lid()
        && let Ok(Some(entry)) = client.get_lid_pn_entry(&jid.to_non_ad()).await
    {
        return Jid::pn(&*entry.phone_number);
    }
    jid
}

fn msg_id(id: &str) -> ApiResult<MessageId> {
    MessageId::new(id).map_err(ApiError::internal)
}

/// whatsmeow `BuildMessageKey`: from me unless a different sender is
/// given; the participant only outside 1:1 chats.
fn message_key(client: &Client, chat: &Jid, sender: Option<&Jid>, id: &str) -> proto::MessageKey {
    let mut key = proto::MessageKey::default();
    key.remote_jid = Some(chat.to_string());
    key.id = Some(id.to_owned());
    key.from_me = Some(true);
    if let Some(sender) = sender {
        let own_pn = client.pn().map(|j| j.user.to_string());
        let own_lid = client.lid().map(|j| j.user.to_string());
        let is_own =
            Some(sender.user.to_string()) == own_pn || Some(sender.user.to_string()) == own_lid;
        if !sender.user.is_empty() && !is_own {
            key.from_me = Some(false);
            if !matches!(chat.server, Server::Pn | Server::Lid | Server::Messenger) {
                key.participant = Some(sender.to_non_ad().to_string());
            }
        }
    }
    key
}

/// Sender of a stored incoming message, else `None` (= from me).
fn incoming_sender(message: Option<&Message>) -> Option<Jid> {
    let m = message?;
    if m.is_from_me || m.sender.is_empty() {
        return None;
    }
    jid::parse_jid(&m.sender).ok()
}

/// GOWA `ReactMessage`.
pub async fn react(
    device: &DeviceInstance,
    message_id: &str,
    mut req: ReactionRequest,
    account_validation: bool,
) -> ApiResult<GenericResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    let (client, chat) = target(device, &req.phone, account_validation).await?;
    let message = match stored(device, &chat, message_id).await {
        Ok(m) => m,
        Err(error) => {
            tracing::warn!(id = message_id, error = %error.message, "reaction lookup failed, assuming sent by me");
            None
        }
    };
    let sender = incoming_sender(message.as_ref());
    let key = message_key(&client, &chat, sender.as_ref(), message_id);
    let sent = client
        .send_reaction_raw(chat, key, &req.emoji)
        .await
        .map_err(super::send::send_error)?;
    Ok(GenericResponse {
        message_id: sent.message_id.to_string(),
        status: format!(
            "Reaction sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
    })
}

/// GOWA `RevokeMessage`: a stored incoming message is revoked as group
/// admin; anything else as its sender. The stored copy is then deleted.
pub async fn revoke(
    device: &DeviceInstance,
    message_id: &str,
    mut req: PhoneRequest,
    account_validation: bool,
) -> ApiResult<GenericResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    let (client, chat) = target(device, &req.phone, account_validation).await?;
    let message = stored(device, &chat, message_id).await.unwrap_or_else(|e| {
        tracing::warn!(id = message_id, error = %e.message, "revoke lookup failed, assuming self-revoke");
        None
    });
    let kind = match incoming_sender(message.as_ref()) {
        Some(sender) => RevokeType::Admin {
            original_sender: to_pn(&client, sender).await,
        },
        None => RevokeType::Sender,
    };
    let sent = client
        .revoke_message_raw(chat.clone(), message_id, kind)
        .await
        .map_err(super::send::send_error)?;
    delete_stored(device, &chat, message_id).await?;
    Ok(GenericResponse {
        message_id: sent.message_id.to_string(),
        status: format!(
            "Revoke success {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
    })
}

async fn delete_stored(device: &DeviceInstance, chat: &Jid, id: &str) -> ApiResult<()> {
    device
        .chats()
        .delete_message(
            &device.storage_partition(),
            &chat.to_non_ad().to_string(),
            id,
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "WhatsApp action succeeded, but failed to delete local message {id}: {e}"
            ))
        })
}

/// `from_me` and author of a message for app-state actions: the stored
/// copy when there is one, else GOWA's heuristic (ids longer than 22
/// characters come from other clients).
async fn action_scope(
    device: &DeviceInstance,
    client: &Client,
    chat: &Jid,
    id: &str,
) -> (bool, Option<Jid>) {
    match stored(device, chat, id).await.ok().flatten() {
        Some(m) if m.is_from_me => (true, None),
        Some(m) => (
            false,
            jid::parse_jid(&m.sender).ok().or_else(|| own_jid(client)),
        ),
        None if id.len() > 22 => (
            false,
            if jid::is_group(chat) {
                own_jid(client)
            } else {
                None
            },
        ),
        None => (true, None),
    }
}

/// GOWA `DeleteMessage`: delete for me (app state), then the stored copy.
pub async fn delete_for_me(
    device: &DeviceInstance,
    message_id: &str,
    mut req: PhoneRequest,
    account_validation: bool,
) -> ApiResult<()> {
    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    let (client, chat) = target(device, &req.phone, account_validation).await?;
    let (from_me, sender) = action_scope(device, &client, &chat, message_id).await;
    let target = MessageRef::new(&chat, msg_id(message_id)?, sender.as_ref(), from_me)
        .map_err(ApiError::internal)?;
    client
        .chat_actions()
        .delete_message_for_me(&target, true, Some(chrono::Utc::now().timestamp_millis()))
        .await
        .map_err(ApiError::internal)?;
    delete_stored(device, &chat, message_id).await
}

/// GOWA `StarMessage` (app-state star/unstar).
pub async fn star(
    device: &DeviceInstance,
    message_id: &str,
    mut req: PhoneRequest,
    starred: bool,
    account_validation: bool,
) -> ApiResult<()> {
    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    let (client, chat) = target(device, &req.phone, account_validation).await?;
    let chat = chat.to_non_ad();
    let (from_me, sender) = action_scope(device, &client, &chat, message_id).await;
    let target = MessageRef::new(&chat, msg_id(message_id)?, sender.as_ref(), from_me)
        .map_err(ApiError::internal)?;
    let actions = client.chat_actions();
    if starred {
        actions.star_message(&target).await
    } else {
        actions.unstar_message(&target).await
    }
    .map_err(ApiError::internal)
}

/// GOWA `MarkAsRead`. Group receipts name the original sender, which must
/// be stored for this device.
pub async fn mark_read(
    device: &DeviceInstance,
    message_id: &str,
    mut req: PhoneRequest,
    account_validation: bool,
) -> ApiResult<GenericResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    let (client, chat) = target(device, &req.phone, account_validation).await?;
    let mut sender = None;
    if jid::is_group(&chat) {
        let message = stored(device, &chat, message_id)
            .await
            .map_err(|e| {
                ApiError::internal(format!(
                    "failed to resolve message {message_id}: {}",
                    e.message
                ))
            })?
            .ok_or_else(|| {
                ApiError::internal(format!(
                    "message with ID {message_id} not found for current device and chat {}",
                    chat.to_non_ad()
                ))
            })?;
        if !message.is_from_me {
            if message.sender.is_empty() {
                return Err(ApiError::internal(format!(
                    "sender is missing for group message {message_id}"
                )));
            }
            let parsed = jid::parse_jid(&message.sender).map_err(|e| {
                ApiError::internal(format!(
                    "failed to parse sender for message {message_id}: {e}"
                ))
            })?;
            let jid = to_pn(&client, parsed.to_non_ad()).await.to_non_ad();
            if jid.user.is_empty()
                || !matches!(jid.server, Server::Pn | Server::Lid | Server::Messenger)
            {
                return Err(ApiError::internal(format!(
                    "invalid sender JID {jid} for group message {message_id}"
                )));
            }
            sender = Some(jid);
        } else {
            sender = own_jid(&client);
        }
    }
    client
        .mark_as_read(&chat, sender.as_ref(), &[message_id])
        .await
        .map_err(ApiError::internal)?;
    Ok(GenericResponse {
        message_id: message_id.to_owned(),
        status: format!("Mark as read success {message_id}"),
    })
}

/// GOWA `MarkAsPlayed`: only stored incoming audio in this chat.
pub async fn mark_played(
    device: &DeviceInstance,
    message_id: &str,
    mut req: PhoneRequest,
    account_validation: bool,
) -> ApiResult<GenericResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    let (client, chat) = target(device, &req.phone, account_validation).await?;
    let message = device
        .chats()
        .get_message(&device.storage_partition(), message_id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to resolve message {message_id}: {e}")))?
        .ok_or_else(|| {
            ApiError::internal(format!(
                "message with ID {message_id} not found for current device"
            ))
        })?;
    let chat_str = chat.to_non_ad().to_string();
    let stored_chat = jid::parse_jid(&message.chat_jid)
        .map(|j| j.to_non_ad().to_string())
        .map_err(|e| {
            ApiError::internal(format!(
                "failed to parse chat for message {message_id}: {e}"
            ))
        })?;
    if stored_chat != chat_str {
        return Err(ApiError::validation(format!(
            "message {message_id} does not belong to chat {chat_str}"
        )));
    }
    if message.media_type != "audio" && message.media_type != "ptt" {
        return Err(ApiError::validation(format!(
            "message {message_id} is not an audio message"
        )));
    }
    if message.is_from_me {
        return Err(ApiError::validation(format!(
            "message {message_id} is not an incoming message"
        )));
    }
    let sender = jid::parse_jid(&message.sender).map_err(|e| {
        ApiError::internal(format!(
            "failed to parse sender for message {message_id}: {e}"
        ))
    })?;
    if jid::is_group(&chat) && sender.user.is_empty() {
        return Err(ApiError::internal(format!(
            "sender is missing for group message {message_id}"
        )));
    }
    let receipt_sender = jid::is_group(&chat).then_some(&sender);
    client
        .mark_as_played(&chat, receipt_sender, &[message_id])
        .await
        .map_err(ApiError::internal)?;
    Ok(GenericResponse {
        message_id: message_id.to_owned(),
        status: format!("Mark as played success {message_id}"),
    })
}

/// GOWA `containsLinkToken`: `link` appears in `text` as a whole URL.
pub fn contains_link_token(text: &str, link: &str) -> bool {
    if link.is_empty() {
        return false;
    }
    const OPENERS: &[char] = &['(', '[', '{', '<', '"', '\'', '“', '‘'];
    const CLOSERS: &[char] = &[
        '.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '”', '’',
    ];
    let starts_ok = |start: usize| {
        let prefix = &text[..start];
        let Some(last) = prefix.chars().next_back() else {
            return true;
        };
        if last.is_whitespace() {
            return true;
        }
        let before = prefix.trim_end_matches(OPENERS);
        if before.len() == prefix.len() {
            return false;
        }
        match before.chars().next_back() {
            None => true,
            Some(c) if c.is_whitespace() => true,
            Some(_) => !before
                .split_whitespace()
                .next_back()
                .is_some_and(|w| w.contains("://")),
        }
    };
    let ends_ok = |end: usize| {
        let rest = text[end..].trim_start_matches(CLOSERS);
        rest.chars().next().is_none_or(char::is_whitespace)
    };
    text.match_indices(link)
        .any(|(start, _)| starts_ok(start) && ends_ok(start + link.len()))
}

/// GOWA `UpdateMessage`: edit text (optionally with a link preview) or the
/// caption of an image/document, then mirror it into storage.
pub async fn update(
    device: &DeviceInstance,
    message_id: &str,
    mut req: UpdateRequest,
    account_validation: bool,
) -> ApiResult<GenericResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    let mut errors: BTreeMap<&str, &str> = [
        ("message", req.message.as_str()),
        ("message_id", message_id),
        ("phone", req.phone.as_str()),
    ]
    .into_iter()
    .filter(|(_, v)| v.trim().is_empty())
    .map(|(k, _)| (k, "cannot be blank"))
    .collect();
    if !req.link.is_empty() && !is_url(&req.link) {
        errors.insert("link", "must be a valid URL");
    }
    if !req.media_type.is_empty() && !matches!(req.media_type.as_str(), "image" | "document") {
        errors.insert("media_type", "must be a valid value");
    }
    ozzo(errors)?;
    if !req.media_type.is_empty() && !req.link.trim().is_empty() {
        return Err(ApiError::validation(
            "link: cannot be combined with media_type.",
        ));
    }

    let (client, chat) = target(device, &req.phone, account_validation).await?;
    // A caption edit only applies to a message of that type; with no stored
    // copy the type cannot be checked, so storage is left alone.
    let mut uncached_caption_edit = false;
    if !req.media_type.is_empty() {
        let message = stored(device, &chat, message_id).await.map_err(|e| {
            ApiError::internal(format!(
                "failed to resolve message {message_id}: {}",
                e.message
            ))
        })?;
        uncached_caption_edit = message.is_none();
        if let Some(m) = message.filter(|m| m.media_type != req.media_type) {
            let kind = if m.media_type.is_empty() {
                "text"
            } else {
                m.media_type.as_str()
            };
            return Err(ApiError::validation(format!(
                "media_type: message {message_id} is {kind}, not {}.",
                req.media_type
            )));
        }
    }

    let mut text = req.message.clone();
    let mut content = proto::Message::default();
    match req.media_type.as_str() {
        "image" => {
            let mut img = proto::message::ImageMessage::default();
            img.caption = Some(text.clone());
            content.image_message = MessageField::some(img);
        }
        "document" => {
            let mut doc = proto::message::DocumentMessage::default();
            doc.caption = Some(text.clone());
            content.document_message = MessageField::some(doc);
        }
        _ => content = msgx::conversation(text.clone()),
    }
    let link = req.link.trim();
    if !link.is_empty() {
        if !contains_link_token(&text, link) {
            text = if text.trim().is_empty() {
                link.to_owned()
            } else {
                format!("{}\n{link}", text.trim())
            };
        }
        content = link_preview_message(&text, link).await?;
    }

    let sent = client
        .edit_message_raw(chat.clone(), message_id, content, EditOptions::default())
        .await
        .map_err(super::send::send_error)?;
    let edit_id = sent.message_id.to_string();
    if !uncached_caption_edit {
        let own = own_jid(&client)
            .map(|j| j.to_string())
            .or_else(|| device.jid())
            .unwrap_or_default();
        crate::device::store_own_edit(
            device.chats(),
            &device.storage_partition(),
            &own,
            &chat,
            message_id,
            &edit_id,
            &text,
            crate::device::now(),
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "WhatsApp action succeeded, but failed to update local message {message_id}: {e}"
            ))
        })?;
    }
    Ok(GenericResponse {
        message_id: edit_id,
        status: format!(
            "Update message success {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
    })
}

// ---- forward ----

const FORWARDABLE_MEDIA: [&str; 7] = [
    "image",
    "video",
    "video_note",
    "audio",
    "ptt",
    "document",
    "sticker",
];
const UNSUPPORTED_FORWARD: &str = "unsupported message type for forward";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ForwardRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub force_reupload: bool,
}

fn is_forward_media(m: &Message) -> bool {
    FORWARDABLE_MEDIA.contains(&m.media_type.as_str())
}

/// GOWA `IsForwardableStorageMessage`: media, or plain text that is not a
/// contact/location summary.
pub fn is_forwardable(m: &Message) -> bool {
    if m.media_type == "call" {
        return false;
    }
    if is_forward_media(m) {
        return true;
    }
    if !m.media_type.is_empty() {
        return false;
    }
    let text = m.content.trim();
    !text.is_empty()
        && !text.starts_with("Contact:")
        && !text.starts_with("Contacts:")
        && !text.contains("https://maps.google.com/?q=")
}

/// GOWA `forwardStoredContent`.
fn forward_content(m: &Message) -> String {
    if !m.content.is_empty() {
        return m.content.clone();
    }
    match m.media_type.as_str() {
        "image" => "🖼️ Image",
        "video" | "video_note" => "🎬 Video",
        "audio" | "ptt" => "🎵 Audio",
        "document" => "📄 Document",
        "sticker" => "🎨 Sticker",
        _ => "",
    }
    .to_owned()
}

/// GOWA `ResolveMediaDirectPath`: stored path, else the request URI of a
/// legacy full media URL.
pub fn resolve_direct_path(direct_path: &str, url: &str) -> String {
    let direct = direct_path.trim();
    if !direct.is_empty() {
        return direct.to_owned();
    }
    match reqwest::Url::parse(url.trim()) {
        Ok(u) => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        Err(_) => String::new(),
    }
}

/// GOWA `defaultStoredMediaMimeType` (audio left to sniffing).
fn stored_mime(media_type: &str, filename: &str) -> String {
    match media_type {
        "image" => "image/jpeg".into(),
        "video" | "video_note" => "video/mp4".into(),
        "sticker" => "image/webp".into(),
        "document" => {
            let guessed = mime_guess::from_path(filename).first_raw();
            guessed.unwrap_or("application/octet-stream").into()
        }
        _ => String::new(),
    }
}

/// GOWA `defaultForwardMimeType`.
fn forward_mime(m: &Message) -> String {
    match m.media_type.as_str() {
        "audio" => "audio/mpeg".into(),
        "ptt" => "audio/ogg; codecs=opus".into(),
        other => stored_mime(other, &m.filename),
    }
}

/// Media references of a forward: the stored ones, or a fresh upload.
struct MediaRefs {
    url: String,
    direct_path: String,
    media_key: Option<Vec<u8>>,
    file_sha256: Option<Vec<u8>>,
    file_enc_sha256: Option<Vec<u8>>,
    file_length: u64,
}

impl MediaRefs {
    fn stored(m: &Message) -> Self {
        Self {
            url: m.url.clone(),
            direct_path: resolve_direct_path(&m.direct_path, &m.url),
            media_key: m.media_key.clone(),
            file_sha256: m.file_sha256.clone(),
            file_enc_sha256: m.file_enc_sha256.clone(),
            file_length: m.file_length.max(0) as u64,
        }
    }

    fn uploaded(u: &crate::wa::sdk::upload::UploadResponse) -> Self {
        Self {
            url: u.url.clone(),
            direct_path: u.direct_path.clone(),
            media_key: Some(u.media_key.to_vec()),
            file_sha256: Some(u.file_sha256.to_vec()),
            file_enc_sha256: Some(u.file_enc_sha256.to_vec()),
            file_length: u.file_length,
        }
    }
}

/// GOWA `BuildForwardMessageFromStorage` / `BuildDownloadableMessage`:
/// a media message carrying `refs`. `ci` and `caption` are only set when given.
fn media_message(
    m: &Message,
    refs: &MediaRefs,
    mime: &str,
    caption: Option<&str>,
    ci: Option<&proto::ContextInfo>,
) -> ApiResult<proto::Message> {
    macro_rules! fill {
        ($x:expr) => {{
            $x.url = Some(refs.url.clone());
            $x.direct_path = Some(refs.direct_path.clone());
            $x.media_key = refs.media_key.clone();
            $x.file_sha256 = refs.file_sha256.clone();
            $x.file_enc_sha256 = refs.file_enc_sha256.clone();
            $x.file_length = Some(refs.file_length);
            if !mime.is_empty() {
                $x.mimetype = Some(mime.to_owned());
            }
            if let Some(ci) = ci {
                $x.context_info = MessageField::some(ci.clone());
            }
        }};
    }
    let mut msg = proto::Message::default();
    match m.media_type.as_str() {
        "image" => {
            let mut x = proto::message::ImageMessage::default();
            fill!(x);
            x.caption = caption.map(str::to_owned);
            msg.image_message = MessageField::some(x);
        }
        "video" | "video_note" => {
            let mut x = proto::message::VideoMessage::default();
            fill!(x);
            x.caption = caption.map(str::to_owned);
            if m.media_type == "video_note" {
                msg.ptv_message = MessageField::some(x);
            } else {
                msg.video_message = MessageField::some(x);
            }
        }
        "audio" | "ptt" => {
            let mut x = proto::message::AudioMessage::default();
            fill!(x);
            if m.media_type == "ptt" {
                x.ptt = Some(true);
            }
            msg.audio_message = MessageField::some(x);
        }
        "document" => {
            let mut x = proto::message::DocumentMessage::default();
            fill!(x);
            x.file_name = Some(m.filename.clone());
            x.caption = caption.map(str::to_owned);
            msg.document_message = MessageField::some(x);
        }
        "sticker" => {
            let mut x = proto::message::StickerMessage::default();
            fill!(x);
            msg.sticker_message = MessageField::some(x);
        }
        _ => return Err(ApiError::validation(UNSUPPORTED_FORWARD)),
    }
    Ok(msg)
}

fn forward_message(
    m: &Message,
    duration: Option<i64>,
    upload: Option<(&crate::wa::sdk::upload::UploadResponse, &str)>,
) -> ApiResult<proto::Message> {
    let mut ci = proto::ContextInfo::default();
    ci.is_forwarded = Some(true);
    ci.forwarding_score = Some(100);
    if let Some(d) = duration.filter(|d| *d > 0) {
        ci.expiration = Some(d as u32);
    }
    if m.media_type.is_empty() {
        return Ok(msgx::extended_text(m.content.clone(), ci));
    }
    let (refs, mut mime) = match upload {
        Some((u, mime)) => (MediaRefs::uploaded(u), mime.to_owned()),
        None => {
            let refs = MediaRefs::stored(m);
            if refs.direct_path.is_empty() && refs.url.is_empty() {
                return Err(ApiError::internal(format!(
                    "message {} has no media references",
                    m.id
                )));
            }
            (refs, String::new())
        }
    };
    if mime.is_empty() {
        mime = forward_mime(m);
    }
    if m.media_type == "ptt" && mime == "audio/ogg" {
        mime = "audio/ogg; codecs=opus".into();
    }
    media_message(m, &refs, &mime, Some(&m.content), Some(&ci))
}

/// Download a stored message's media (GOWA `ExtractMedia`), returning the
/// bytes and MIME type. Expired media is not re-requested from the phone yet.
async fn download_stored(client: &Client, m: &Message) -> ApiResult<(Vec<u8>, String)> {
    use crate::wa::sdk::download::{DownloadLimits, Downloadable};

    let direct_path = resolve_direct_path(&m.direct_path, &m.url);
    if m.media_type.is_empty() || direct_path.is_empty() {
        return Err(ApiError::internal(format!(
            "message {} does not contain downloadable media",
            m.id
        )));
    }
    if m.file_length as u64 > crate::media::MAX_DOWNLOAD_SIZE {
        return Err(ApiError::internal(format!(
            "failed to download media: file size exceeds the maximum limit of {} bytes",
            crate::media::MAX_DOWNLOAD_SIZE
        )));
    }
    let mime = stored_mime(&m.media_type, &m.filename);
    let msg = media_message(m, &MediaRefs::stored(m), &mime, None, None)
        .map_err(|_| ApiError::internal(format!("unsupported media type: {}", m.media_type)))?;
    let source: &dyn Downloadable = if let Some(x) = msg.image_message.as_option() {
        x
    } else if let Some(x) = msg.video_message.as_option() {
        x
    } else if let Some(x) = msg.ptv_message.as_option() {
        x
    } else if let Some(x) = msg.audio_message.as_option() {
        x
    } else if let Some(x) = msg.document_message.as_option() {
        x
    } else if let Some(x) = msg.sticker_message.as_option() {
        x
    } else {
        return Err(ApiError::internal(format!(
            "unsupported media type: {}",
            m.media_type
        )));
    };
    let bytes = client
        .download(source, DownloadLimits::default())
        .await
        .map_err(|e| ApiError::internal(format!("failed to download media: {e}")))?;
    if bytes.len() as u64 > crate::media::MAX_DOWNLOAD_SIZE {
        return Err(ApiError::internal(format!(
            "failed to download media: file size exceeds the maximum limit of {} bytes",
            crate::media::MAX_DOWNLOAD_SIZE
        )));
    }
    let mime = if mime.is_empty() {
        crate::media::detect_mime(&bytes, &m.filename)
    } else {
        mime
    };
    Ok((bytes, mime))
}

/// GOWA `reuploadForwardMessage`.
async fn reupload_forward(
    client: &Client,
    m: &Message,
    duration: Option<i64>,
) -> ApiResult<proto::Message> {
    use crate::wa::sdk::download::MediaType;

    let (bytes, mime) = download_stored(client, m).await.map_err(|e| {
        ApiError::internal(format!(
            "failed to download media for re-upload: {}",
            e.message
        ))
    })?;
    let kind = match m.media_type.as_str() {
        "image" | "sticker" => MediaType::Image,
        "video" | "video_note" => MediaType::Video,
        "audio" | "ptt" => MediaType::Audio,
        "document" => MediaType::Document,
        other => {
            return Err(ApiError::internal(format!(
                "unsupported media type: {other}"
            )));
        }
    };
    let uploaded = crate::usecase::send_media::upload(client, bytes, kind)
        .await
        .map_err(|mut e| {
            e.message = format!("failed to re-upload media: {}", e.message);
            e
        })?;
    forward_message(m, duration, Some((&uploaded, &mime)))
}

/// GOWA `SendForward`: resend a stored message, marked forwarded. Media is
/// relayed from the same CDN blob, re-uploaded when that fails or when
/// `force_reupload` is set.
pub async fn forward(
    device: &DeviceInstance,
    message_id: &str,
    mut req: ForwardRequest,
    account_validation: bool,
) -> ApiResult<GenericResponse> {
    use super::send::{
        chat_keys, default_expiration, deliver, validate_duration, validate_phone_number,
    };

    req.phone = jid::sanitize_phone(&req.phone);
    require(&[("message_id", message_id), ("phone", &req.phone)])?;
    validate_phone_number(&req.phone)?;
    validate_duration(req.duration)?;

    let message = device
        .chats()
        .get_message(&device.storage_partition(), message_id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to load message {message_id}: {e}")))?
        .ok_or_else(|| ApiError::internal(format!("message with ID {message_id} not found")))?;
    if !is_forwardable(&message) {
        return Err(ApiError::validation(UNSUPPORTED_FORWARD));
    }
    let (client, recipient) = target(device, &req.phone, account_validation).await?;
    let duration = match req.duration {
        Some(d) => Some(d),
        None => Some(default_expiration(device, &chat_keys(&req.phone, &recipient)).await as i64)
            .filter(|d| *d != 0),
    };
    let content = forward_content(&message);
    let media = is_forward_media(&message);

    let msg = if req.force_reupload && media {
        reupload_forward(&client, &message, duration).await?
    } else {
        forward_message(&message, duration, None)?
    };
    let sent = match deliver(device, &client, &recipient, msg, &content).await {
        Err(error) if media && !req.force_reupload => {
            match reupload_forward(&client, &message, duration).await {
                Ok(msg) => deliver(device, &client, &recipient, msg, &content).await,
                Err(_) => Err(error),
            }
        }
        other => other,
    }?;
    Ok(GenericResponse {
        message_id: sent,
        status: format!(
            "Message forwarded to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
    })
}

// ---- download ----

#[derive(Debug, Default, Serialize)]
pub struct DownloadResponse {
    pub message_id: String,
    pub status: String,
    pub media_type: String,
    pub filename: String,
    pub file_path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file_url: String,
    pub file_size: i64,
}

/// GOWA `DownloadMedia`: save this device's copy of the media under
/// `<media_dir>/<phone>/<YYYY-MM-DD>/<unix>-<uuid><ext>`.
pub async fn download(
    device: &DeviceInstance,
    message_id: &str,
    phone: &str,
    media_dir: &std::path::Path,
    account_validation: bool,
) -> ApiResult<DownloadResponse> {
    let phone = jid::sanitize_phone(phone);
    require(&[("message_id", message_id), ("phone", &phone)])?;
    let (client, chat) = target(device, &phone, account_validation).await?;
    let message = stored(device, &chat, message_id)
        .await
        .map_err(|e| ApiError::internal(format!("message not found: {}", e.message)))?
        .ok_or_else(|| {
            ApiError::internal(format!(
                "message with ID {message_id} not found in chat {chat} for current device"
            ))
        })?;
    if message.media_type.is_empty()
        || resolve_direct_path(&message.direct_path, &message.url).is_empty()
    {
        return Err(ApiError::internal(format!(
            "message {message_id} does not contain downloadable media"
        )));
    }

    let phone_dir: String = message
        .chat_jid
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    let day = chrono::DateTime::from_timestamp(message.timestamp, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string();
    let dir = media_dir.join(phone_dir).join(day);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::internal(format!("failed to create directory: {e}")))?;

    let (bytes, mime) = download_stored(&client, &message).await?;
    let filename = if message.media_type == "document" {
        message.filename.as_str()
    } else {
        ""
    };
    let name = format!(
        "{}-{}{}",
        crate::device::now(),
        uuid::Uuid::new_v4(),
        crate::media::extension_for(filename, &mime)
    );
    let path = dir.join(&name);
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| ApiError::internal(format!("failed to download media: {e}")))?;
    let file_path = path.to_string_lossy().into_owned();
    Ok(DownloadResponse {
        message_id: message_id.to_owned(),
        status: format!("Media downloaded successfully to {file_path}"),
        media_type: message.media_type,
        filename: name,
        file_path,
        file_url: String::new(),
        file_size: bytes.len() as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_tokens_like_gowa() {
        let l = "https://example.com";
        assert!(contains_link_token("https://example.com", l));
        assert!(contains_link_token("Visit (https://example.com).", l));
        assert!(contains_link_token("[label](https://example.com)", l));
        assert!(contains_link_token("see https://example.com, ok", l));
        assert!(!contains_link_token("https://example.com.evil", l));
        assert!(!contains_link_token(
            "https://evil.test/(https://example.com)",
            l
        ));
        assert!(!contains_link_token("?u=https://example.com", l));
        assert!(!contains_link_token("nothing here", l));
    }

    #[test]
    fn forwardable_rows() {
        let m = |media: &str, content: &str| Message {
            media_type: media.into(),
            content: content.into(),
            ..Default::default()
        };
        assert!(is_forwardable(&m("", "hi")));
        assert!(is_forwardable(&m("ptt", "")));
        assert!(!is_forwardable(&m("", "Contact: Bob (62)")));
        assert!(!is_forwardable(&m(
            "",
            "x — https://maps.google.com/?q=1,2"
        )));
        assert!(!is_forwardable(&m("call", "")));
        assert!(!is_forwardable(&m("", "  ")));
        assert_eq!(forward_content(&m("video_note", "")), "🎬 Video");
    }

    #[test]
    fn direct_path_fallback() {
        assert_eq!(resolve_direct_path(" /v/a ", "x"), "/v/a");
        assert_eq!(
            resolve_direct_path("", "https://mmg.whatsapp.net/v/t62/abc?ccb=1&oh=2"),
            "/v/t62/abc?ccb=1&oh=2"
        );
        assert_eq!(resolve_direct_path("", "not a url"), "");
    }

    #[test]
    fn forward_builds_media_with_stored_refs() {
        let m = Message {
            id: "X".into(),
            media_type: "ptt".into(),
            direct_path: "/v/1".into(),
            media_key: Some(vec![1]),
            file_length: 9,
            ..Default::default()
        };
        let msg = forward_message(&m, Some(86400), None).unwrap();
        let a = msg.audio_message.as_option().unwrap();
        assert_eq!(a.ptt, Some(true));
        assert_eq!(a.mimetype.as_deref(), Some("audio/ogg; codecs=opus"));
        assert_eq!(a.file_length, Some(9));
        let ci = a.context_info.as_option().unwrap();
        assert_eq!(
            (ci.is_forwarded, ci.forwarding_score, ci.expiration),
            (Some(true), Some(100), Some(86400))
        );
        let none = Message {
            media_type: "image".into(),
            ..Default::default()
        };
        assert!(forward_message(&none, None, None).is_err());
    }

    #[test]
    fn ozzo_join() {
        assert_eq!(
            require(&[("phone", ""), ("message_id", " ")])
                .unwrap_err()
                .message,
            "message_id: cannot be blank; phone: cannot be blank."
        );
    }
}
