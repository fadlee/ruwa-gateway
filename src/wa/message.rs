//! Read helpers over the `wa::Message` proto, mirroring GOWA `pkg/utils`
//! (`UnwrapMessage`, `BuildEventMessage`, `ExtractMessageTextFromProto`).

use whatsapp_rust::wacore::proto_helpers::MessageExt;

use super::proto as wa;

/// Innermost message, with ephemeral/view-once/device-sent wrappers removed.
pub fn unwrap(msg: &wa::Message) -> &wa::Message {
    msg.get_base_message()
}

/// Text body: `conversation` or `extendedTextMessage.text` (GOWA `BuildEventMessage`).
pub fn text(msg: &wa::Message) -> Option<&str> {
    let base = unwrap(msg);
    if let Some(ext) = base.extended_text_message.as_option() {
        return ext.text.as_deref().filter(|t| !t.is_empty());
    }
    base.conversation.as_deref().filter(|t| !t.is_empty())
}

/// Caption of image/video/document messages.
pub fn caption(msg: &wa::Message) -> Option<&str> {
    msg.get_caption().filter(|c| !c.is_empty())
}

/// GOWA `ExtractMessageTextFromProto`: best-effort text of a (quoted) message.
pub fn any_text(msg: &wa::Message) -> Option<&str> {
    let base = unwrap(msg);
    if let Some(t) = base.conversation.as_deref().filter(|t| !t.is_empty()) {
        return Some(t);
    }
    if let Some(ext) = base.extended_text_message.as_option() {
        if let Some(t) = ext.text.as_deref().filter(|t| !t.is_empty()) {
            return Some(t);
        }
        if let Some(t) = ext.matched_text.as_deref().filter(|t| !t.is_empty()) {
            return Some(t);
        }
    }
    caption(base)
}

pub fn context_info(msg: &wa::Message) -> Option<&wa::ContextInfo> {
    msg.context_info()
}

pub fn is_forwarded(msg: &wa::Message) -> bool {
    msg.is_forwarded()
}

pub fn is_view_once(msg: &wa::Message) -> bool {
    msg.is_view_once()
}

/// Build a plain text message the way GOWA `SendText` does: always an
/// `extendedTextMessage` with an (initially empty) `contextInfo`.
pub fn extended_text(text: impl Into<String>, context: wa::ContextInfo) -> wa::Message {
    let mut ext = wa::message::ExtendedTextMessage::default();
    ext.text = Some(text.into());
    ext.context_info = whatsapp_rust::waproto::buffa::MessageField::some(context);
    let mut msg = wa::Message::default();
    msg.extended_text_message = whatsapp_rust::waproto::buffa::MessageField::some(ext);
    msg
}

/// Quoted-message stub used for replies (GOWA stores only the text).
pub fn conversation(text: impl Into<String>) -> wa::Message {
    let mut msg = wa::Message::default();
    msg.conversation = Some(text.into());
    msg
}

// ---- chat-storage extraction (GOWA `pkg/utils`) ----

/// GOWA `ExtractMessageTextFromProto`, as stored in chat storage. Business
/// (template/interactive/list/buttons) summaries are not ported yet.
pub fn storage_text(msg: &wa::Message) -> String {
    let base = unwrap(msg);
    if let Some(t) = any_text(base) {
        return t.to_owned();
    }
    if let Some(c) = base.contact_message.as_option() {
        return contact_summary(
            c.display_name.as_deref().unwrap_or_default(),
            &vcard_phone(c.vcard.as_deref().unwrap_or_default()),
            false,
        );
    }
    if let Some(arr) = base.contacts_array_message.as_option() {
        return match arr.contacts.first() {
            Some(first) => contact_summary(
                first.display_name.as_deref().unwrap_or_default(),
                &vcard_phone(first.vcard.as_deref().unwrap_or_default()),
                true,
            ),
            None => "Contacts shared".to_owned(),
        };
    }
    if let Some(loc) = base.location_message.as_option() {
        return location_summary(
            loc.name.as_deref().unwrap_or_default(),
            loc.address.as_deref().unwrap_or_default(),
            loc.degrees_latitude.unwrap_or_default(),
            loc.degrees_longitude.unwrap_or_default(),
        );
    }
    if let Some(live) = base.live_location_message.as_option() {
        return location_summary(
            live.caption.as_deref().unwrap_or_default(),
            "",
            live.degrees_latitude.unwrap_or_default(),
            live.degrees_longitude.unwrap_or_default(),
        );
    }
    String::new()
}

/// GOWA `FormatContactSummary`.
pub fn contact_summary(name: &str, phone: &str, plural: bool) -> String {
    let (name, phone) = (name.trim(), phone.trim());
    let prefix = if plural { "Contacts" } else { "Contact" };
    match (name.is_empty(), phone.is_empty()) {
        (false, false) => format!("{prefix}: {name} ({phone})"),
        (false, true) => format!("{prefix}: {name}"),
        (true, false) => format!("{prefix}: {phone}"),
        (true, true) => format!("{prefix} shared"),
    }
}

/// GOWA `FormatLocationSummary` (Go `%g` and Rust `{}` agree on the
/// shortest round-trip form for coordinates).
pub fn location_summary(name: &str, address: &str, lat: f64, long: f64) -> String {
    let mut parts: Vec<String> = [name.trim(), address.trim()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    parts.push(format!("https://maps.google.com/?q={lat},{long}"));
    parts.join(" — ")
}

/// GOWA `ExtractPhoneFromVCard`: value of the first `TEL` property
/// (folded lines joined, `item1.TEL` group prefixes allowed).
pub fn vcard_phone(vcard: &str) -> String {
    let normalized = vcard.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    for raw in normalized.split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if raw.starts_with([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(line);
            }
            continue;
        }
        lines.push(line.to_owned());
    }
    for line in &lines {
        let name = line.split([';', ':']).next().unwrap_or_default();
        let name = name.rsplit('.').next().unwrap_or_default();
        if name.trim().eq_ignore_ascii_case("TEL")
            && let Some((_, value)) = line.rsplit_once(':')
        {
            return value.trim().to_owned();
        }
    }
    String::new()
}

/// Media columns of a stored message (GOWA `ExtractMediaInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaInfo {
    pub media_type: &'static str,
    pub filename: String,
    pub url: String,
    pub direct_path: String,
    pub media_key: Option<Vec<u8>>,
    pub file_sha256: Option<Vec<u8>>,
    pub file_enc_sha256: Option<Vec<u8>>,
    pub file_length: u64,
}

/// GOWA `GenerateMediaFilename`: `<type>_<YYYYmmdd_HHMMSS>[_<caption>][.<ext>]`.
pub fn media_filename(media_type: &str, ext: &str, caption: &str) -> String {
    let mut name = format!(
        "{media_type}_{}",
        chrono::Utc::now().format("%Y%m%d_%H%M%S")
    );
    if !caption.is_empty() {
        let clean: String = caption
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(30)
            .collect();
        name.push('_');
        name.push_str(&clean);
    }
    if !ext.is_empty() {
        name.push('.');
        name.push_str(ext);
    }
    name
}

pub fn media_info(msg: &wa::Message) -> Option<MediaInfo> {
    macro_rules! info {
        ($m:expr, $kind:expr, $filename:expr) => {
            MediaInfo {
                media_type: $kind,
                filename: $filename,
                url: $m.url.clone().unwrap_or_default(),
                direct_path: $m.direct_path.clone().unwrap_or_default(),
                media_key: $m.media_key.clone(),
                file_sha256: $m.file_sha256.clone(),
                file_enc_sha256: $m.file_enc_sha256.clone(),
                file_length: $m.file_length.unwrap_or_default(),
            }
        };
    }
    let base = unwrap(msg);
    if let Some(m) = base.image_message.as_option() {
        let name = media_filename("image", "jpg", m.caption.as_deref().unwrap_or_default());
        return Some(info!(m, "image", name));
    }
    if let Some(m) = base.video_message.as_option() {
        let name = media_filename("video", "mp4", m.caption.as_deref().unwrap_or_default());
        return Some(info!(m, "video", name));
    }
    if let Some(m) = base.ptv_message.as_option() {
        let name = media_filename(
            "video_note",
            "mp4",
            m.caption.as_deref().unwrap_or_default(),
        );
        return Some(info!(m, "video_note", name));
    }
    if let Some(m) = base.audio_message.as_option() {
        return Some(info!(m, "audio", media_filename("audio", "ogg", "")));
    }
    if let Some(m) = base.document_message.as_option() {
        let name = match m.file_name.as_deref().filter(|n| !n.is_empty()) {
            Some(n) => n.to_owned(),
            None => media_filename("document", "", m.title.as_deref().unwrap_or_default()),
        };
        return Some(info!(m, "document", name));
    }
    if let Some(m) = base.sticker_message.as_option() {
        return Some(info!(m, "sticker", media_filename("sticker", "webp", "")));
    }
    None
}

/// GOWA `ExtractContextMetadata`: `{"replied_to_id","replied_to_sender"?}`
/// JSON, or empty when the message is not a reply.
pub fn context_metadata(msg: &wa::Message) -> String {
    let Some(ci) = context_info(msg) else {
        return String::new();
    };
    let Some(id) = ci.stanza_id.as_deref().filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let mut v = serde_json::json!({ "replied_to_id": id });
    if let Some(p) = ci.participant.as_deref().filter(|p| !p.is_empty()) {
        v["replied_to_sender"] = p.into();
    }
    v.to_string()
}

/// GOWA `ExtractEphemeralExpiration`: the context timer, else a protocol
/// message's ephemeral setting.
pub fn ephemeral_expiration(msg: &wa::Message) -> u32 {
    if let Some(exp) = context_info(msg)
        .and_then(|ci| ci.expiration)
        .filter(|e| *e != 0)
    {
        return exp;
    }
    unwrap(msg)
        .protocol_message
        .as_option()
        .and_then(|pm| pm.ephemeral_expiration)
        .unwrap_or_default()
}

fn protocol(
    msg: &wa::Message,
    kind: wa::message::protocol_message::Type,
) -> Option<&wa::message::ProtocolMessage> {
    unwrap(msg)
        .protocol_message
        .as_option()
        .filter(|pm| pm.r#type == Some(kind))
}

/// Id of the message a revoke removes.
pub fn revoked_id(msg: &wa::Message) -> Option<&str> {
    protocol(msg, wa::message::protocol_message::Type::Revoke)?
        .key
        .as_option()?
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
}

/// Author named by a revoke's key (`participant`), used for admin deletes.
pub fn revoked_participant(msg: &wa::Message) -> Option<&str> {
    protocol(msg, wa::message::protocol_message::Type::Revoke)?
        .key
        .as_option()?
        .participant
        .as_deref()
        .filter(|p| !p.is_empty())
}

/// `(original id, new message)` of an edit.
pub fn edit(msg: &wa::Message) -> Option<(&str, &wa::Message)> {
    let pm = protocol(msg, wa::message::protocol_message::Type::MessageEdit)?;
    let id = pm
        .key
        .as_option()?
        .id
        .as_deref()
        .filter(|id| !id.is_empty())?;
    Some((id, pm.edited_message.as_option()?))
}

/// A reaction: `(reacted message key, emoji)`; empty emoji removes it.
pub fn reaction(msg: &wa::Message) -> Option<(&wa::MessageKey, &str)> {
    let r = unwrap(msg).reaction_message.as_option()?;
    Some((r.key.as_option()?, r.text.as_deref().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::waproto::buffa::MessageField;

    #[test]
    fn vcard_phone_like_gowa() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Bob\r\nitem1.TEL;waid=628:+62 812\r\n 345\r\nEND:VCARD";
        assert_eq!(vcard_phone(v), "+62 812345");
        assert_eq!(vcard_phone("TELEX:1"), "");
        assert_eq!(vcard_phone(""), "");
    }

    #[test]
    fn summaries() {
        assert_eq!(contact_summary("Bob", "62", false), "Contact: Bob (62)");
        assert_eq!(contact_summary("", "", true), "Contacts shared");
        assert_eq!(
            location_summary("Monas", "", -6.175392, 106.827153),
            "Monas — https://maps.google.com/?q=-6.175392,106.827153"
        );
        assert_eq!(
            location_summary("", "", 1.0, 2.5),
            "https://maps.google.com/?q=1,2.5"
        );
    }

    #[test]
    fn media_and_text_through_wrappers() {
        let mut img = wa::message::ImageMessage::default();
        img.caption = Some("hi there!".into());
        img.direct_path = Some("/v/t62".into());
        img.file_length = Some(42);
        let mut inner = wa::Message::default();
        inner.image_message = MessageField::some(img);
        let mut eph = wa::message::FutureProofMessage::default();
        eph.message = MessageField::some(inner);
        let mut msg = wa::Message::default();
        msg.ephemeral_message = MessageField::some(eph);

        assert_eq!(storage_text(&msg), "hi there!");
        let info = media_info(&msg).unwrap();
        assert_eq!(info.media_type, "image");
        assert_eq!(info.direct_path, "/v/t62");
        assert_eq!(info.file_length, 42);
        assert!(info.filename.starts_with("image_") && info.filename.ends_with("_hi_there_.jpg"));
    }

    #[test]
    fn reply_context_json() {
        let mut ci = wa::ContextInfo::default();
        ci.stanza_id = Some("ABC".into());
        ci.participant = Some("628@s.whatsapp.net".into());
        ci.expiration = Some(86400);
        let msg = extended_text("x", ci);
        assert_eq!(
            context_metadata(&msg),
            r#"{"replied_to_id":"ABC","replied_to_sender":"628@s.whatsapp.net"}"#
        );
        assert_eq!(ephemeral_expiration(&msg), 86400);
        assert_eq!(context_metadata(&conversation("x")), "");
    }
}
