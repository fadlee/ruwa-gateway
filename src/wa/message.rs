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
