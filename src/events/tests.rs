//! Golden tests: each case rebuilds a JSON example from
//! `docs/upstream/webhook-payload.md` and compares field by field.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;
use crate::wa::{message as msgx, proto};

const DOC: &str = include_str!("../../docs/upstream/webhook-payload.md");

/// First ```json block after the `### {heading}` line of the upstream doc.
fn doc_example(heading: &str) -> Value {
    let marker = format!("### {heading}\n");
    let start = DOC
        .find(&marker)
        .unwrap_or_else(|| panic!("heading {heading:?} not in doc"));
    let rest = &DOC[start..];
    let open = rest.find("```json\n").expect("json block") + "```json\n".len();
    let close = rest[open..].find("```").expect("block end") + open;
    serde_json::from_str(&rest[open..close]).expect("valid json example")
}

/// Fields GOWA always emits but some doc examples leave out.
const ALWAYS_EMITTED: [&str; 1] = ["sender_display_name"];

fn assert_matches_doc(actual: &Value, expected: &Value) {
    let (Value::Object(a), Value::Object(e)) = (actual, expected) else {
        assert_eq!(actual, expected);
        return;
    };
    for (key, ev) in e {
        let av = a
            .get(key)
            .unwrap_or_else(|| panic!("missing key {key:?} in {actual:#}"));
        assert_matches_doc(av, ev);
    }
    for key in a.keys() {
        assert!(
            e.contains_key(key) || ALWAYS_EMITTED.contains(&key.as_str()),
            "unexpected key {key:?} in {actual:#}"
        );
    }
}

#[derive(Default)]
struct FakeResolver {
    lids: HashMap<String, String>,
    display: String,
}

impl FakeResolver {
    fn new(lids: &[(&str, &str)]) -> Self {
        Self {
            lids: lids
                .iter()
                .map(|(l, p)| (l.to_string(), p.to_string()))
                .collect(),
            display: "Saved Contact".into(),
        }
    }
}

impl Resolver for FakeResolver {
    async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
        self.lids
            .get(lid.user.as_str())
            .map(|pn| Jid::pn(pn.as_str()))
    }

    async fn display_name(&self, _: &Jid, _: bool, _: &str) -> String {
        self.display.clone()
    }
}

fn jid(s: &str) -> Jid {
    s.parse().unwrap()
}

fn secs(rfc: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc)
        .unwrap()
        .timestamp()
}

fn incoming(
    id: &str,
    chat: &str,
    sender: &str,
    push: &str,
    ts: &str,
    message: proto::Message,
) -> IncomingMessage {
    IncomingMessage {
        id: id.into(),
        chat: jid(chat),
        sender: jid(sender),
        is_from_me: false,
        push_name: push.into(),
        timestamp: secs(ts),
        verified_name: None,
        verified_level: None,
        is_business: false,
        message: Arc::new(message),
    }
}

async fn message_envelope(r: &FakeResolver, m: &IncomingMessage, device: &str) -> Value {
    let (event, payload) = message_payload(r, m).await;
    Value::Object(envelope(event, device, payload))
}

#[tokio::test]
async fn text_message() {
    let r = FakeResolver::new(&[("251556368777322", "628123456789")]);
    let m = incoming(
        "3EB0C127D7BACC83D6A1",
        "628987654321@s.whatsapp.net",
        "251556368777322@lid",
        "John Doe",
        "2023-10-15T10:30:00Z",
        msgx::conversation("Hello, how are you?"),
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Text Message"));
}

#[tokio::test]
async fn reply_message() {
    let r = FakeResolver::new(&[]);
    let mut ci = proto::ContextInfo::default();
    ci.stanza_id = Some("3EB0C127D7BACC83D6A1".into());
    ci.participant = Some("628987654321@s.whatsapp.net".into());
    ci.quoted_message = crate::wa::sdk::waproto::buffa::MessageField::some(msgx::conversation(
        "Hello, how are you?",
    ));
    let m = incoming(
        "3EB0C127D7BACC83D6A2",
        "628987654321@s.whatsapp.net",
        "628123456789@s.whatsapp.net",
        "John Doe",
        "2023-10-15T10:35:00Z",
        msgx::extended_text("I'm doing great, thanks!", ci),
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Reply Message"));
}

#[tokio::test]
async fn mentions() {
    let r = FakeResolver::new(&[("251556368777322", "628111111111")]);
    let mut ci = proto::ContextInfo::default();
    ci.mentioned_jid = vec![
        "628987654321@s.whatsapp.net".into(),
        "251556368777322@lid".into(),
        "628987654321@s.whatsapp.net".into(),
    ];
    let m = incoming(
        "3EB0C127D7BACC83D6A3",
        "120363012345678901@g.us",
        "628123456789@s.whatsapp.net",
        "",
        "2023-10-15T10:36:00Z",
        msgx::extended_text("@628987654321 can you check this, @251556368777322?", ci),
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Mentions"));
}

#[tokio::test]
async fn reaction_message() {
    let r = FakeResolver::new(&[]);
    let mut reaction = proto::message::ReactionMessage::default();
    reaction.text = Some("👍".into());
    let mut key = proto::MessageKey::default();
    key.id = Some("3EB0C127D7BACC83D6A1".into());
    reaction.key = crate::wa::sdk::waproto::buffa::MessageField::some(key);
    let mut message = proto::Message::default();
    message.reaction_message = crate::wa::sdk::waproto::buffa::MessageField::some(reaction);
    let m = incoming(
        "88760C69D1F35FEB239102699AE9XXXX",
        "628987654321@s.whatsapp.net",
        "628123456789@s.whatsapp.net",
        "John Doe",
        "2023-10-15T10:40:00Z",
        message,
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Reaction Message"));
}

fn receipt(kind: &str, ts: &str) -> ReceiptView {
    ReceiptView {
        chat: jid("120363402106@g.us"),
        sender: jid("6289685000000@s.whatsapp.net"),
        is_from_me: false,
        message_ids: vec!["3EB00106E8BE0F407E88EC".into()],
        receipt_type: kind.into(),
        timestamp: secs(ts),
    }
}

/// The doc masks numbers with `XXXXX`; swap in our concrete fixture JIDs.
fn unmask(mut v: Value) -> Value {
    v["payload"]["chat_id"] = json!("120363402106@g.us");
    v["payload"]["from"] = json!("6289685000000@s.whatsapp.net");
    if let Some(p) = v["payload"].as_object_mut() {
        p.remove("from_lid");
    }
    v
}

#[tokio::test]
async fn receipt_delivered() {
    let r = FakeResolver::new(&[]);
    let rc = receipt("delivery", "2025-07-18T22:44:20Z");
    let actual = Value::Object(receipt_envelope(&r, &rc, "628123456789@s.whatsapp.net").await);
    assert_matches_doc(&actual, &unmask(doc_example("Message Delivered")));
}

#[tokio::test]
async fn receipt_read() {
    let r = FakeResolver::new(&[]);
    let rc = receipt("read", "2025-07-18T22:44:44Z");
    let actual = Value::Object(receipt_envelope(&r, &rc, "628123456789@s.whatsapp.net").await);
    assert_matches_doc(&actual, &unmask(doc_example("Message Read")));
}

#[test]
fn receipt_forwarding_rules() {
    let mut rc = receipt("delivery", "2025-07-18T22:44:20Z");
    assert!(should_forward_receipt(&rc));
    rc.sender = jid("6289685000000:3@s.whatsapp.net");
    assert!(
        !should_forward_receipt(&rc),
        "linked-device delivery is dropped"
    );
    rc.receipt_type = "read".into();
    assert!(should_forward_receipt(&rc));
    rc.receipt_type = "read-self".into();
    assert!(!should_forward_receipt(&rc));
    rc.is_from_me = true;
    assert!(should_forward_receipt(&rc));
}

#[test]
fn device_logged_out() {
    let actual = Value::Object(device_envelope(
        DEVICE_LOGGED_OUT,
        "org_2",
        Some("628123456789@s.whatsapp.net"),
        logged_out_payload(401, true),
        secs("2026-10-28T09:00:00Z"),
    ));
    assert_matches_doc(&actual, &doc_example("Device Logged Out"));
}

#[tokio::test]
async fn lid_mentions_in_body_are_rewritten() {
    let r = FakeResolver::new(&[("251556368777322", "628111111111")]);
    assert_eq!(
        replace_lid_mentions(&r, "hi @251556368777322 a@251556368777322 @999").await,
        "hi @628111111111 a@251556368777322 @999"
    );
}
