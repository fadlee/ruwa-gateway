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
const ALWAYS_EMITTED: [&str; 2] = ["sender_display_name", "is_from_me"];

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
    /// `Some(path)` enables auto-download and returns this path.
    saved: Option<String>,
}

impl FakeResolver {
    fn new(lids: &[(&str, &str)]) -> Self {
        Self {
            lids: lids
                .iter()
                .map(|(l, p)| (l.to_string(), p.to_string()))
                .collect(),
            display: "Saved Contact".into(),
            saved: None,
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

    fn auto_download(&self) -> bool {
        self.saved.is_some()
    }

    async fn save_media<'a>(&'a self, _: MediaRef<'a>) -> anyhow::Result<String> {
        self.saved
            .clone()
            .ok_or_else(|| anyhow::anyhow!("disabled"))
    }
}

fn saving(path: &str) -> FakeResolver {
    FakeResolver {
        saved: Some(path.into()),
        ..FakeResolver::new(&[])
    }
}

fn field<T>(set: impl FnOnce(&mut proto::Message, T), value: T) -> proto::Message {
    let mut m = proto::Message::default();
    set(&mut m, value);
    m
}

use crate::wa::sdk::waproto::buffa::MessageField;

fn image(caption: Option<&str>) -> proto::Message {
    let mut i = proto::message::ImageMessage::default();
    i.caption = caption.map(Into::into);
    i.url = Some("https://mmg.whatsapp.net/...".into());
    field(|m, v| m.image_message = MessageField::some(v), i)
}

/// Doc examples for media omit `sender_display_name`/`is_from_me`; our
/// fixtures use the doc's sender and push name.
fn media_msg(id: &str, ts: &str, message: proto::Message) -> IncomingMessage {
    incoming(
        id,
        "628987654321@s.whatsapp.net",
        "628123456789@s.whatsapp.net",
        "John Doe",
        ts,
        message,
    )
}

/// The n-th ```json block after a heading.
fn doc_example_n(heading: &str, n: usize) -> Value {
    let marker = format!("### {heading}\n");
    let start = DOC
        .find(&marker)
        .unwrap_or_else(|| panic!("heading {heading:?} not in doc"));
    let mut rest = &DOC[start..];
    for _ in 0..n {
        let open = rest.find("```json\n").unwrap() + 8;
        rest = &rest[open..];
        let close = rest.find("```").unwrap();
        rest = &rest[close + 3..];
    }
    let open = rest.find("```json\n").unwrap() + 8;
    let close = rest[open..].find("```").unwrap() + open;
    serde_json::from_str(&rest[open..close]).unwrap()
}

#[tokio::test]
async fn image_auto_download_without_caption() {
    let r = saving("statics/media/1752404751-ad9e37ac-c658-4fe5-8d25-ba4a3f4d58fd.jpeg");
    let m = media_msg("3EB0C127D7BACC83D6A3", "2025-07-13T11:05:51Z", image(None));
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example_n("Image Message", 0));
}

#[tokio::test]
async fn image_auto_download_with_caption() {
    let r = saving("statics/media/1752404751-ad9e37ac-c658-4fe5-8d25-ba4a3f4d58fd.jpeg");
    let m = media_msg(
        "3EB0C127D7BACC83D6A3",
        "2025-07-13T11:05:51Z",
        image(Some("Check this out!")),
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example_n("Image Message", 1));
}

#[tokio::test]
async fn image_without_auto_download() {
    let r = FakeResolver::new(&[]);
    let m = media_msg(
        "3EB0C127D7BACC83D6A3",
        "2025-07-13T11:05:51Z",
        image(Some("Check this out!")),
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example_n("Image Message", 2));
}

#[tokio::test]
async fn audio_and_sticker_are_plain_paths() {
    let r = saving("statics/media/1752404905-b9393cd1-8546-4df9-8a60-ee3276036aba.ogg");
    let a = field(
        |m, v| m.audio_message = MessageField::some(v),
        proto::message::AudioMessage::default(),
    );
    let m = media_msg("3EB0C127D7BACC83D6A5", "2023-10-15T10:55:00Z", a);
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Audio Message"));

    let r = saving("statics/media/1752404986-ff2464a6-c54c-4e6c-afde-c4c925ce3573.webp");
    let st = field(
        |m, v| m.sticker_message = MessageField::some(v),
        proto::message::StickerMessage::default(),
    );
    let m = media_msg(
        "446AC2BAF2061B53E24CA526DBDFBD4E",
        "2025-07-13T11:09:45Z",
        st,
    );
    let actual = message_envelope(&r, &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Sticker Message"));
}

#[tokio::test]
async fn document_both_modes() {
    let mut d = proto::message::DocumentMessage::default();
    d.caption = Some("Monthly report".into());
    d.file_name = Some("report.pdf".into());
    d.url = Some("https://mmg.whatsapp.net/...".into());
    let msg = field(|m, v| m.document_message = MessageField::some(v), d);
    let m = media_msg("3EB0C127D7BACC83D6A6", "2023-10-15T11:00:00Z", msg);

    let actual = message_envelope(
        &saving("statics/media/1752404965-document.pdf"),
        &m,
        "628987654321@s.whatsapp.net",
    )
    .await;
    assert_matches_doc(&actual, &doc_example_n("Document Message", 0));
    let actual = message_envelope(&FakeResolver::new(&[]), &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example_n("Document Message", 1));
}

#[tokio::test]
async fn contact_message() {
    let mut c = proto::message::ContactMessage::default();
    c.display_name = Some("3Care".into());
    c.vcard = Some(
        "BEGIN:VCARD\nVERSION:3.0\nN:;3Care;;;\nFN:3Care\nTEL;type=Mobile:+62 132\nEND:VCARD"
            .into(),
    );
    let msg = field(|m, v| m.contact_message = MessageField::some(v), c);
    let m = media_msg(
        "56B3DFF4994284634E7AAFEEF6F1A0A2",
        "2025-07-13T11:10:19Z",
        msg,
    );
    let actual = message_envelope(&FakeResolver::new(&[]), &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Contact Message"));
}

#[tokio::test]
async fn location_message() {
    let mut l = proto::message::LocationMessage::default();
    l.degrees_latitude = Some(-6.2088);
    l.degrees_longitude = Some(106.8456);
    l.name = Some("Jakarta, Indonesia".into());
    l.address = Some("Central Jakarta, DKI Jakarta, Indonesia".into());
    let msg = field(|m, v| m.location_message = MessageField::some(v), l);
    let m = media_msg("3EB0C127D7BACC83D6A9", "2023-10-15T11:15:00Z", msg);
    let actual = message_envelope(&FakeResolver::new(&[]), &m, "628987654321@s.whatsapp.net").await;
    assert_matches_doc(&actual, &doc_example("Location Message"));
}

async fn presence(
    r: &FakeResolver,
    chat: &str,
    sender: &str,
    group: bool,
    state: &'static str,
    media: &'static str,
    ts: &str,
) -> Value {
    let view = ChatPresenceView {
        chat: jid(chat),
        sender: jid(sender),
        is_group: group,
        state,
        media,
    };
    Value::Object(chat_presence_envelope(r, &view, "628123456789@s.whatsapp.net", secs(ts)).await)
}

#[tokio::test]
async fn chat_presence_examples() {
    let r = FakeResolver::new(&[("251556368777322", "628987654321")]);
    let dm = "628987654321@s.whatsapp.net";
    assert_matches_doc(
        &presence(&r, dm, dm, false, "composing", "", "2026-01-22T12:00:00Z").await,
        &doc_example("User Typing"),
    );
    assert_matches_doc(
        &presence(&r, dm, dm, false, "paused", "", "2026-01-22T12:00:05Z").await,
        &doc_example("User Stopped Typing"),
    );
    assert_matches_doc(
        &presence(
            &r,
            dm,
            dm,
            false,
            "composing",
            "audio",
            "2026-01-22T12:01:00Z",
        )
        .await,
        &doc_example("User Recording Audio"),
    );
    let mut expected = doc_example("Group Typing");
    expected["payload"]["chat_id"] = json!("120363402106@g.us");
    assert_matches_doc(
        &presence(
            &r,
            "120363402106@g.us",
            "251556368777322@lid",
            true,
            "composing",
            "",
            "2026-01-22T12:02:00Z",
        )
        .await,
        &expected,
    );
}

#[test]
fn device_temporary_ban() {
    let now = secs("2026-10-28T09:00:00Z");
    let actual = Value::Object(device_envelope(
        DEVICE_TEMPORARY_BAN,
        "org_2",
        Some("628123456789@s.whatsapp.net"),
        temporary_ban_payload(101, 86400, now),
        now,
    ));
    assert_matches_doc(&actual, &doc_example("Device Temporary Ban"));
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
        sender_alt: None,
        admin_revoke: false,
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

fn group_doc(heading: &str) -> Value {
    let mut v = doc_example(heading);
    v["payload"]["chat_id"] = json!("120363402106@g.us");
    if let Some(jids) = v["payload"].get_mut("jids").and_then(Value::as_array_mut) {
        for (i, j) in jids.iter_mut().enumerate() {
            *j = json!(format!("628990000000{i}@s.whatsapp.net"));
        }
    }
    if v["payload"].get("name_set_by").is_some() {
        v["payload"]["name_set_by"] = json!("6289685000000@s.whatsapp.net");
    }
    v
}

#[tokio::test]
async fn group_participants_events() {
    let r = FakeResolver::new(&[("251556368777322", "6289900000001")]);
    let group = jid("120363402106@g.us");
    for (heading, kind, n, ts) in [
        ("Group Member Join", "join", 2, "2025-07-28T10:30:00Z"),
        ("Group Member Leave", "leave", 1, "2025-07-28T10:32:00Z"),
        (
            "Group Member Promotion",
            "promote",
            1,
            "2025-07-28T10:33:00Z",
        ),
        ("Group Member Demotion", "demote", 1, "2025-07-28T10:34:00Z"),
    ] {
        // The second member of a join arrives as a LID and is resolved.
        let jids: Vec<Jid> = (0..n)
            .map(|i| {
                if i == 1 {
                    jid("251556368777322@lid")
                } else {
                    jid(&format!("628990000000{i}@s.whatsapp.net"))
                }
            })
            .collect();
        let actual = Value::Object(
            group_participants_envelope(
                &r,
                &group,
                kind,
                &jids,
                "628123456789@s.whatsapp.net",
                secs(ts),
            )
            .await,
        );
        assert_matches_doc(&actual, &group_doc(heading));
    }
}

#[tokio::test]
async fn group_name_change() {
    let r = FakeResolver::new(&[("251556368777322", "6289685000000")]);
    let group = jid("120363402106@g.us");
    let owner = jid("251556368777322@lid");
    let actual = Value::Object(
        group_info_envelope(
            &r,
            &group,
            "Project Team",
            Some((&owner, None)),
            "628123456789@s.whatsapp.net",
            secs("2025-07-28T10:35:00Z"),
        )
        .await,
    );
    assert_matches_doc(&actual, &group_doc("Group Name Change"));

    // The phone-number alias wins; no renamer → field omitted.
    let pn = jid("6289111111111@s.whatsapp.net");
    let with_pn = group_info_envelope(&r, &group, "X", Some((&owner, Some(&pn))), "", 0).await;
    assert_eq!(
        with_pn["payload"]["name_set_by"],
        "6289111111111@s.whatsapp.net"
    );
    assert!(with_pn.get("device_id").is_none());
    let none = group_info_envelope(&r, &group, "X", None, "d", 0).await;
    assert!(none["payload"].get("name_set_by").is_none());
}

#[test]
fn group_joined_shape() {
    let env = group_joined_envelope(
        &jid("120363402106@g.us"),
        &jid("628123456789:5@s.whatsapp.net"),
        "invite",
        "Team",
        "628123456789@s.whatsapp.net",
        0,
    );
    assert_eq!(env["event"], "group.joined");
    assert_eq!(
        env["payload"],
        json!({"chat_id":"120363402106@g.us","type":"join","jids":["628123456789@s.whatsapp.net"],"reason":"invite","group_name":"Team"})
    );
}

#[tokio::test]
async fn call_offer_events() {
    let r = FakeResolver::new(&[]);
    let mut view = CallOfferView {
        call_id: "ABC123DEF456".into(),
        from: jid("628987654321@s.whatsapp.net"),
        auto_rejected: false,
        platform: "android".into(),
        version: "2.24.1.5".into(),
        group: None,
    };
    let device = "628123456789@s.whatsapp.net";
    let ts = secs("2026-02-05T12:00:00Z");
    let actual = Value::Object(call_offer_envelope(&r, &view, device, ts).await);
    assert_matches_doc(&actual, &doc_example("Call Offer"));

    view.auto_rejected = true;
    view.group = Some(jid("120363402106@g.us"));
    let actual = Value::Object(call_offer_envelope(&r, &view, device, ts).await);
    let mut expected = doc_example("Call Offer with Auto-Reject Enabled");
    expected["payload"]["group_jid"] = json!("120363402106@g.us");
    assert_matches_doc(&actual, &expected);

    view.platform.clear();
    view.version.clear();
    let bare = call_offer_envelope(&r, &view, device, ts).await;
    assert!(bare["payload"].get("remote_platform").is_none());
    assert!(bare["payload"].get("remote_version").is_none());
}

#[tokio::test]
async fn presence_events() {
    let r = FakeResolver::new(&[("251556368777322", "628987654321")]);
    let device = "628123456789@s.whatsapp.net";
    let section = DOC
        .split("## Presence Events")
        .nth(1)
        .and_then(|s| s.split("### Presence Event Fields").next())
        .unwrap();
    let blocks: Vec<Value> = section
        .split("```json")
        .skip(1)
        .map(|b| b.split("```").next().unwrap().trim())
        .filter(|b| b.contains("\"event\": \"presence\""))
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    assert_eq!(blocks.len(), 2);

    let offline = presence_envelope(
        &r,
        &jid("628987654321@s.whatsapp.net"),
        false,
        Some(secs("2026-01-22T12:04:35Z")),
        device,
        secs("2026-01-22T12:05:00Z"),
    )
    .await;
    assert_matches_doc(&Value::Object(offline), &blocks[0]);

    // LID sender is resolved and reported; no last_seen when hidden.
    let online = presence_envelope(
        &r,
        &jid("251556368777322@lid"),
        true,
        None,
        device,
        secs("2026-01-22T12:10:00Z"),
    )
    .await;
    assert_eq!(online["payload"]["from_lid"], "251556368777322@lid");
    let mut online = Value::Object(online);
    online["payload"]
        .as_object_mut()
        .unwrap()
        .remove("from_lid");
    assert_matches_doc(&online, &blocks[1]);
}

#[tokio::test]
async fn message_deleted_event() {
    let r = FakeResolver::new(&[]);
    let view = DeletedView {
        id: "3EB0C127D7BACC83D6A1".into(),
        from: jid("628987654321@s.whatsapp.net"),
        event_chat: jid("628987654321@s.whatsapp.net"),
        chat_id: "628987654321@s.whatsapp.net".into(),
        content: "Hello, how are you?".into(),
        sender: "628987654321@s.whatsapp.net".into(),
        timestamp: secs("2025-07-13T10:30:00Z"),
        was_from_me: false,
        media_type: String::new(),
        filename: String::new(),
    };
    let actual = Value::Object(
        message_deleted_envelope(
            &r,
            &view,
            "628123456789@s.whatsapp.net",
            secs("2025-07-13T11:12:00Z"),
        )
        .await,
    );
    assert_matches_doc(&actual, &doc_example("Message Deleted"));

    let mut media = view.clone();
    media.media_type = "image".into();
    media.filename = "a.jpg".into();
    media.event_chat = jid("251556368777322@lid");
    let env = message_deleted_envelope(&r, &media, "d", 0).await;
    assert_eq!(env["payload"]["original_media_type"], "image");
    assert_eq!(env["payload"]["original_filename"], "a.jpg");
    assert_eq!(env["payload"]["chat_lid"], "251556368777322@lid");
}

#[tokio::test]
async fn label_events() {
    let device = "628123456789@s.whatsapp.net";
    let edit = label_edit_envelope(
        &LabelEditView {
            label_id: "9".into(),
            name: Some("Important".into()),
            color: Some(2),
            predefined_id: Some(1),
            deleted: Some(false),
            order_index: Some(1),
            is_active: Some(true),
            kind: Some("CUSTOM".into()),
            is_immutable: Some(false),
            mute_end_time_ms: Some(0),
        },
        device,
        secs("2026-01-22T12:03:00Z"),
    );
    let mut expected = doc_example("Label Edit");
    // GOWA emits `predefinedId` (int32) as a JSON number; the doc shows a string.
    expected["payload"]["predefined_id"] = json!(1);
    assert_matches_doc(&Value::Object(edit), &expected);

    // Absent fields are omitted.
    let sparse = label_edit_envelope(
        &LabelEditView {
            label_id: "9".into(),
            deleted: Some(true),
            ..Default::default()
        },
        device,
        0,
    );
    assert_eq!(sparse["payload"], json!({"label_id": "9", "deleted": true}));

    let r = FakeResolver::new(&[("251556368777322", "628987654321")]);
    let assoc = label_association_envelope(
        &r,
        "9",
        true,
        &jid("120363402106@g.us"),
        device,
        secs("2026-01-22T12:04:00Z"),
    )
    .await;
    let mut expected = doc_example("Label Association");
    expected["payload"]["chat_id"] = json!("120363402106@g.us");
    assert_matches_doc(&Value::Object(assoc), &expected);

    let lid =
        label_association_envelope(&r, "9", false, &jid("251556368777322@lid"), device, 0).await;
    assert_eq!(lid["payload"]["chat_lid"], "251556368777322@lid");
    assert_eq!(lid["payload"]["chat_id"], "628987654321@s.whatsapp.net");
    assert_eq!(lid["payload"]["labeled"], false);
}

#[test]
fn device_reachout_timelock() {
    let actual = Value::Object(device_envelope(
        DEVICE_REACHOUT_TIMELOCK,
        "org_2",
        Some("628123456789@s.whatsapp.net"),
        reachout_timelock_payload(true, "", Some(secs("2026-10-28T21:00:00Z"))),
        secs("2026-10-28T09:00:00Z"),
    ));
    assert_matches_doc(&actual, &doc_example("Device Reach-out Timelock"));
    assert_eq!(
        reachout_timelock_payload(false, "BIZ", None),
        json!({"is_active": false, "enforcement_type": "BIZ"})
    );
}
