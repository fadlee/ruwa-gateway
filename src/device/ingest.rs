//! Events → chat storage (GOWA `CreateMessage`, `CreateReaction`,
//! `StoreSentMessageWithContext`, `history_sync.go`, `event_archive.go`).
//!
//! `key` is the storage partition (account JID, else slot id). JIDs are
//! normalised LID → phone number before they are stored, like GOWA.

use crate::{
    events::{IncomingMessage, Resolver},
    storage::chat::{Chat, ChatStore, ContactName, EditOutcome, Message, MessageEdit, Reaction},
    wa::{Jid, Server, jid::parse_jid, message as msgx, proto},
};

const STATUS_BROADCAST: &str = "status@broadcast";

async fn normalize<R: Resolver>(r: &R, jid: &Jid) -> Jid {
    if jid.is_lid()
        && let Some(pn) = r.pn_for_lid(jid).await
    {
        return pn;
    }
    jid.clone()
}

/// GOWA `GetChatNameWithPushNameByDevice`.
pub fn chat_name(existing: Option<&Chat>, jid: &Jid, sender_user: &str, push_name: &str) -> String {
    if jid.to_string() == STATUS_BROADCAST {
        return "Status".to_owned();
    }
    let own_user = jid.to_non_ad().user.to_string();
    if let Some(chat) = existing.filter(|c| !c.name.is_empty()) {
        if !push_name.is_empty() && (chat.name == own_user || chat.name == sender_user) {
            return push_name.to_owned();
        }
        return chat.name.clone();
    }
    match jid.server {
        Server::Group => format!("Group {}", jid.user),
        Server::Newsletter => format!("Newsletter {}", jid.user),
        _ => {
            if !push_name.is_empty() && push_name != sender_user && push_name != own_user {
                push_name.to_owned()
            } else if !sender_user.is_empty() {
                sender_user.to_owned()
            } else {
                own_user
            }
        }
    }
}

/// GOWA `MessageAuthorMatches`.
fn author_matches(
    stored_sender: &str,
    stored_from_me: bool,
    sender: &str,
    alias: &str,
    from_me: bool,
) -> bool {
    if stored_from_me && from_me {
        return true;
    }
    let stored = stored_sender.trim();
    !stored.is_empty() && [sender, alias].iter().any(|c| c.trim() == stored)
}

fn media_columns(m: &mut Message, msg: &proto::Message) {
    if let Some(info) = msgx::media_info(msg) {
        m.media_type = info.media_type.to_owned();
        m.filename = info.filename;
        m.url = info.url;
        m.direct_path = info.direct_path;
        m.media_key = info.media_key;
        m.file_sha256 = info.file_sha256;
        m.file_enc_sha256 = info.file_enc_sha256;
        m.file_length = info.file_length as i64;
    }
}

/// Store a live message (GOWA `handleMessage` → `CreateReaction` /
/// `CreateMessage`), including revokes and edits.
pub async fn store_incoming<R: Resolver>(
    store: &ChatStore,
    r: &R,
    key: &str,
    own: Option<&Jid>,
    m: &IncomingMessage,
) -> anyhow::Result<()> {
    let chat_jid = normalize(r, &m.chat.to_non_ad()).await;
    let sender_jid = normalize(r, &m.sender).await.to_non_ad();
    let chat = chat_jid.to_string();
    let sender = sender_jid.to_string();
    let msg = &*m.message;

    // whatsmeow keeps the sender's push name and verified business name in
    // its contact store; GOWA reads them back for display names.
    if !m.is_from_me && sender_jid.server != Server::Group {
        if !m.push_name.is_empty() && m.push_name != "-" {
            store
                .set_contact_name(key, &sender, ContactName::Push(m.push_name.clone()))
                .await?;
        }
        if let Some(name) = m.verified_name.as_deref().filter(|n| !n.is_empty()) {
            store
                .set_contact_name(key, &sender, ContactName::Business(name.to_owned()))
                .await?;
        }
    }

    if let Some((reacted, emoji)) = msgx::reaction(msg) {
        let Some(id) = reacted.id.as_deref().filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        let reactor = if m.sender.user.is_empty() && m.is_from_me {
            own.map(|j| j.to_non_ad().to_string()).unwrap_or_default()
        } else {
            sender.clone()
        };
        anyhow::ensure!(!reactor.is_empty(), "reaction sender jid is required");
        return store
            .store_reaction(Reaction {
                message_id: id.to_owned(),
                chat_jid: chat,
                device_id: key.to_owned(),
                reactor_jid: reactor,
                emoji: emoji.to_owned(),
                is_from_me: m.is_from_me,
                reaction_timestamp: m.timestamp,
                ..Default::default()
            })
            .await;
    }

    if let Some(revoked) = msgx::revoked_id(msg) {
        return revoke(store, r, key, &chat_jid, &sender, m, revoked).await;
    }

    // The push name is the sender's: on our own messages it would label the
    // peer chat with our name.
    let (name_user, push) = if m.is_from_me {
        (chat_jid.user.to_string(), "")
    } else {
        (sender_jid.user.to_string(), m.push_name.as_str())
    };
    let existing = store.get_chat(key, &chat).await?;
    let name = chat_name(existing.as_ref(), &chat_jid, &name_user, push);

    if let Some((original, edited)) = msgx::edit(msg) {
        if existing.is_none() {
            store
                .store_chat(Chat {
                    jid: chat.clone(),
                    device_id: key.to_owned(),
                    name,
                    last_message_time: m.timestamp,
                    ..Default::default()
                })
                .await?;
        }
        return apply_edit(store, key, &chat, &sender, m, original, edited).await;
    }

    let exp = msgx::ephemeral_expiration(msg);
    store
        .store_chat(Chat {
            jid: chat.clone(),
            device_id: key.to_owned(),
            name,
            last_message_time: m.timestamp,
            ephemeral_expiration: if exp > 0 {
                exp as i32
            } else {
                existing.as_ref().map_or(0, |c| c.ephemeral_expiration)
            },
            archived: existing.as_ref().is_some_and(|c| c.archived),
            ..Default::default()
        })
        .await?;

    let mut row = Message {
        id: m.id.clone(),
        chat_jid: chat,
        device_id: key.to_owned(),
        sender,
        content: msgx::storage_text(msg),
        timestamp: m.timestamp,
        is_from_me: m.is_from_me,
        context_metadata: msgx::context_metadata(msg),
        ..Default::default()
    };
    media_columns(&mut row, msg);
    store.store_message(row).await
}

/// GOWA `revokeMessageByAuthor`: only the author, a group admin, or this
/// account may remove a stored message.
async fn revoke<R: Resolver>(
    store: &ChatStore,
    r: &R,
    key: &str,
    chat: &Jid,
    sender: &str,
    m: &IncomingMessage,
    revoked: &str,
) -> anyhow::Result<()> {
    let chat_str = chat.to_string();
    let existing = store.get_message_in_chat(key, &chat_str, revoked).await?;
    let raw_sender = m.sender.to_non_ad().to_string();
    let mut authorized = m.is_from_me;
    if !authorized && let Some(stored) = &existing {
        authorized = author_matches(
            &stored.sender,
            stored.is_from_me,
            sender,
            &raw_sender,
            false,
        ) || admin_revoke(r, m, &stored.sender).await;
    }
    if !authorized {
        if existing.is_some() {
            tracing::warn!(
                id = revoked,
                sender,
                "ignoring revoke: sender is not the author"
            );
        }
        return Ok(());
    }
    store.delete_message(key, &chat_str, revoked).await
}

/// GOWA `groupAdminRevoke`: an admin deleting another member's message
/// (`edit="8"`, author in the key's participant).
async fn admin_revoke<R: Resolver>(r: &R, m: &IncomingMessage, stored_sender: &str) -> bool {
    if !m.admin_revoke || m.chat.server != Server::Group {
        return false;
    }
    let Some(author) = msgx::revoked_participant(&m.message).and_then(|p| parse_jid(p).ok()) else {
        return false;
    };
    let normalized = normalize(r, &author).await.to_non_ad().to_string();
    author_matches(
        stored_sender,
        false,
        &normalized,
        &author.to_non_ad().to_string(),
        false,
    ) && r
        .is_group_admin(&m.chat, &m.sender, m.sender_alt.as_ref())
        .await
}

/// GOWA `storeEditedMessage`.
async fn apply_edit(
    store: &ChatStore,
    key: &str,
    chat: &str,
    sender: &str,
    m: &IncomingMessage,
    original: &str,
    edited: &proto::Message,
) -> anyhow::Result<()> {
    let new_content = msgx::storage_text(edited);
    // Without the original a peer's authorship cannot be checked, so only
    // our own edits may create the row.
    let stub = m.is_from_me.then(|| {
        let mut row = Message {
            id: original.to_owned(),
            chat_jid: chat.to_owned(),
            device_id: key.to_owned(),
            sender: sender.to_owned(),
            content: new_content.clone(),
            timestamp: m.timestamp,
            is_from_me: true,
            ..Default::default()
        };
        media_columns(&mut row, edited);
        row
    });
    let (who, alias, from_me) = (
        sender.to_owned(),
        m.sender.to_non_ad().to_string(),
        m.is_from_me,
    );
    let outcome = store
        .apply_edit(
            MessageEdit {
                original_message_id: original.to_owned(),
                edit_event_id: m.id.clone(),
                chat_jid: chat.to_owned(),
                device_id: key.to_owned(),
                editor: sender.to_owned(),
                new_content,
                edited_at: m.timestamp,
                ..Default::default()
            },
            msgx::context_metadata(edited),
            stub,
            move |stored| author_matches(&stored.sender, stored.is_from_me, &who, &alias, from_me),
        )
        .await?;
    if outcome == EditOutcome::Rejected {
        tracing::warn!(
            id = original,
            sender,
            "ignoring edit: sender is not the author"
        );
    }
    Ok(())
}

/// A message this account sent through the API (GOWA
/// `StoreSentMessageWithContext`). `recipient` is already resolved to a
/// phone-number JID where possible.
#[allow(clippy::too_many_arguments)]
pub async fn store_sent(
    store: &ChatStore,
    key: &str,
    own: &str,
    recipient: &Jid,
    id: &str,
    content: &str,
    timestamp: i64,
    msg: &proto::Message,
) -> anyhow::Result<()> {
    let chat = recipient.to_non_ad();
    let chat_str = chat.to_string();
    let mut row = Message {
        id: id.to_owned(),
        chat_jid: chat_str.clone(),
        device_id: key.to_owned(),
        sender: own.to_owned(),
        content: content.to_owned(),
        timestamp,
        is_from_me: true,
        context_metadata: msgx::context_metadata(msg),
        ..Default::default()
    };
    media_columns(&mut row, msg);
    // Message before chat: losing only the chat bump is invisible.
    store.store_sent_message(row).await?;
    let existing = store.get_chat(key, &chat_str).await?;
    let name = chat_name(existing.as_ref(), &chat, &chat.user, "");
    store
        .store_chat(Chat {
            jid: chat_str,
            device_id: key.to_owned(),
            name,
            last_message_time: timestamp,
            ephemeral_expiration: existing.as_ref().map_or(0, |c| c.ephemeral_expiration),
            archived: existing.as_ref().is_some_and(|c| c.archived),
            ..Default::default()
        })
        .await
}

/// Mirror an edit this account sent through the API (GOWA
/// `updateStoredMessage`): the edit is never echoed back, so nothing else
/// would apply it to the stored copy.
#[allow(clippy::too_many_arguments)]
pub async fn store_own_edit(
    store: &ChatStore,
    key: &str,
    own: &str,
    chat: &Jid,
    original: &str,
    edit_id: &str,
    text: &str,
    timestamp: i64,
) -> anyhow::Result<()> {
    let chat = chat.to_non_ad().to_string();
    let stub = Message {
        id: original.to_owned(),
        chat_jid: chat.clone(),
        device_id: key.to_owned(),
        sender: own.to_owned(),
        content: text.to_owned(),
        timestamp,
        is_from_me: true,
        ..Default::default()
    };
    store
        .apply_edit(
            MessageEdit {
                original_message_id: original.to_owned(),
                edit_event_id: edit_id.to_owned(),
                chat_jid: chat,
                device_id: key.to_owned(),
                editor: own.to_owned(),
                new_content: text.to_owned(),
                edited_at: timestamp,
                ..Default::default()
            },
            String::new(),
            Some(stub),
            {
                let own = own.to_owned();
                move |stored| author_matches(&stored.sender, stored.is_from_me, &own, "", true)
            },
        )
        .await?;
    Ok(())
}

/// GOWA `handleArchive`: only chats already stored are updated.
pub async fn store_archive<R: Resolver>(
    store: &ChatStore,
    r: &R,
    key: &str,
    jid: &Jid,
    archived: bool,
) -> anyhow::Result<()> {
    let jid = normalize(r, &jid.to_non_ad()).await.to_string();
    if let Some(mut chat) = store.get_chat(key, &jid).await? {
        chat.archived = archived;
        store.store_chat(chat).await?;
    }
    Ok(())
}

/// App-state contact action (address book entry): whatsmeow stores its
/// first/full name under the phone-number JID.
pub async fn store_contact<R: Resolver>(
    store: &ChatStore,
    r: &R,
    key: &str,
    jid: &Jid,
    action: &proto::sync_action_value::ContactAction,
) -> anyhow::Result<()> {
    let jid = match action.pn_jid.as_deref().and_then(|p| parse_jid(p).ok()) {
        Some(pn) if jid.is_lid() => pn,
        _ => normalize(r, jid).await,
    };
    store
        .set_contact_name(
            key,
            &jid.to_non_ad().to_string(),
            ContactName::Saved {
                first: action.first_name.clone().unwrap_or_default(),
                full: action.full_name.clone().unwrap_or_default(),
            },
        )
        .await
}

/// GOWA `processHistorySync`: conversations (bootstrap/recent/on-demand)
/// and push names.
pub async fn store_history<R: Resolver>(
    store: &ChatStore,
    r: &R,
    key: &str,
    own: Option<&Jid>,
    data: &proto::HistorySync,
) -> anyhow::Result<()> {
    use proto::history_sync::HistorySyncType as T;
    match data.sync_type {
        T::INITIAL_BOOTSTRAP | T::RECENT | T::ON_DEMAND => {
            let on_demand = data.sync_type == T::ON_DEMAND;
            for conv in &data.conversations {
                if let Err(error) = store_conversation(store, r, key, own, conv, on_demand).await {
                    tracing::warn!(chat = %conv.id, %error, "failed to store synced conversation");
                }
            }
            Ok(())
        }
        T::PUSH_NAME => {
            for p in &data.pushnames {
                let (Some(id), Some(name)) = (p.id.as_deref(), p.pushname.as_deref()) else {
                    continue;
                };
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let Ok(jid) = parse_jid(id) else { continue };
                let jid = normalize(r, &jid).await.to_string();
                store
                    .set_contact_name(key, &jid, ContactName::Push(name.to_owned()))
                    .await?;
                match store.get_chat(key, &jid).await? {
                    // Zero time keeps it from bubbling up in the chat list.
                    None => {
                        store
                            .store_chat(Chat {
                                jid,
                                device_id: key.to_owned(),
                                name: name.to_owned(),
                                ..Default::default()
                            })
                            .await?
                    }
                    Some(mut chat) if chat.name != name => {
                        chat.name = name.to_owned();
                        store.store_chat(chat).await?;
                    }
                    Some(_) => {}
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

async fn store_conversation<R: Resolver>(
    store: &ChatStore,
    r: &R,
    key: &str,
    own: Option<&Jid>,
    conv: &proto::Conversation,
    on_demand: bool,
) -> anyhow::Result<()> {
    if conv.id.is_empty() {
        return Ok(());
    }
    let jid = normalize(r, &parse_jid(&conv.id).map_err(anyhow::Error::msg)?).await;
    let chat = jid.to_string();
    let is_group = jid.server == Server::Group;
    // GOWA prefers the contact store for 1:1 names; until contacts are
    // stored the conversation's display name is used. Groups and
    // newsletters get "Group <id>" / "Newsletter <id>" like GOWA.
    let display = conv.display_name.as_deref().unwrap_or_default();
    let existing = store.get_chat(key, &chat).await?;
    let name = chat_name(existing.as_ref(), &jid, "", display);

    let mut batch = Vec::new();
    let mut latest = 0i64;
    for hist in &conv.messages {
        let Some(info) = hist.message.as_option() else {
            continue;
        };
        let Some(msg_key) = info.key.as_option() else {
            continue;
        };
        let Some(id) = msg_key.id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(message) = info.message.as_option() else {
            continue;
        };
        let from_me = msg_key.from_me.unwrap_or(false);
        let sender = if from_me {
            match own {
                Some(own) => own.to_non_ad(),
                None => continue,
            }
        } else {
            let participant = msg_key
                .participant
                .as_deref()
                .filter(|p| !p.is_empty())
                .or(info.participant.as_deref().filter(|p| !p.is_empty()));
            match participant {
                Some(p) => match parse_jid(p) {
                    Ok(j) => normalize(r, &j).await.to_non_ad(),
                    Err(_) => continue,
                },
                // A group message without participant would be stored as
                // sent by the group (GOWA #609).
                None if is_group => continue,
                None => jid.clone(),
            }
        };
        let timestamp = info.message_timestamp.unwrap_or_default() as i64;

        if let Some((reacted, emoji)) = msgx::reaction(message) {
            if let Some(target) = reacted.id.as_deref().filter(|t| !t.is_empty()) {
                store
                    .store_reaction(Reaction {
                        message_id: target.to_owned(),
                        chat_jid: chat.clone(),
                        device_id: key.to_owned(),
                        reactor_jid: sender.to_string(),
                        emoji: emoji.to_owned(),
                        is_from_me: from_me,
                        reaction_timestamp: timestamp,
                        ..Default::default()
                    })
                    .await?;
            }
            latest = latest.max(timestamp);
            continue;
        }

        let mut row = Message {
            id: id.to_owned(),
            chat_jid: chat.clone(),
            device_id: key.to_owned(),
            sender: sender.to_string(),
            content: msgx::storage_text(message),
            timestamp,
            is_from_me: from_me,
            context_metadata: msgx::context_metadata(message),
            ..Default::default()
        };
        media_columns(&mut row, message);
        if row.content.is_empty() && row.media_type.is_empty() {
            continue;
        }
        latest = latest.max(timestamp);
        batch.push(row);
    }
    if batch.is_empty() {
        return Ok(());
    }

    let mut row = Chat {
        jid: chat,
        device_id: key.to_owned(),
        name,
        last_message_time: latest,
        ephemeral_expiration: conv.ephemeral_expiration.unwrap_or_default() as i32,
        ..Default::default()
    };
    // On-demand chunks are older than what is stored: never move activity
    // back or clobber flags; zero expiration means "not reported".
    if on_demand && let Some(existing) = &existing {
        row.last_message_time = row.last_message_time.max(existing.last_message_time);
        row.archived = existing.archived;
        if row.ephemeral_expiration == 0 {
            row.ephemeral_expiration = existing.ephemeral_expiration;
        }
    }
    store.store_chat(row).await?;
    store.store_messages(batch).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        events::MediaRef,
        storage::{Db, chat::MessageFilter},
        wa::sdk::waproto::buffa::MessageField,
    };

    const KEY: &str = "628111@s.whatsapp.net";

    struct R;
    impl Resolver for R {
        async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
            (lid.user == "999").then(|| Jid::pn("628999"))
        }
        async fn display_name(&self, _: &Jid, _: bool, _: &str) -> String {
            String::new()
        }
        fn auto_download(&self) -> bool {
            false
        }
        async fn save_media<'a>(&'a self, _: MediaRef<'a>) -> anyhow::Result<String> {
            unreachable!()
        }
        async fn is_group_admin(&self, _: &Jid, sender: &Jid, _: Option<&Jid>) -> bool {
            sender.user == "628admin"
        }
    }

    fn store() -> ChatStore {
        ChatStore::new(Db::open(":memory:").unwrap())
    }

    fn incoming(id: &str, chat: &str, sender: &str, msg: proto::Message) -> IncomingMessage {
        IncomingMessage {
            id: id.into(),
            chat: parse_jid(chat).unwrap(),
            sender: parse_jid(sender).unwrap(),
            is_from_me: false,
            push_name: "Bobby".into(),
            timestamp: 100,
            verified_name: None,
            verified_level: None,
            is_business: false,
            sender_alt: None,
            admin_revoke: false,
            message: Arc::new(msg),
        }
    }

    fn protocol(kind: proto::message::protocol_message::Type, id: &str) -> proto::Message {
        let mut k = proto::MessageKey::default();
        k.id = Some(id.into());
        let mut pm = proto::message::ProtocolMessage::default();
        pm.r#type = Some(kind);
        pm.key = MessageField::some(k);
        let mut msg = proto::Message::default();
        msg.protocol_message = MessageField::some(pm);
        msg
    }

    async fn messages(s: &ChatStore, chat: &str) -> Vec<Message> {
        s.list_messages(MessageFilter {
            device_id: KEY.into(),
            chat_jid: chat.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.message)
        .collect()
    }

    #[test]
    fn chat_names_like_gowa() {
        let pn = parse_jid("628222@s.whatsapp.net").unwrap();
        assert_eq!(chat_name(None, &pn, "628222", "Bob"), "Bob");
        assert_eq!(chat_name(None, &pn, "628222", ""), "628222");
        assert_eq!(chat_name(None, &pn, "", ""), "628222");
        let group = parse_jid("1203@g.us").unwrap();
        assert_eq!(chat_name(None, &group, "628", "Bob"), "Group 1203");
        let status = parse_jid("status@broadcast").unwrap();
        assert_eq!(chat_name(None, &status, "", ""), "Status");
        let stored = Chat {
            name: "628222".into(),
            ..Default::default()
        };
        assert_eq!(chat_name(Some(&stored), &pn, "628222", "Bob"), "Bob");
        let named = Chat {
            name: "Robert".into(),
            ..Default::default()
        };
        assert_eq!(chat_name(Some(&named), &pn, "628222", "Bob"), "Robert");
    }

    #[tokio::test]
    async fn incoming_text_lid_reply_and_ephemeral() {
        let s = store();
        let mut ci = proto::ContextInfo::default();
        ci.stanza_id = Some("Q".into());
        ci.expiration = Some(604_800);
        let m = incoming("1", "999@lid", "999@lid", msgx::extended_text("hello", ci));
        store_incoming(&s, &R, KEY, None, &m).await.unwrap();

        let chat = s
            .get_chat(KEY, "628999@s.whatsapp.net")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chat.name, "Bobby");
        assert_eq!(chat.ephemeral_expiration, 604_800);
        let rows = messages(&s, "628999@s.whatsapp.net").await;
        assert_eq!(rows[0].sender, "628999@s.whatsapp.net");
        assert_eq!(rows[0].content, "hello");
        assert_eq!(rows[0].context_metadata, r#"{"replied_to_id":"Q"}"#);

        // A later message without a timer keeps the stored one.
        let m = incoming("2", "999@lid", "999@lid", msgx::conversation("again"));
        store_incoming(&s, &R, KEY, None, &m).await.unwrap();
        let chat = s
            .get_chat(KEY, "628999@s.whatsapp.net")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chat.ephemeral_expiration, 604_800);
    }

    #[tokio::test]
    async fn reactions_edits_and_revokes() {
        use proto::message::protocol_message::Type;
        let s = store();
        let chat = "628222@s.whatsapp.net";
        store_incoming(
            &s,
            &R,
            KEY,
            None,
            &incoming("1", chat, chat, msgx::conversation("v1")),
        )
        .await
        .unwrap();

        let mut k = proto::MessageKey::default();
        k.id = Some("1".into());
        let mut rm = proto::message::ReactionMessage::default();
        rm.key = MessageField::some(k);
        rm.text = Some("🔥".into());
        let mut react = proto::Message::default();
        react.reaction_message = MessageField::some(rm);
        store_incoming(&s, &R, KEY, None, &incoming("r", chat, chat, react))
            .await
            .unwrap();
        let listed = s
            .list_messages(MessageFilter {
                device_id: KEY.into(),
                chat_jid: chat.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(listed[0].reactions[0].emoji, "🔥");

        // Edit by a stranger is ignored; by the author applied.
        let mut edit = protocol(Type::MessageEdit, "1");
        edit.protocol_message
            .as_option_mut()
            .unwrap()
            .edited_message = MessageField::some(msgx::conversation("v2"));
        let stranger = incoming("e0", chat, "628333@s.whatsapp.net", edit.clone());
        store_incoming(&s, &R, KEY, None, &stranger).await.unwrap();
        assert_eq!(messages(&s, chat).await[0].content, "v1");
        store_incoming(&s, &R, KEY, None, &incoming("e1", chat, chat, edit))
            .await
            .unwrap();
        assert_eq!(messages(&s, chat).await[0].content, "v2");

        // Revoke by a stranger is ignored; by the author applied.
        let revoke = protocol(Type::Revoke, "1");
        let stranger = incoming("x0", chat, "628333@s.whatsapp.net", revoke.clone());
        store_incoming(&s, &R, KEY, None, &stranger).await.unwrap();
        assert_eq!(messages(&s, chat).await.len(), 1);
        store_incoming(&s, &R, KEY, None, &incoming("x1", chat, chat, revoke))
            .await
            .unwrap();
        assert!(messages(&s, chat).await.is_empty());
    }

    #[tokio::test]
    async fn group_admin_revoke() {
        use proto::message::protocol_message::Type;
        let s = store();
        let group = "1203@g.us";
        let author = "628222@s.whatsapp.net";
        store_incoming(
            &s,
            &R,
            KEY,
            None,
            &incoming("1", group, author, msgx::conversation("hi")),
        )
        .await
        .unwrap();
        let mut revoke = protocol(Type::Revoke, "1");
        revoke
            .protocol_message
            .as_option_mut()
            .unwrap()
            .key
            .as_option_mut()
            .unwrap()
            .participant = Some(author.into());
        let mut by_member = incoming("x", group, "628333@s.whatsapp.net", revoke.clone());
        by_member.admin_revoke = true;
        store_incoming(&s, &R, KEY, None, &by_member).await.unwrap();
        assert_eq!(messages(&s, group).await.len(), 1);
        let mut by_admin = incoming("y", group, "628admin@s.whatsapp.net", revoke);
        by_admin.admin_revoke = true;
        store_incoming(&s, &R, KEY, None, &by_admin).await.unwrap();
        assert!(messages(&s, group).await.is_empty());
    }

    #[tokio::test]
    async fn sent_message_and_archive() {
        let s = store();
        let to = parse_jid("628222@s.whatsapp.net").unwrap();
        store_sent(
            &s,
            KEY,
            KEY,
            &to,
            "S1",
            "hey",
            50,
            &msgx::conversation("hey"),
        )
        .await
        .unwrap();
        let chat = s.get_chat(KEY, &to.to_string()).await.unwrap().unwrap();
        assert_eq!(chat.name, "628222");
        assert_eq!(chat.last_message_time, 50);
        let row = s.get_message(KEY, "S1").await.unwrap().unwrap();
        assert!(row.is_from_me);
        assert_eq!(row.sender, KEY);

        store_archive(&s, &R, KEY, &to, true).await.unwrap();
        assert!(
            s.get_chat(KEY, &to.to_string())
                .await
                .unwrap()
                .unwrap()
                .archived
        );
        // Unknown chats are not created.
        let other = parse_jid("628444@s.whatsapp.net").unwrap();
        store_archive(&s, &R, KEY, &other, true).await.unwrap();
        assert!(s.get_chat(KEY, &other.to_string()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn history_sync_conversations_and_push_names() {
        use proto::history_sync::HistorySyncType as T;
        let s = store();
        let own = parse_jid("628111@s.whatsapp.net").unwrap();
        let wmi = |id: &str, from_me: bool, participant: Option<&str>, text: &str, ts: u64| {
            let mut key = proto::MessageKey::default();
            key.id = Some(id.into());
            key.from_me = Some(from_me);
            let mut info = proto::WebMessageInfo::default();
            info.key = MessageField::some(key);
            info.participant = participant.map(Into::into);
            info.message = MessageField::some(msgx::conversation(text));
            info.message_timestamp = Some(ts);
            let mut h = proto::HistorySyncMsg::default();
            h.message = MessageField::some(info);
            h
        };
        let mut dm = proto::Conversation::default();
        dm.id = "999@lid".into();
        dm.display_name = Some("Bob".into());
        dm.ephemeral_expiration = Some(86_400);
        dm.messages = vec![
            wmi("a", false, None, "one", 10),
            wmi("b", true, None, "two", 20),
        ];
        let mut group = proto::Conversation::default();
        group.id = "1203@g.us".into();
        group.display_name = Some("Team".into());
        group.messages = vec![
            wmi("g1", false, Some("628333@s.whatsapp.net"), "hi", 30),
            wmi("g2", false, None, "dropped", 40),
        ];
        let mut data = proto::HistorySync::default();
        data.sync_type = T::INITIAL_BOOTSTRAP;
        data.conversations = vec![dm, group];
        store_history(&s, &R, KEY, Some(&own), &data).await.unwrap();

        let chat = s
            .get_chat(KEY, "628999@s.whatsapp.net")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((chat.name.as_str(), chat.last_message_time), ("Bob", 20));
        assert_eq!(chat.ephemeral_expiration, 86_400);
        let rows = messages(&s, "628999@s.whatsapp.net").await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].sender, "628111@s.whatsapp.net");
        assert_eq!(rows[1].sender, "628999@s.whatsapp.net");
        let rows = messages(&s, "1203@g.us").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sender, "628333@s.whatsapp.net");
        assert_eq!(
            s.get_chat(KEY, "1203@g.us").await.unwrap().unwrap().name,
            "Group 1203"
        );

        let mut pn = proto::Pushname::default();
        pn.id = Some("628555@s.whatsapp.net".into());
        pn.pushname = Some("Eve".into());
        let mut data = proto::HistorySync::default();
        data.sync_type = T::PUSH_NAME;
        data.pushnames = vec![pn];
        store_history(&s, &R, KEY, Some(&own), &data).await.unwrap();
        let eve = s
            .get_chat(KEY, "628555@s.whatsapp.net")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((eve.name.as_str(), eve.last_message_time), ("Eve", 0));
    }
}
