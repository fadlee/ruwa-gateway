-- Device slots (GOWA chatstorage `devices` table, minus chat columns).
CREATE TABLE devices (
    device_id TEXT PRIMARY KEY NOT NULL,
    -- Directory name under storages/devices/ (safe form of device_id).
    storage_key TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL DEFAULT '',
    jid TEXT NOT NULL DEFAULT '',
    webhook_url TEXT,
    webhook_secret TEXT NOT NULL DEFAULT '',
    webhook_events TEXT NOT NULL DEFAULT '',
    webhook_insecure_skip_verify BOOLEAN NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
