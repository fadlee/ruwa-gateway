-- Chat storage (GOWA chatstorage `chats`, `messages`, `message_reactions`,
-- `message_edits`). `device_id` is the partition key GOWA uses: the account
-- JID (non-AD) once paired, else the slot id. Times are unix seconds.
CREATE TABLE chats (
    jid TEXT NOT NULL,
    device_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    last_message_time BIGINT NOT NULL DEFAULT 0,
    ephemeral_expiration INTEGER NOT NULL DEFAULT 0,
    archived BOOLEAN NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (jid, device_id)
);
CREATE INDEX idx_chats_device_last ON chats(device_id, last_message_time);
CREATE INDEX idx_chats_name ON chats(name);

CREATE TABLE messages (
    id TEXT NOT NULL,
    chat_jid TEXT NOT NULL,
    device_id TEXT NOT NULL,
    sender TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL DEFAULT '',
    timestamp BIGINT NOT NULL,
    is_from_me BOOLEAN NOT NULL DEFAULT 0,
    media_type TEXT NOT NULL DEFAULT '',
    call_metadata TEXT NOT NULL DEFAULT '',
    filename TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    direct_path TEXT NOT NULL DEFAULT '',
    media_key BLOB,
    file_sha256 BLOB,
    file_enc_sha256 BLOB,
    file_length BIGINT NOT NULL DEFAULT 0,
    referral_metadata TEXT NOT NULL DEFAULT '',
    context_metadata TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (id, chat_jid, device_id)
);
CREATE INDEX idx_messages_chat_device_ts ON messages(chat_jid, device_id, timestamp);
CREATE INDEX idx_messages_device_id ON messages(device_id, id);

CREATE TABLE message_reactions (
    message_id TEXT NOT NULL,
    chat_jid TEXT NOT NULL,
    device_id TEXT NOT NULL,
    reactor_jid TEXT NOT NULL,
    emoji TEXT NOT NULL DEFAULT '',
    is_from_me BOOLEAN NOT NULL DEFAULT 0,
    reaction_timestamp BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (message_id, reactor_jid, device_id)
);
CREATE INDEX idx_message_reactions_lookup ON message_reactions(device_id, chat_jid, message_id);

CREATE TABLE message_edits (
    original_message_id TEXT NOT NULL,
    edit_event_id TEXT NOT NULL,
    chat_jid TEXT NOT NULL,
    device_id TEXT NOT NULL,
    editor TEXT NOT NULL DEFAULT '',
    previous_content TEXT NOT NULL DEFAULT '',
    new_content TEXT NOT NULL DEFAULT '',
    edited_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (original_message_id, edit_event_id, device_id)
);
CREATE INDEX idx_message_edits_original ON message_edits(device_id, original_message_id);
