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

fn is_url(s: &str) -> bool {
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

async fn upload(
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
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient);

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
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient);

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
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient);

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
    merge_reply(device, &mut ci, req.reply_message_id.as_deref(), &recipient);
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
    let (preview, thumbnail) = fetch::link_preview(&link)
        .await
        .map_err(|e| ApiError::internal(format!("failed to fetch link preview: {e}")))?;

    let mut ext = proto::message::ExtendedTextMessage::default();
    ext.text = Some(text.clone());
    ext.title = Some(preview.title);
    ext.matched_text = Some(link);
    ext.description = Some(preview.description);
    ext.jpeg_thumbnail = thumbnail;
    let ci = base_context(req.is_forwarded, req.duration);
    if ci != proto::ContextInfo::default() {
        ext.context_info = sdk::waproto::buffa::MessageField::some(ci);
    }
    let mut message = proto::Message::default();
    message.extended_text_message = sdk::waproto::buffa::MessageField::some(ext);

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
