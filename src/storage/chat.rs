//! Chat storage (GOWA `infrastructure/chatstorage/sqlite_repository.go`):
//! chats, messages, reactions, and edit history.
//!
//! Rows are partitioned by `device_id`, which is the account JID (non-AD)
//! once paired, else the slot id — the key GOWA uses. Times are unix seconds.
//! Every query is device-scoped; GOWA's global lookups are not ported.

use diesel::{
    prelude::*,
    sql_types::{BigInt, Binary, Bool, Integer, Nullable, Text},
};

use super::Db;

diesel::table! {
    chats (jid, device_id) {
        jid -> Text,
        device_id -> Text,
        name -> Text,
        last_message_time -> BigInt,
        ephemeral_expiration -> Integer,
        archived -> Bool,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    messages (id, chat_jid, device_id) {
        id -> Text,
        chat_jid -> Text,
        device_id -> Text,
        sender -> Text,
        content -> Text,
        timestamp -> BigInt,
        is_from_me -> Bool,
        media_type -> Text,
        call_metadata -> Text,
        filename -> Text,
        url -> Text,
        direct_path -> Text,
        media_key -> Nullable<Binary>,
        file_sha256 -> Nullable<Binary>,
        file_enc_sha256 -> Nullable<Binary>,
        file_length -> BigInt,
        referral_metadata -> Text,
        context_metadata -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    message_reactions (message_id, reactor_jid, device_id) {
        message_id -> Text,
        chat_jid -> Text,
        device_id -> Text,
        reactor_jid -> Text,
        emoji -> Text,
        is_from_me -> Bool,
        reaction_timestamp -> BigInt,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    message_edits (original_message_id, edit_event_id, device_id) {
        original_message_id -> Text,
        edit_event_id -> Text,
        chat_jid -> Text,
        device_id -> Text,
        editor -> Text,
        previous_content -> Text,
        new_content -> Text,
        edited_at -> BigInt,
        created_at -> BigInt,
    }
}

diesel::allow_tables_to_appear_in_same_query!(chats, messages, message_reactions, message_edits);

/// Query cap GOWA applies to `limit`.
pub const MAX_LIMIT: i64 = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Queryable, Selectable)]
#[diesel(table_name = chats)]
pub struct Chat {
    pub jid: String,
    pub device_id: String,
    pub name: String,
    pub last_message_time: i64,
    pub ephemeral_expiration: i32,
    pub archived: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Queryable, Selectable)]
#[diesel(table_name = messages)]
pub struct Message {
    pub id: String,
    pub chat_jid: String,
    pub device_id: String,
    pub sender: String,
    pub content: String,
    pub timestamp: i64,
    pub is_from_me: bool,
    pub media_type: String,
    pub call_metadata: String,
    pub filename: String,
    pub url: String,
    pub direct_path: String,
    pub media_key: Option<Vec<u8>>,
    pub file_sha256: Option<Vec<u8>>,
    pub file_enc_sha256: Option<Vec<u8>>,
    pub file_length: i64,
    pub referral_metadata: String,
    pub context_metadata: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Message {
    /// GOWA skips rows with neither text nor media.
    fn is_empty(&self) -> bool {
        self.content.is_empty() && self.media_type.is_empty()
    }
}

/// A message with its reactions, as list queries return it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageWithReactions {
    pub message: Message,
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = message_reactions)]
pub struct Reaction {
    pub message_id: String,
    pub chat_jid: String,
    pub device_id: String,
    pub reactor_jid: String,
    pub emoji: String,
    pub is_from_me: bool,
    pub reaction_timestamp: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = message_edits)]
pub struct MessageEdit {
    pub original_message_id: String,
    pub edit_event_id: String,
    pub chat_jid: String,
    pub device_id: String,
    pub editor: String,
    pub previous_content: String,
    pub new_content: String,
    pub edited_at: i64,
    pub created_at: i64,
}

/// GOWA `ChatFilter`.
#[derive(Debug, Clone, Default)]
pub struct ChatFilter {
    pub device_id: String,
    pub limit: i64,
    pub offset: i64,
    pub search_name: String,
    pub has_media: bool,
    pub archived: Option<bool>,
}

/// GOWA `MessageFilter`.
#[derive(Debug, Clone, Default)]
pub struct MessageFilter {
    pub device_id: String,
    pub chat_jid: String,
    pub limit: i64,
    pub offset: i64,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub media_only: bool,
    pub is_from_me: Option<bool>,
}

/// Outcome of [`ChatStore::apply_edit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    Applied,
    /// The editor is not the author, or the original is unknown and the
    /// edit came from a peer (GOWA refuses to create a stub for it).
    Rejected,
}

#[derive(Clone)]
pub struct ChatStore {
    db: Db,
}

const UPSERT_MESSAGE: &str = "
    INSERT INTO messages (
        id, chat_jid, device_id, sender, content, timestamp, is_from_me,
        media_type, call_metadata, filename, url, direct_path, media_key, file_sha256,
        file_enc_sha256, file_length, referral_metadata, context_metadata, created_at, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT (id, chat_jid, device_id) DO UPDATE SET
        sender = excluded.sender,
        content = CONTENT_RULE,
        timestamp = excluded.timestamp,
        is_from_me = excluded.is_from_me,
        media_type = excluded.media_type,
        call_metadata = excluded.call_metadata,
        filename = excluded.filename,
        url = excluded.url,
        direct_path = excluded.direct_path,
        media_key = excluded.media_key,
        file_sha256 = excluded.file_sha256,
        file_enc_sha256 = excluded.file_enc_sha256,
        file_length = excluded.file_length,
        referral_metadata = excluded.referral_metadata,
        context_metadata = COALESCE(NULLIF(excluded.context_metadata, ''), messages.context_metadata, ''),
        updated_at = excluded.updated_at
";

/// Replays overwrite content (GOWA `StoreMessage`).
const CONTENT_REPLACE: &str = "excluded.content";
/// A sent message never rolls back an edit stored before it (GOWA
/// `storeSentMessagePreservingEdits`).
const CONTENT_KEEP_EDITED: &str = "CASE WHEN EXISTS (
            SELECT 1 FROM message_edits e
            WHERE e.original_message_id = messages.id
              AND e.chat_jid = messages.chat_jid
              AND e.device_id = messages.device_id
        ) THEN messages.content ELSE excluded.content END";

fn upsert_message(
    conn: &mut SqliteConnection,
    m: &Message,
    content_rule: &str,
    now: i64,
) -> QueryResult<usize> {
    if m.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(UPSERT_MESSAGE.replace("CONTENT_RULE", content_rule))
        .bind::<Text, _>(&m.id)
        .bind::<Text, _>(&m.chat_jid)
        .bind::<Text, _>(&m.device_id)
        .bind::<Text, _>(&m.sender)
        .bind::<Text, _>(&m.content)
        .bind::<BigInt, _>(m.timestamp)
        .bind::<Bool, _>(m.is_from_me)
        .bind::<Text, _>(&m.media_type)
        .bind::<Text, _>(&m.call_metadata)
        .bind::<Text, _>(&m.filename)
        .bind::<Text, _>(&m.url)
        .bind::<Text, _>(&m.direct_path)
        .bind::<Nullable<Binary>, _>(&m.media_key)
        .bind::<Nullable<Binary>, _>(&m.file_sha256)
        .bind::<Nullable<Binary>, _>(&m.file_enc_sha256)
        .bind::<BigInt, _>(m.file_length)
        .bind::<Text, _>(&m.referral_metadata)
        .bind::<Text, _>(&m.context_metadata)
        .bind::<BigInt, _>(now)
        .bind::<BigInt, _>(now)
        .execute(conn)
}

fn upsert_chat(conn: &mut SqliteConnection, chat: &Chat, now: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO chats (jid, device_id, name, last_message_time, ephemeral_expiration, archived, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (jid, device_id) DO UPDATE SET
            name = excluded.name,
            last_message_time = excluded.last_message_time,
            ephemeral_expiration = excluded.ephemeral_expiration,
            archived = excluded.archived,
            updated_at = excluded.updated_at",
    )
    .bind::<Text, _>(&chat.jid)
    .bind::<Text, _>(&chat.device_id)
    .bind::<Text, _>(&chat.name)
    .bind::<BigInt, _>(chat.last_message_time)
    .bind::<Integer, _>(chat.ephemeral_expiration)
    .bind::<Bool, _>(chat.archived)
    .bind::<BigInt, _>(now)
    .bind::<BigInt, _>(now)
    .execute(conn)
}

fn find_message(
    conn: &mut SqliteConnection,
    device: &str,
    chat: &str,
    id: &str,
) -> QueryResult<Option<Message>> {
    messages::table
        .filter(messages::device_id.eq(device))
        .filter(messages::chat_jid.eq(chat))
        .filter(messages::id.eq(id))
        .select(Message::as_select())
        .first(conn)
        .optional()
}

fn delete_message_rows(
    conn: &mut SqliteConnection,
    device: &str,
    chat: &str,
    id: &str,
) -> QueryResult<usize> {
    diesel::delete(
        message_reactions::table
            .filter(message_reactions::device_id.eq(device))
            .filter(message_reactions::chat_jid.eq(chat))
            .filter(message_reactions::message_id.eq(id)),
    )
    .execute(conn)?;
    diesel::delete(
        message_edits::table
            .filter(message_edits::device_id.eq(device))
            .filter(message_edits::chat_jid.eq(chat))
            .filter(message_edits::original_message_id.eq(id)),
    )
    .execute(conn)?;
    diesel::delete(
        messages::table
            .filter(messages::device_id.eq(device))
            .filter(messages::chat_jid.eq(chat))
            .filter(messages::id.eq(id)),
    )
    .execute(conn)
}

/// SQL `LIKE` pattern matching `needle` literally (escape char `\`).
fn like_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in needle.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

fn clamp_limit(limit: i64) -> i64 {
    limit.min(MAX_LIMIT)
}

impl ChatStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // ---- chats ----

    /// Create or update a chat (GOWA `StoreChat`); `created_at` is kept.
    pub async fn store_chat(&self, chat: Chat) -> anyhow::Result<()> {
        let now = crate::device::now();
        self.db.run(move |c| upsert_chat(c, &chat, now)).await?;
        Ok(())
    }

    pub async fn get_chat(&self, device: &str, jid: &str) -> anyhow::Result<Option<Chat>> {
        let (device, jid) = (device.to_owned(), jid.to_owned());
        self.db
            .run(move |c| {
                chats::table
                    .find((jid, device))
                    .select(Chat::as_select())
                    .first(c)
                    .optional()
            })
            .await
    }

    /// First stored chat among `keys` (GOWA `storedChat`).
    pub async fn first_chat(&self, device: &str, keys: &[String]) -> anyhow::Result<Option<Chat>> {
        for key in keys {
            if let Some(chat) = self.get_chat(device, key).await? {
                return Ok(Some(chat));
            }
        }
        Ok(None)
    }

    fn chat_query(filter: &ChatFilter) -> chats::BoxedQuery<'static, diesel::sqlite::Sqlite> {
        let mut q = chats::table
            .filter(chats::device_id.eq(filter.device_id.clone()))
            .into_boxed();
        if !filter.search_name.is_empty() {
            q = q.filter(
                chats::name
                    .like(like_pattern(&filter.search_name))
                    .escape('\\'),
            );
        }
        if filter.has_media {
            q = q.filter(diesel::dsl::exists(
                messages::table
                    .filter(messages::chat_jid.eq(chats::jid))
                    .filter(messages::device_id.eq(chats::device_id))
                    .filter(messages::media_type.ne_all(["", "call"])),
            ));
        }
        if let Some(archived) = filter.archived {
            q = q.filter(chats::archived.eq(archived));
        }
        q
    }

    /// GOWA `GetChats`: newest activity first.
    pub async fn list_chats(&self, filter: ChatFilter) -> anyhow::Result<Vec<Chat>> {
        self.db
            .run(move |c| {
                let mut q = Self::chat_query(&filter)
                    .order(chats::last_message_time.desc())
                    .select(Chat::as_select());
                if filter.limit > 0 {
                    q = q.limit(clamp_limit(filter.limit));
                    if filter.offset > 0 {
                        q = q.offset(filter.offset);
                    }
                }
                q.load(c)
            })
            .await
    }

    /// GOWA `GetFilteredChatCount`.
    pub async fn count_chats(&self, filter: ChatFilter) -> anyhow::Result<i64> {
        self.db
            .run(move |c| Self::chat_query(&filter).count().get_result(c))
            .await
    }

    /// Delete a chat with its messages, reactions and edits.
    pub async fn delete_chat(&self, device: &str, jid: &str) -> anyhow::Result<()> {
        let (device, jid) = (device.to_owned(), jid.to_owned());
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    diesel::delete(
                        message_reactions::table
                            .filter(message_reactions::device_id.eq(&device))
                            .filter(message_reactions::chat_jid.eq(&jid)),
                    )
                    .execute(c)?;
                    diesel::delete(
                        message_edits::table
                            .filter(message_edits::device_id.eq(&device))
                            .filter(message_edits::chat_jid.eq(&jid)),
                    )
                    .execute(c)?;
                    diesel::delete(
                        messages::table
                            .filter(messages::device_id.eq(&device))
                            .filter(messages::chat_jid.eq(&jid)),
                    )
                    .execute(c)?;
                    diesel::delete(chats::table.find((&jid, &device))).execute(c)
                })
            })
            .await?;
        Ok(())
    }

    // ---- messages ----

    /// Create or update a message (GOWA `StoreMessage`). Empty
    /// `context_metadata` keeps any stored reply context.
    pub async fn store_message(&self, message: Message) -> anyhow::Result<()> {
        let now = crate::device::now();
        self.db
            .run(move |c| upsert_message(c, &message, CONTENT_REPLACE, now))
            .await?;
        Ok(())
    }

    /// Store a message this account sent, without undoing an edit that was
    /// stored first (GOWA `storeSentMessagePreservingEdits`).
    pub async fn store_sent_message(&self, message: Message) -> anyhow::Result<()> {
        let now = crate::device::now();
        self.db
            .run(move |c| upsert_message(c, &message, CONTENT_KEEP_EDITED, now))
            .await?;
        Ok(())
    }

    /// One transaction for a history-sync batch (GOWA `StoreMessagesBatch`).
    pub async fn store_messages(&self, batch: Vec<Message>) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let now = crate::device::now();
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    for m in &batch {
                        upsert_message(c, m, CONTENT_REPLACE, now)?;
                    }
                    Ok(())
                })
            })
            .await
    }

    /// Device-scoped lookup by id (GOWA `GetMessageByIDAndDevice`).
    pub async fn get_message(&self, device: &str, id: &str) -> anyhow::Result<Option<Message>> {
        let (device, id) = (device.to_owned(), id.to_owned());
        self.db
            .run(move |c| {
                messages::table
                    .filter(messages::device_id.eq(device))
                    .filter(messages::id.eq(id))
                    .select(Message::as_select())
                    .first(c)
                    .optional()
            })
            .await
    }

    /// GOWA `GetMessageByIDChatAndDevice`.
    pub async fn get_message_in_chat(
        &self,
        device: &str,
        chat: &str,
        id: &str,
    ) -> anyhow::Result<Option<Message>> {
        let (device, chat, id) = (device.to_owned(), chat.to_owned(), id.to_owned());
        self.db
            .run(move |c| find_message(c, &device, &chat, &id))
            .await
    }

    /// GOWA `DeleteMessageByDevice` (reactions and edits go with it).
    pub async fn delete_message(&self, device: &str, chat: &str, id: &str) -> anyhow::Result<()> {
        let (device, chat, id) = (device.to_owned(), chat.to_owned(), id.to_owned());
        self.db
            .run(move |c| c.transaction(|c| delete_message_rows(c, &device, &chat, &id)))
            .await?;
        Ok(())
    }

    fn message_query(
        filter: &MessageFilter,
    ) -> messages::BoxedQuery<'static, diesel::sqlite::Sqlite> {
        let mut q = messages::table
            .filter(messages::chat_jid.eq(filter.chat_jid.clone()))
            .filter(messages::device_id.eq(filter.device_id.clone()))
            .into_boxed();
        if let Some(start) = filter.start_time {
            q = q.filter(messages::timestamp.ge(start));
        }
        if let Some(end) = filter.end_time {
            q = q.filter(messages::timestamp.le(end));
        }
        if filter.media_only {
            q = q.filter(messages::media_type.ne_all(["", "call"]));
        }
        if let Some(from_me) = filter.is_from_me {
            q = q.filter(messages::is_from_me.eq(from_me));
        }
        q
    }

    /// GOWA `GetMessages`: newest first, with reactions.
    pub async fn list_messages(
        &self,
        filter: MessageFilter,
    ) -> anyhow::Result<Vec<MessageWithReactions>> {
        anyhow::ensure!(
            !filter.device_id.is_empty(),
            "device_id is required for message queries (data isolation)"
        );
        self.db
            .run(move |c| {
                let mut q = Self::message_query(&filter)
                    .order(messages::timestamp.desc())
                    .select(Message::as_select());
                if filter.limit > 0 {
                    q = q.limit(clamp_limit(filter.limit));
                    if filter.offset > 0 {
                        q = q.offset(filter.offset);
                    }
                }
                let rows = q.load(c)?;
                with_reactions(c, &filter.device_id, &filter.chat_jid, rows)
            })
            .await
    }

    /// Count for the same filter as [`list_messages`](Self::list_messages).
    pub async fn count_messages(&self, filter: MessageFilter) -> anyhow::Result<i64> {
        self.db
            .run(move |c| Self::message_query(&filter).count().get_result(c))
            .await
    }

    /// GOWA `SearchMessages`: case-insensitive substring of the content.
    pub async fn search_messages(
        &self,
        device: &str,
        chat: &str,
        text: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<MessageWithReactions>> {
        anyhow::ensure!(
            !device.is_empty(),
            "device_id is required for message search (data isolation)"
        );
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let (device, chat) = (device.to_owned(), chat.to_owned());
        let pattern = like_pattern(&text.to_lowercase());
        self.db
            .run(move |c| {
                let mut q = messages::table
                    .filter(messages::chat_jid.eq(&chat))
                    .filter(messages::device_id.eq(&device))
                    .filter(
                        diesel::dsl::sql::<Text>("LOWER(content)")
                            .like(pattern)
                            .escape('\\'),
                    )
                    .order(messages::timestamp.desc())
                    .select(Message::as_select())
                    .into_boxed();
                if limit > 0 {
                    q = q.limit(clamp_limit(limit));
                }
                let rows = q.load(c)?;
                with_reactions(c, &device, &chat, rows)
            })
            .await
    }

    // ---- reactions ----

    /// Create, update, or (empty emoji) remove a reaction (GOWA `StoreReaction`).
    pub async fn store_reaction(&self, mut reaction: Reaction) -> anyhow::Result<()> {
        let now = crate::device::now();
        self.db
            .run(move |c| {
                let key = message_reactions::table.find((
                    reaction.message_id.clone(),
                    reaction.reactor_jid.clone(),
                    reaction.device_id.clone(),
                ));
                if reaction.emoji.is_empty() {
                    return diesel::delete(key).execute(c);
                }
                reaction.created_at = now;
                reaction.updated_at = now;
                diesel::insert_into(message_reactions::table)
                    .values(&reaction)
                    .on_conflict((
                        message_reactions::message_id,
                        message_reactions::reactor_jid,
                        message_reactions::device_id,
                    ))
                    .do_update()
                    .set((
                        message_reactions::chat_jid.eq(&reaction.chat_jid),
                        message_reactions::emoji.eq(&reaction.emoji),
                        message_reactions::is_from_me.eq(reaction.is_from_me),
                        message_reactions::reaction_timestamp.eq(reaction.reaction_timestamp),
                        message_reactions::updated_at.eq(now),
                    ))
                    .execute(c)
            })
            .await?;
        Ok(())
    }

    // ---- edits ----

    /// Apply an edit and record it in the history (GOWA `storeEditedMessage`).
    ///
    /// `stub` is the row to create when the original is not stored; only
    /// edits from this account may create one. A stored original is only
    /// updated when `authorized(original)` holds.
    pub async fn apply_edit(
        &self,
        edit: MessageEdit,
        new_context: String,
        stub: Option<Message>,
        authorized: impl FnOnce(&Message) -> bool + Send + 'static,
    ) -> anyhow::Result<EditOutcome> {
        let now = crate::device::now();
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    let mut edit = edit;
                    let existing = find_message(
                        c,
                        &edit.device_id,
                        &edit.chat_jid,
                        &edit.original_message_id,
                    )?;
                    match existing {
                        Some(original) => {
                            if !authorized(&original) {
                                return Ok(EditOutcome::Rejected);
                            }
                            edit.previous_content = original.content.clone();
                            diesel::sql_query(
                                "UPDATE messages SET content = ?,
                                    context_metadata = COALESCE(NULLIF(?, ''), context_metadata, ''),
                                    updated_at = ?
                                 WHERE id = ? AND chat_jid = ? AND device_id = ?",
                            )
                            .bind::<Text, _>(&edit.new_content)
                            .bind::<Text, _>(&new_context)
                            .bind::<BigInt, _>(now)
                            .bind::<Text, _>(&edit.original_message_id)
                            .bind::<Text, _>(&edit.chat_jid)
                            .bind::<Text, _>(&edit.device_id)
                            .execute(c)?;
                        }
                        None => {
                            let Some(mut stub) = stub else {
                                return Ok(EditOutcome::Rejected);
                            };
                            stub.context_metadata = new_context;
                            // Unlike a replay, the stub is written even when
                            // empty so the edit row has a parent.
                            diesel::sql_query(UPSERT_MESSAGE.replace("CONTENT_RULE", CONTENT_REPLACE))
                                .bind::<Text, _>(&stub.id)
                                .bind::<Text, _>(&stub.chat_jid)
                                .bind::<Text, _>(&stub.device_id)
                                .bind::<Text, _>(&stub.sender)
                                .bind::<Text, _>(&stub.content)
                                .bind::<BigInt, _>(stub.timestamp)
                                .bind::<Bool, _>(stub.is_from_me)
                                .bind::<Text, _>(&stub.media_type)
                                .bind::<Text, _>(&stub.call_metadata)
                                .bind::<Text, _>(&stub.filename)
                                .bind::<Text, _>(&stub.url)
                                .bind::<Text, _>(&stub.direct_path)
                                .bind::<Nullable<Binary>, _>(&stub.media_key)
                                .bind::<Nullable<Binary>, _>(&stub.file_sha256)
                                .bind::<Nullable<Binary>, _>(&stub.file_enc_sha256)
                                .bind::<BigInt, _>(stub.file_length)
                                .bind::<Text, _>(&stub.referral_metadata)
                                .bind::<Text, _>(&stub.context_metadata)
                                .bind::<BigInt, _>(now)
                                .bind::<BigInt, _>(now)
                                .execute(c)?;
                        }
                    }
                    edit.created_at = now;
                    diesel::insert_into(message_edits::table)
                        .values(&edit)
                        .on_conflict_do_nothing()
                        .execute(c)?;
                    Ok(EditOutcome::Applied)
                })
            })
            .await
    }

    /// Edit history of a message, oldest first (GOWA `GetMessageEdits`).
    pub async fn message_edits(&self, device: &str, id: &str) -> anyhow::Result<Vec<MessageEdit>> {
        let (device, id) = (device.to_owned(), id.to_owned());
        self.db
            .run(move |c| {
                message_edits::table
                    .filter(message_edits::device_id.eq(device))
                    .filter(message_edits::original_message_id.eq(id))
                    .order((
                        message_edits::edited_at.asc(),
                        message_edits::created_at.asc(),
                    ))
                    .select(MessageEdit::as_select())
                    .load(c)
            })
            .await
    }

    // ---- maintenance ----

    /// Drop everything stored under a partition key (GOWA `DeleteDeviceData`).
    pub async fn delete_device_data(&self, device: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!device.is_empty(), "device id is required");
        let device = device.to_owned();
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    diesel::delete(
                        message_reactions::table.filter(message_reactions::device_id.eq(&device)),
                    )
                    .execute(c)?;
                    diesel::delete(
                        message_edits::table.filter(message_edits::device_id.eq(&device)),
                    )
                    .execute(c)?;
                    diesel::delete(messages::table.filter(messages::device_id.eq(&device)))
                        .execute(c)?;
                    diesel::delete(chats::table.filter(chats::device_id.eq(&device))).execute(c)
                })
            })
            .await?;
        Ok(())
    }
}

/// Attach reactions, oldest first (GOWA `loadMessageReactions`).
fn with_reactions(
    conn: &mut SqliteConnection,
    device: &str,
    chat: &str,
    rows: Vec<Message>,
) -> QueryResult<Vec<MessageWithReactions>> {
    let mut by_id: std::collections::HashMap<String, Vec<Reaction>> = Default::default();
    let ids: Vec<&str> = rows.iter().map(|m| m.id.as_str()).collect();
    for batch in ids.chunks(500) {
        let found: Vec<Reaction> = message_reactions::table
            .filter(message_reactions::device_id.eq(device))
            .filter(message_reactions::chat_jid.eq(chat))
            .filter(message_reactions::message_id.eq_any(batch))
            .order((
                message_reactions::reaction_timestamp.asc(),
                message_reactions::created_at.asc(),
            ))
            .select(Reaction::as_select())
            .load(conn)?;
        for r in found {
            by_id.entry(r.message_id.clone()).or_default().push(r);
        }
    }
    Ok(rows
        .into_iter()
        .map(|message| {
            let reactions = by_id.remove(&message.id).unwrap_or_default();
            MessageWithReactions { message, reactions }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = "628111@s.whatsapp.net";
    const CHAT: &str = "628222@s.whatsapp.net";

    fn store() -> ChatStore {
        ChatStore::new(Db::open(":memory:").unwrap())
    }

    fn msg(id: &str, ts: i64, content: &str) -> Message {
        Message {
            id: id.into(),
            chat_jid: CHAT.into(),
            device_id: DEV.into(),
            sender: CHAT.into(),
            content: content.into(),
            timestamp: ts,
            ..Default::default()
        }
    }

    fn chat(jid: &str, name: &str, last: i64) -> Chat {
        Chat {
            jid: jid.into(),
            device_id: DEV.into(),
            name: name.into(),
            last_message_time: last,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn chats_upsert_filter_and_order() {
        let s = store();
        s.store_chat(chat("a@s.whatsapp.net", "Alice", 10))
            .await
            .unwrap();
        s.store_chat(chat("b@s.whatsapp.net", "Bob_x", 20))
            .await
            .unwrap();
        let mut other = chat("c@s.whatsapp.net", "Carol", 30);
        other.device_id = "other".into();
        s.store_chat(other).await.unwrap();

        let mut renamed = chat("a@s.whatsapp.net", "Alicia", 15);
        renamed.archived = true;
        s.store_chat(renamed).await.unwrap();

        let all = s
            .list_chats(ChatFilter {
                device_id: DEV.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Bob_x", "Alicia"]
        );

        let f = |search: &str, archived| ChatFilter {
            device_id: DEV.into(),
            search_name: search.into(),
            archived,
            ..Default::default()
        };
        assert_eq!(s.count_chats(f("lic", None)).await.unwrap(), 1);
        // `_` is literal, not a wildcard.
        assert_eq!(s.count_chats(f("b_", None)).await.unwrap(), 1);
        assert_eq!(s.count_chats(f("o_x", None)).await.unwrap(), 0);
        assert_eq!(s.count_chats(f("", Some(true))).await.unwrap(), 1);
        assert_eq!(s.count_chats(f("", Some(false))).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn message_replay_keeps_reply_context() {
        let s = store();
        let mut m = msg("1", 100, "hi");
        m.context_metadata = r#"{"replied_to_id":"0"}"#.into();
        s.store_message(m).await.unwrap();
        s.store_message(msg("1", 100, "hi again")).await.unwrap();
        let got = s.get_message(DEV, "1").await.unwrap().unwrap();
        assert_eq!(got.content, "hi again");
        assert_eq!(got.context_metadata, r#"{"replied_to_id":"0"}"#);
        // Empty rows are skipped.
        s.store_message(msg("2", 100, "")).await.unwrap();
        assert!(s.get_message(DEV, "2").await.unwrap().is_none());
        // Other devices cannot see it.
        assert!(s.get_message("other", "1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_messages_newest_first_with_reactions() {
        let s = store();
        s.store_messages(vec![
            msg("1", 100, "a"),
            msg("2", 200, "b"),
            msg("3", 300, "c"),
        ])
        .await
        .unwrap();
        let mut media = msg("4", 400, "");
        media.media_type = "image".into();
        s.store_message(media).await.unwrap();
        let reaction = |reactor: &str, emoji: &str, ts| Reaction {
            message_id: "2".into(),
            chat_jid: CHAT.into(),
            device_id: DEV.into(),
            reactor_jid: reactor.into(),
            emoji: emoji.into(),
            reaction_timestamp: ts,
            ..Default::default()
        };
        s.store_reaction(reaction("x", "👍", 5)).await.unwrap();
        s.store_reaction(reaction("y", "❤️", 3)).await.unwrap();
        s.store_reaction(reaction("x", "😂", 6)).await.unwrap();

        let filter = MessageFilter {
            device_id: DEV.into(),
            chat_jid: CHAT.into(),
            limit: 2,
            offset: 1,
            ..Default::default()
        };
        let page = s.list_messages(filter.clone()).await.unwrap();
        assert_eq!(
            page.iter()
                .map(|m| m.message.id.as_str())
                .collect::<Vec<_>>(),
            ["3", "2"]
        );
        let emojis: Vec<_> = page[1].reactions.iter().map(|r| r.emoji.as_str()).collect();
        assert_eq!(emojis, ["❤️", "😂"]);
        assert_eq!(s.count_messages(filter.clone()).await.unwrap(), 4);

        let media_only = MessageFilter {
            media_only: true,
            limit: 0,
            offset: 0,
            ..filter.clone()
        };
        assert_eq!(s.list_messages(media_only).await.unwrap().len(), 1);
        let ranged = MessageFilter {
            start_time: Some(150),
            end_time: Some(300),
            limit: 0,
            offset: 0,
            ..filter
        };
        assert_eq!(s.count_messages(ranged).await.unwrap(), 2);

        // Empty emoji removes the reaction.
        s.store_reaction(reaction("y", "", 7)).await.unwrap();
        let found = s.search_messages(DEV, CHAT, "B", 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].reactions.len(), 1);
        assert!(
            s.search_messages(DEV, CHAT, "  ", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            s.search_messages(DEV, CHAT, "%", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn edits_are_authorized_and_survive_sent_replay() {
        let s = store();
        s.store_message(msg("1", 100, "old")).await.unwrap();
        let edit = |event: &str, text: &str| MessageEdit {
            original_message_id: "1".into(),
            edit_event_id: event.into(),
            chat_jid: CHAT.into(),
            device_id: DEV.into(),
            editor: CHAT.into(),
            new_content: text.into(),
            edited_at: 200,
            ..Default::default()
        };
        let out = s
            .apply_edit(edit("e1", "nope"), String::new(), None, |_| false)
            .await
            .unwrap();
        assert_eq!(out, EditOutcome::Rejected);
        let out = s
            .apply_edit(edit("e1", "new"), String::new(), None, |m| m.sender == CHAT)
            .await
            .unwrap();
        assert_eq!(out, EditOutcome::Applied);
        let hist = s.message_edits(DEV, "1").await.unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].previous_content, "old");

        // A late "sent" write does not roll the edit back.
        s.store_sent_message(msg("1", 100, "old")).await.unwrap();
        assert_eq!(
            s.get_message(DEV, "1").await.unwrap().unwrap().content,
            "new"
        );

        // Unknown original: no stub → rejected; stub → created.
        let mut e = edit("e2", "x");
        e.original_message_id = "9".into();
        assert_eq!(
            s.apply_edit(e.clone(), String::new(), None, |_| true)
                .await
                .unwrap(),
            EditOutcome::Rejected
        );
        let stub = msg("9", 100, "x");
        assert_eq!(
            s.apply_edit(e, String::new(), Some(stub), |_| true)
                .await
                .unwrap(),
            EditOutcome::Applied
        );
        assert!(s.get_message(DEV, "9").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn deletes_cascade() {
        let s = store();
        s.store_chat(chat(CHAT, "Bob", 100)).await.unwrap();
        s.store_message(msg("1", 100, "a")).await.unwrap();
        s.store_message(msg("2", 100, "b")).await.unwrap();
        s.store_reaction(Reaction {
            message_id: "1".into(),
            chat_jid: CHAT.into(),
            device_id: DEV.into(),
            reactor_jid: "x".into(),
            emoji: "👍".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        s.delete_message(DEV, CHAT, "1").await.unwrap();
        assert!(s.get_message(DEV, "1").await.unwrap().is_none());
        s.store_message(msg("1", 100, "a")).await.unwrap();
        let listed = s
            .list_messages(MessageFilter {
                device_id: DEV.into(),
                chat_jid: CHAT.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(listed.iter().all(|m| m.reactions.is_empty()));

        s.delete_chat(DEV, CHAT).await.unwrap();
        assert!(s.get_chat(DEV, CHAT).await.unwrap().is_none());
        assert!(s.get_message(DEV, "2").await.unwrap().is_none());

        s.store_chat(chat(CHAT, "Bob", 100)).await.unwrap();
        s.store_message(msg("3", 100, "c")).await.unwrap();
        s.delete_device_data(DEV).await.unwrap();
        assert!(s.get_chat(DEV, CHAT).await.unwrap().is_none());
        assert!(s.get_message(DEV, "3").await.unwrap().is_none());
    }
}
