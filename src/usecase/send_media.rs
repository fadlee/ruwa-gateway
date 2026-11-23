//! Media and structured sends (GOWA `SendImage`, `SendFile`, `SendVideo`,
//! `SendAudio`, `SendContact`, `SendLocation`, `SendLink`).

use std::path::Path;

use serde::Deserialize;

use super::send::{
    SendResponse, base_context, deliver, merge_reply, must_login, required, resolve_mentions,
    resolve_recipient, server_time, validate_allow_reshare, validate_duration, validate_mentions,
    validate_phone_number,
};
use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    media::{self, Upload, fetch, ffmpeg},
    wa::{
        jid, proto,
        sdk::{
            self,
            download::MediaType,
            media::{AudioOptions, DocumentOptions, VideoOptions},
            prelude::Client,
            upload::UploadOptions,
        },
    },
};

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ImageRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub caption: String,
    pub reply_message_id: Option<String>,
    pub mentions: Vec<String>,
    pub image_url: Option<String>,
    pub view_once: bool,
    /// GOWA presets `Compress = true` before binding.
    #[serde(default = "yes")]
    pub compress: bool,
    pub hd: bool,
    pub allow_reshare: bool,
}

impl Default for ImageRequest {
    fn default() -> Self {
        Self {
            phone: String::new(),
            duration: None,
            is_forwarded: false,
            caption: String::new(),
            reply_message_id: None,
            mentions: Vec::new(),
            image_url: None,
            view_once: false,
            compress: true,
            hd: false,
            allow_reshare: false,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub file_url: Option<String>,
    pub caption: String,
    pub reply_message_id: Option<String>,
    pub mentions: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct VideoRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub caption: String,
    pub reply_message_id: Option<String>,
    pub mentions: Vec<String>,
    pub view_once: bool,
    pub compress: bool,
    pub hd: bool,
    pub gif_playback: bool,
    pub video_url: Option<String>,
    pub allow_reshare: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AudioRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub audio_url: Option<String>,
    pub reply_message_id: Option<String>,
    pub ptt: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContactRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub contact_name: String,
    pub contact_phone: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LocationRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub latitude: String,
    pub longitude: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LinkRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub caption: String,
    pub link: String,
}

/// Request-scoped settings the usecases need.
#[derive(Debug, Clone, Copy)]
pub struct SendSettings {
    pub account_validation: bool,
}

pub(crate) fn is_url(s: &str) -> bool {
    reqwest::Url::parse(s)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
}

/// `either <File> or <FileURL> must be provided` + URL checks.
fn validate_source(
    file: Option<&Upload>,
    url: Option<&str>,
    file_label: &str,
    url_label: &str,
) -> ApiResult<()> {
    if file.is_none() && url.is_none_or(str::is_empty) {
        return Err(ApiError::validation(format!(
            "either {file_label} or {url_label} must be provided"
        )));
    }
    if let Some(url) = url {
        if url.is_empty() {
            return Err(ApiError::validation(format!("{url_label} cannot be empty")));
        }
        if !is_url(url) {
            return Err(ApiError::validation(format!(
                "{url_label} must be a valid URL"
            )));
        }
    }
    Ok(())
}

/// humanize.Bytes for the GOWA size messages (SI units).
fn human_bytes(n: usize) -> String {
    format!("{} MB", n / 1_000_000)
}

async fn source(
    file: Option<Upload>,
    url: Option<&str>,
    max: usize,
    what: &str,
) -> ApiResult<Upload> {
    if let Some(url) = url.filter(|u| !u.is_empty()) {
        return fetch::download(url, max)
            .await
            .map_err(|e| ApiError::internal(format!("failed to download {what} from URL: {e}")));
    }
    file.ok_or_else(|| ApiError::internal(format!("{what} is missing")))
}

/// Validate, log in, and resolve the recipient (common prologue).
async fn prologue(
    device: &DeviceInstance,
    phone: &str,
    settings: SendSettings,
) -> ApiResult<(std::sync::Arc<Client>, sdk::Jid)> {
    let client = must_login(device)?;
    let recipient = resolve_recipient(&client, phone, settings.account_validation).await?;
    Ok((client, recipient))
}

pub(crate) async fn upload(
    client: &Client,
    bytes: Vec<u8>,
    kind: MediaType,
) -> ApiResult<sdk::upload::UploadResponse> {
    client
        .upload(bytes, kind, UploadOptions::default())
        .await
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "UPLOAD_MEDIA_ERROR",
                e.to_string(),
            )
        })
}

fn boxed(ci: proto::ContextInfo) -> Option<Box<proto::ContextInfo>> {
    (ci != proto::ContextInfo::default()).then(|| Box::new(ci))
}

pub async fn send_image(
    device: &DeviceInstance,
    mut req: ImageRequest,
    file: Option<Upload>,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[("phone", &req.phone)])?;
    validate_phone_number(&req.phone)?;
    validate_source(file.as_ref(), req.image_url.as_deref(), "Image", "ImageURL")?;
    if let Some(f) = &file
        && !matches!(
            f.content_type.as_str(),
            "image/jpeg" | "image/jpg" | "image/png"
        )
    {
        return Err(ApiError::validation(
            "your image is not allowed. please use jpg/jpeg/png",
        ));
    }
    validate_duration(req.duration)?;
    validate_allow_reshare(req.allow_reshare, &req.phone)?;
    validate_mentions(&req.mentions)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let raw = source(
        file,
        req.image_url.as_deref(),
        media::MAX_IMAGE_SIZE,
        "image",
    )
    .await?;
    let (compress, hd) = (req.compress, req.hd);
    let prepared =
        tokio::task::spawn_blocking(move || media::image::prepare(raw.bytes, compress, hd))
            .await
            .map_err(ApiError::internal)?
            .map_err(|e| ApiError::internal(format!("failed to open image for sending: {e}")))?;

    let mut ci = base_context(req.is_forwarded, req.duration);
    let mentions = resolve_mentions(
        &client,
        &req.caption,
        &req.mentions,
        &recipient,
        settings.account_validation,
    )
    .await;
    if !mentions.is_empty() {
        ci.mentioned_jid = mentions;
    }
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient).await;

    let (width, height, thumbnail, mime) = (
        prepared.width,
        prepared.height,
        prepared.thumbnail,
        prepared.mime,
    );
    let uploaded = upload(&client, prepared.bytes, MediaType::Image).await?;
    let mut message = sdk::media::image_message(uploaded, {
        let mut o = sdk::media::ImageOptions::default();
        o.caption = Some(req.caption.clone());
        o.mimetype = Some(mime);
        o.jpeg_thumbnail = thumbnail;
        o.context_info = boxed(ci);
        o
    });
    if let Some(img) = message.image_message.as_option_mut() {
        img.width = Some(width);
        img.height = Some(height);
        img.view_once = Some(req.view_once);
    }

    let label = if req.caption.is_empty() {
        "🖼️ Image"
    } else {
        req.caption.as_str()
    };
    let message_id = deliver(device, &client, &recipient, message, label).await?;
    Ok(SendResponse {
        status: format!(
            "Message sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

pub async fn send_file(
    device: &DeviceInstance,
    mut req: FileRequest,
    file: Option<Upload>,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[("phone", &req.phone)])?;
    validate_phone_number(&req.phone)?;
    validate_source(file.as_ref(), req.file_url.as_deref(), "File", "FileURL")?;
    if let Some(f) = &file
        && f.bytes.len() > media::MAX_FILE_SIZE
    {
        let max = human_bytes(media::MAX_FILE_SIZE);
        return Err(ApiError::validation(format!(
            "max file upload is {max}, please upload in cloud and send via text if your file is higher than {max}"
        )));
    }
    validate_duration(req.duration)?;
    validate_mentions(&req.mentions)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let raw = source(file, req.file_url.as_deref(), media::MAX_FILE_SIZE, "file").await?;
    let mime = media::document_mime(&raw.filename, &raw.bytes);
    let thumbnail = if mime.starts_with("image/") {
        media::image::thumbnail_of(&raw.bytes)
    } else {
        None
    };

    let mut ci = base_context(req.is_forwarded, req.duration);
    let mentions = resolve_mentions(
        &client,
        &req.caption,
        &req.mentions,
        &recipient,
        settings.account_validation,
    )
    .await;
    if !mentions.is_empty() {
        ci.mentioned_jid = mentions;
    }
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient).await;

    let uploaded = upload(&client, raw.bytes, MediaType::Document).await?;
    let message = sdk::media::document_message(uploaded, {
        let mut o = DocumentOptions::default();
        o.mimetype = Some(mime);
        o.file_name = Some(raw.filename.clone());
        o.title = Some(raw.filename.clone());
        o.caption = Some(req.caption.clone());
        o.jpeg_thumbnail = thumbnail;
        o.context_info = boxed(ci);
        o
    });
    let label = if req.caption.is_empty() {
        format!("📄 {}", raw.filename)
    } else {
        req.caption.clone()
    };
    let message_id = deliver(device, &client, &recipient, message, &label).await?;
    Ok(SendResponse {
        status: format!(
            "Document sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

async fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> ApiResult<std::path::PathBuf> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let path = dir.join(format!("input.{ext}"));
    tokio::fs::write(&path, bytes)
        .await
        .map_err(ApiError::internal)?;
    Ok(path)
}

pub async fn send_video(
    device: &DeviceInstance,
    mut req: VideoRequest,
    file: Option<Upload>,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[("phone", &req.phone)])?;
    validate_phone_number(&req.phone)?;
    validate_source(file.as_ref(), req.video_url.as_deref(), "Video", "VideoURL")?;
    if let Some(f) = &file {
        if !matches!(
            f.content_type.as_str(),
            "video/mp4" | "video/x-matroska" | "video/avi" | "video/x-msvideo"
        ) {
            return Err(ApiError::validation(
                "your video type is not allowed. please use mp4/mkv/avi/x-msvideo",
            ));
        }
        if f.bytes.len() > media::MAX_VIDEO_SIZE {
            let max = human_bytes(media::MAX_VIDEO_SIZE);
            return Err(ApiError::validation(format!(
                "max video upload is {max}, please upload in cloud and send via text if your file is higher than {max}"
            )));
        }
    }
    validate_duration(req.duration)?;
    validate_allow_reshare(req.allow_reshare, &req.phone)?;
    validate_mentions(&req.mentions)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let raw = source(
        file,
        req.video_url.as_deref(),
        media::MAX_VIDEO_SIZE,
        "video",
    )
    .await?;
    if !ffmpeg::available() {
        return Err(ApiError::internal("ffmpeg not installed"));
    }
    let dir = tempfile::tempdir().map_err(ApiError::internal)?;
    let input = write_temp(dir.path(), &raw.filename, &raw.bytes).await?;
    let thumbnail = ffmpeg::video_thumbnail(&input)
        .await
        .map_err(|e| ApiError::internal(format!("failed to create thumbnail: {e}")))?;
    let bytes = match ffmpeg::transcode_video(&input, req.compress, req.hd).await {
        Ok(Some(out)) => out,
        Ok(None) => raw.bytes,
        Err(e) if req.hd => {
            return Err(ApiError::internal(format!(
                "failed to convert video to HD: {e}"
            )));
        }
        Err(e) => {
            tracing::warn!(error = %e, "video compression failed, sending original");
            raw.bytes
        }
    };
    let seconds = ffmpeg::duration_secs(&input).await;
    let mime = media::detect_mime(&bytes, &raw.filename);

    let mut ci = base_context(req.is_forwarded, req.duration);
    let mentions = resolve_mentions(
        &client,
        &req.caption,
        &req.mentions,
        &recipient,
        settings.account_validation,
    )
    .await;
    if !mentions.is_empty() {
        ci.mentioned_jid = mentions;
    }
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient).await;

    let uploaded = upload(&client, bytes, MediaType::Video).await?;
    let mut message = sdk::media::video_message(uploaded, {
        let mut o = VideoOptions::default();
        o.caption = Some(req.caption.clone());
        o.mimetype = Some(mime);
        o.jpeg_thumbnail = Some(thumbnail);
        o.duration_seconds = (seconds > 0).then_some(seconds);
        o.gif_playback = Some(req.gif_playback);
        o.context_info = boxed(ci);
        o
    });
    if let Some(v) = message.video_message.as_option_mut() {
        v.view_once = Some(req.view_once);
    }
    let label = if req.caption.is_empty() {
        "🎥 Video"
    } else {
        req.caption.as_str()
    };
    let message_id = deliver(device, &client, &recipient, message, label).await?;
    Ok(SendResponse {
        status: format!(
            "Video sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

const AUDIO_MIMES: [&str; 16] = [
    "audio/aac",
    "audio/amr",
    "audio/flac",
    "audio/m4a",
    "audio/m4r",
    "audio/mp3",
    "audio/mpeg",
    "audio/ogg",
    "audio/vnd.wav",
    "audio/vnd.wave",
    "audio/wav",
    "audio/wave",
    "audio/wma",
    "audio/x-ms-wma",
    "audio/x-pn-wav",
    "audio/x-wav",
];

pub async fn send_audio(
    device: &DeviceInstance,
    mut req: AudioRequest,
    file: Option<Upload>,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[("phone", &req.phone)])?;
    validate_phone_number(&req.phone)?;
    validate_source(file.as_ref(), req.audio_url.as_deref(), "Audio", "AudioURL")?;
    if let Some(f) = &file
        && !AUDIO_MIMES.contains(&f.content_type.as_str())
    {
        let list: String = AUDIO_MIMES.iter().map(|m| format!("{m},")).collect();
        return Err(ApiError::validation(format!(
            "your audio type is not allowed. please use ({list})"
        )));
    }
    validate_duration(req.duration)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let raw = source(
        file,
        req.audio_url.as_deref(),
        media::MAX_FILE_SIZE,
        "audio",
    )
    .await?;
    let mut mime = media::detect_mime(&raw.bytes, &raw.filename);
    let dir = tempfile::tempdir().map_err(ApiError::internal)?;
    let input = write_temp(dir.path(), &raw.filename, &raw.bytes).await?;
    let seconds = ffmpeg::duration_secs(&input).await;

    let mut bytes = raw.bytes;
    let mut waveform = None;
    if req.ptt {
        waveform = Some(ffmpeg::waveform(&input, seconds).await);
        if !mime.starts_with("audio/ogg") {
            if !ffmpeg::available() {
                return Err(ApiError::internal(
                    "ffmpeg not installed (required for PTT voice notes)",
                ));
            }
            bytes = ffmpeg::to_ogg_opus(&input).await.map_err(|e| {
                ApiError::internal(format!("failed to convert audio to OGG Opus for PTT: {e}"))
            })?;
        }
        mime = "audio/ogg; codecs=opus".to_owned();
    }

    let mut ci = base_context(req.is_forwarded, req.duration);
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient).await;
    let uploaded = upload(&client, bytes, MediaType::Audio).await?;
    let message = sdk::media::audio_message(uploaded, {
        let mut o = AudioOptions::default();
        o.mimetype = Some(mime);
        o.duration_seconds = Some(seconds);
        o.ptt = Some(req.ptt);
        o.waveform = waveform;
        o.context_info = boxed(ci);
        o
    });
    let message_id = deliver(device, &client, &recipient, message, "🎵 Audio").await?;
    Ok(SendResponse {
        status: format!(
            "Send audio success {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

pub async fn send_contact(
    device: &DeviceInstance,
    mut req: ContactRequest,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[
        ("phone", &req.phone),
        ("contact_phone", &req.contact_phone),
        ("contact_name", &req.contact_name),
    ])?;
    validate_phone_number(&req.phone)?;
    validate_phone_number(&req.contact_phone)
        .map_err(|e| ApiError::validation(format!("contact {}", e.message)))?;
    validate_duration(req.duration)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let name = req.contact_name.trim().to_owned();
    let phone = req.contact_phone.replace('+', "").trim().to_owned();
    let vcard = format!(
        "BEGIN:VCARD\nVERSION:3.0\nN:;{name};;;\nFN:{name}\nTEL;type=CELL;waid={phone}:+{phone}\nEND:VCARD"
    );

    let mut contact = proto::message::ContactMessage::default();
    contact.display_name = Some(name.clone());
    contact.vcard = Some(vcard);
    let ci = base_context(req.is_forwarded, req.duration);
    if ci != proto::ContextInfo::default() {
        contact.context_info = sdk::waproto::buffa::MessageField::some(ci);
    }
    let mut message = proto::Message::default();
    message.contact_message = sdk::waproto::buffa::MessageField::some(contact);

    let label = if phone.is_empty() {
        format!("👤 {name}")
    } else {
        format!("👤 {name} (+{phone})")
    };
    let message_id = deliver(device, &client, &recipient, message, &label).await?;
    Ok(SendResponse {
        status: format!(
            "Contact sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

fn parse_coordinate(value: &str, limit: f64, field: &str) -> ApiResult<f64> {
    match value.trim().parse::<f64>() {
        Ok(v) if v.abs() <= limit => Ok(v),
        _ => Err(ApiError::validation(format!(
            "{field}: must be a valid {field}."
        ))),
    }
}

pub async fn send_location(
    device: &DeviceInstance,
    mut req: LocationRequest,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[
        ("phone", &req.phone),
        ("latitude", &req.latitude),
        ("longitude", &req.longitude),
    ])?;
    let lat = parse_coordinate(&req.latitude, 90.0, "latitude")?;
    let lng = parse_coordinate(&req.longitude, 180.0, "longitude")?;
    validate_phone_number(&req.phone)?;
    validate_duration(req.duration)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let mut location = proto::message::LocationMessage::default();
    location.degrees_latitude = Some(lat);
    location.degrees_longitude = Some(lng);
    let ci = base_context(req.is_forwarded, req.duration);
    if ci != proto::ContextInfo::default() {
        location.context_info = sdk::waproto::buffa::MessageField::some(ci);
    }
    let mut message = proto::Message::default();
    message.location_message = sdk::waproto::buffa::MessageField::some(location);

    let label = format!("📍 {}, {}", req.latitude, req.longitude);
    let message_id = deliver(device, &client, &recipient, message, &label).await?;
    Ok(SendResponse {
        status: format!(
            "Send location success {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

/// GOWA `buildLinkPreviewMessage` (inline JPEG thumbnail only).
pub(crate) async fn link_preview_message(text: &str, link: &str) -> ApiResult<proto::Message> {
    let (preview, thumbnail) = fetch::link_preview(link)
        .await
        .map_err(|e| ApiError::internal(format!("failed to fetch link preview: {e}")))?;
    let mut ext = proto::message::ExtendedTextMessage::default();
    ext.text = Some(text.to_owned());
    ext.title = Some(preview.title);
    ext.matched_text = Some(link.to_owned());
    ext.description = Some(preview.description);
    ext.jpeg_thumbnail = thumbnail;
    let mut message = proto::Message::default();
    message.extended_text_message = sdk::waproto::buffa::MessageField::some(ext);
    Ok(message)
}

pub async fn send_link(
    device: &DeviceInstance,
    mut req: LinkRequest,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[
        ("caption", &req.caption),
        ("link", &req.link),
        ("phone", &req.phone),
    ])?;
    if !is_url(req.link.trim()) {
        return Err(ApiError::validation("link: must be a valid URL."));
    }
    validate_phone_number(&req.phone)?;
    validate_duration(req.duration)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let link = req.link.trim().to_owned();
    let caption = req.caption.trim();
    let text = if caption.is_empty() {
        link.clone()
    } else {
        format!("{caption}\n{link}")
    };
    let mut message = link_preview_message(&text, &link).await?;
    let ci = base_context(req.is_forwarded, req.duration);
    if ci != proto::ContextInfo::default()
        && let Some(ext) = message.extended_text_message.as_option_mut()
    {
        ext.context_info = sdk::waproto::buffa::MessageField::some(ci);
    }

    let message_id = deliver(device, &client, &recipient, message, &text).await?;
    Ok(SendResponse {
        status: format!(
            "Link sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_validation_messages() {
        assert_eq!(
            validate_source(None, None, "Image", "ImageURL")
                .unwrap_err()
                .message,
            "either Image or ImageURL must be provided"
        );
        assert_eq!(
            validate_source(None, Some("nope"), "Image", "ImageURL")
                .unwrap_err()
                .message,
            "ImageURL must be a valid URL"
        );
        assert!(validate_source(None, Some("https://x.y/a.png"), "Image", "ImageURL").is_ok());
        assert!(validate_source(Some(&Upload::default()), None, "Image", "ImageURL").is_ok());
    }

    #[test]
    fn image_compress_defaults_true() {
        let r: ImageRequest = serde_json::from_str(r#"{"phone":"1"}"#).unwrap();
        assert!(r.compress);
        let r: ImageRequest = serde_json::from_str(r#"{"phone":"1","compress":false}"#).unwrap();
        assert!(!r.compress);
    }

    #[test]
    fn coordinates() {
        assert_eq!(parse_coordinate("-6.2", 90.0, "latitude").unwrap(), -6.2);
        assert!(parse_coordinate("91", 90.0, "latitude").is_err());
        assert!(parse_coordinate("abc", 180.0, "longitude").is_err());
    }

    #[test]
    fn sizes_render_like_humanize() {
        assert_eq!(human_bytes(media::MAX_FILE_SIZE), "50 MB");
        assert_eq!(human_bytes(media::MAX_VIDEO_SIZE), "100 MB");
    }
}

#[cfg(test)]
mod form_tests {
    use super::*;

    #[test]
    fn form_binding_with_flattened_base() {
        let r: VideoRequest = serde_html_form::from_str(
            "phone=628&duration=86400&is_forwarded=true&mentions=1&mentions=2&gif_playback=true",
        )
        .unwrap();
        assert_eq!(r.duration, Some(86400));
        assert!(r.is_forwarded);
        assert!(r.gif_playback);
        assert_eq!(r.mentions, vec!["1", "2"]);
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PresenceRequest {
    #[serde(rename = "type")]
    pub kind: String,
}

/// `POST /send/presence` (GOWA `SendPresence`).
pub async fn send_presence(
    device: &DeviceInstance,
    req: PresenceRequest,
) -> ApiResult<SendResponse> {
    if !matches!(req.kind.as_str(), "available" | "unavailable") {
        super::chat::ozzo([("type", "must be a valid value".to_owned())].into())?;
    }
    let client = must_login(device)?;
    let result = if req.kind == "available" {
        client.presence().set_available().await
    } else {
        client.presence().set_unavailable().await
    };
    result.map_err(ApiError::internal)?;
    Ok(SendResponse {
        message_id: "presence".into(),
        status: format!("Send presence success {}", req.kind),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChatPresenceRequest {
    pub phone: String,
    pub action: String,
}

/// `POST /send/chat-presence` (GOWA `SendChatPresence`): typing indicator.
pub async fn send_chat_presence(
    device: &DeviceInstance,
    mut req: ChatPresenceRequest,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[("phone", &req.phone), ("action", &req.action)])?;
    if !matches!(req.action.as_str(), "start" | "stop") {
        super::chat::ozzo([("action", "must be a valid value".to_owned())].into())?;
    }
    validate_phone_number(&req.phone)?;
    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let (message_id, verb) = if req.action == "start" {
        client
            .chatstate()
            .send_composing(&recipient)
            .await
            .map_err(ApiError::internal)?;
        ("chat-presence-start", "start")
    } else {
        client
            .chatstate()
            .send_paused(&recipient)
            .await
            .map_err(ApiError::internal)?;
        ("chat-presence-stop", "stop")
    };
    Ok(SendResponse {
        message_id: message_id.into(),
        status: format!("Send chat presence {verb} typing success {}", req.phone),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct StickerRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub is_forwarded: bool,
    pub reply_message_id: Option<String>,
    pub sticker_url: Option<String>,
}

/// GOWA animated-sticker limit.
const ANIMATED_STICKER_MAX: usize = 500 * 1024;

/// `POST /send/sticker` (GOWA `SendSticker`).
pub async fn send_sticker(
    device: &DeviceInstance,
    mut req: StickerRequest,
    file: Option<Upload>,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    required(&[("phone", &req.phone)])?;
    validate_phone_number(&req.phone)?;
    let has_url = req.sticker_url.as_deref().is_some_and(|u| !u.is_empty());
    if file.is_none() && !has_url {
        return Err(ApiError::validation(
            "either Sticker or StickerURL must be provided",
        ));
    }
    if file.is_some() && has_url {
        return Err(ApiError::validation(
            "cannot provide both Sticker file and StickerURL",
        ));
    }
    if let Some(f) = &file
        && !matches!(
            f.content_type.as_str(),
            "image/jpeg" | "image/jpg" | "image/png" | "image/webp" | "image/gif"
        )
    {
        return Err(ApiError::validation(
            "your sticker is not allowed. please use jpg/jpeg/png/webp/gif",
        ));
    }
    if has_url && !is_url(req.sticker_url.as_deref().unwrap_or_default()) {
        return Err(ApiError::validation("StickerURL must be a valid URL"));
    }
    validate_duration(req.duration)?;

    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let raw = source(
        file,
        req.sticker_url.as_deref(),
        media::MAX_IMAGE_SIZE,
        "sticker",
    )
    .await?;

    let (webp, width, height, animated) = match media::image::webp_info(&raw.bytes) {
        Some((true, w, h)) => {
            if w != 512 || h != 512 {
                return Err(ApiError::validation(format!(
                    "animated WebP stickers must be exactly 512x512 pixels (got {w}x{h}). Please resize your sticker before uploading."
                )));
            }
            if raw.bytes.len() > ANIMATED_STICKER_MAX {
                return Err(ApiError::validation(format!(
                    "animated WebP stickers must be under 500KB (got {} KB). Please reduce the file size.",
                    raw.bytes.len() / 1024
                )));
            }
            (raw.bytes, w, h, true)
        }
        _ => {
            let (png, w, h) =
                tokio::task::spawn_blocking(move || media::image::sticker_png(&raw.bytes))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| {
                        ApiError::internal(format!(
                            "failed to open image for sticker conversion: {e}"
                        ))
                    })?;
            let webp = if ffmpeg::available() {
                ffmpeg::png_to_webp(&png).await.map_err(|e| {
                    ApiError::internal(format!("failed to convert sticker to WebP: {e}"))
                })?
            } else {
                media::image::png_to_webp_lossless(&png).map_err(|e| {
                    ApiError::internal(format!("failed to convert sticker to WebP: {e}"))
                })?
            };
            (webp, w, h, false)
        }
    };

    let mut ci = base_context(req.is_forwarded, req.duration);
    if req.is_forwarded {
        ci.forwarding_score = Some(100);
    }
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient).await;

    let uploaded = upload(&client, webp, MediaType::Image).await?;
    let mut sticker = proto::message::StickerMessage::default();
    sticker.url = Some(uploaded.url);
    sticker.direct_path = Some(uploaded.direct_path);
    sticker.mimetype = Some("image/webp".into());
    sticker.file_length = Some(uploaded.file_length);
    sticker.file_sha256 = Some(uploaded.file_sha256.to_vec());
    sticker.file_enc_sha256 = Some(uploaded.file_enc_sha256.to_vec());
    sticker.media_key = Some(uploaded.media_key.to_vec());
    sticker.media_key_timestamp = Some(uploaded.media_key_timestamp);
    sticker.width = Some(width);
    sticker.height = Some(height);
    sticker.is_animated = Some(animated);
    if ci != proto::ContextInfo::default() {
        sticker.context_info = sdk::waproto::buffa::MessageField::some(ci);
    }
    let mut message = proto::Message::default();
    message.sticker_message = sdk::waproto::buffa::MessageField::some(sticker);

    let label = if animated {
        "🎨 Animated Sticker"
    } else {
        "🎨 Sticker"
    };
    let message_id = deliver(device, &client, &recipient, message, label).await?;
    let status = if animated {
        format!(
            "Animated sticker sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        )
    } else {
        format!(
            "Sticker sent to {} (server timestamp: {})",
            req.phone,
            server_time()
        )
    };
    Ok(SendResponse { message_id, status })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PollRequest {
    pub phone: String,
    pub duration: Option<i64>,
    pub question: String,
    pub options: Vec<String>,
    pub max_answer: i64,
}

/// GOWA `ValidateSendPoll` (ozzo messages, same order of checks).
fn validate_poll(req: &PollRequest) -> ApiResult<()> {
    use std::collections::BTreeMap;
    if req.options.is_empty() {
        return Err(ApiError::validation("options: cannot be blank."));
    }
    if req.max_answer > i64::from(u32::MAX) {
        return Err(ApiError::validation(format!(
            "max_answer: must be no greater than {}.",
            u32::MAX
        )));
    }
    let mut errors: BTreeMap<&str, String> = BTreeMap::new();
    if req.phone.is_empty() {
        errors.insert("phone", "cannot be blank".into());
    }
    if req.question.is_empty() {
        errors.insert("question", "cannot be blank".into());
    }
    let blank: Vec<String> = req
        .options
        .iter()
        .enumerate()
        .filter(|(_, o)| o.is_empty())
        .map(|(i, _)| format!("{i}: cannot be blank."))
        .collect();
    if !blank.is_empty() {
        errors.insert("options", format!("({})", blank.join("; ")));
    }
    let max = req.options.len() as i64;
    if req.max_answer == 0 {
        errors.insert("max_answer", "cannot be blank".into());
    } else if req.max_answer < 1 {
        errors.insert("max_answer", "must be no less than 1".into());
    } else if req.max_answer > max {
        errors.insert("max_answer", format!("must be no greater than {max}"));
    }
    super::chat::ozzo(errors)?;
    validate_phone_number(&req.phone)?;
    validate_duration(req.duration)?;
    let mut seen = std::collections::HashSet::new();
    if !req.options.iter().all(|o| seen.insert(o)) {
        return Err(ApiError::validation("options should be unique"));
    }
    Ok(())
}

/// Poll creation message as WA Web / whatsmeow `BuildPollCreation`: v3 for a
/// single-select poll, v1 otherwise, plus a fresh 32-byte message secret.
fn poll_message(
    question: &str,
    options: &[String],
    max_answer: u32,
    duration: Option<i64>,
) -> proto::Message {
    use sdk::waproto::buffa::MessageField;
    let mut poll = proto::message::PollCreationMessage::default();
    poll.name = Some(question.to_owned());
    poll.options = options
        .iter()
        .map(|name| {
            let mut o = proto::message::poll_creation_message::Option::default();
            o.option_name = Some(name.clone());
            o
        })
        .collect();
    poll.selectable_options_count = Some(max_answer);
    poll.poll_content_type = Some(proto::message::PollContentType::TEXT);
    if let Some(d) = duration.filter(|d| *d > 0) {
        let mut ci = proto::ContextInfo::default();
        ci.expiration = Some(d as u32);
        poll.context_info = MessageField::some(ci);
    }
    let mut message = proto::Message::default();
    if max_answer == 1 {
        message.poll_creation_message_v3 = MessageField::some(poll);
    } else {
        message.poll_creation_message = MessageField::some(poll);
    }
    let mut secret = uuid::Uuid::new_v4().as_bytes().to_vec();
    secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let mut info = proto::MessageContextInfo::default();
    info.message_secret = Some(secret);
    message.message_context_info = MessageField::some(info);
    message
}

/// `POST /send/poll` (GOWA `SendPoll`). Vote resolution storage
/// (`poll_definitions`) is not kept yet.
pub async fn send_poll(
    device: &DeviceInstance,
    mut req: PollRequest,
    settings: SendSettings,
) -> ApiResult<SendResponse> {
    req.phone = jid::sanitize_phone(&req.phone);
    validate_poll(&req)?;
    let (client, recipient) = prologue(device, &req.phone, settings).await?;
    let message = poll_message(
        &req.question,
        &req.options,
        req.max_answer as u32,
        req.duration,
    );
    let label = format!("📊 {}", req.question);
    let message_id = deliver(device, &client, &recipient, message, &label).await?;
    Ok(SendResponse {
        status: format!(
            "Send poll success {} (server timestamp: {})",
            req.phone,
            server_time()
        ),
        message_id,
    })
}

#[cfg(test)]
mod poll_tests {
    use super::*;

    #[test]
    fn poll_message_shape() {
        let opts = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let single = poll_message("q", &opts, 1, None);
        assert!(single.poll_creation_message_v3.is_set());
        assert!(single.poll_creation_message.is_unset());

        let multi = poll_message("q", &opts, 2, Some(86_400));
        let poll = multi.poll_creation_message.as_option().expect("v1 poll");
        assert_eq!(poll.name.as_deref(), Some("q"));
        assert_eq!(poll.options.len(), 3);
        assert_eq!(poll.selectable_options_count, Some(2));
        assert_eq!(
            poll.context_info.as_option().and_then(|c| c.expiration),
            Some(86_400)
        );
        let secret = multi.message_context_info.as_option().unwrap();
        assert_eq!(secret.message_secret.as_ref().map(Vec::len), Some(32));
        // Each poll gets its own secret.
        assert_ne!(
            secret.message_secret,
            single
                .message_context_info
                .as_option()
                .unwrap()
                .message_secret
        );
    }
}
