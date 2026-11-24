-- Durable queue of scheduled / recurring sends (GOWA `scheduled_sends`).
-- Times are unix seconds. `device_id` is the device slot id, as in GOWA.
-- A worker claims a row by moving it to `running` with a lease; a lapsed
-- lease is recovered on the next tick. `pending_message_id` (and the poll
-- secret) pin the identity of an occurrence across retries and crashes.
CREATE TABLE scheduled_sends (
    id TEXT PRIMARY KEY NOT NULL,
    device_id TEXT NOT NULL,
    message_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    assets_json TEXT NOT NULL DEFAULT '',
    phone TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    scheduled_at BIGINT NOT NULL,
    next_run_at BIGINT NOT NULL,
    timezone TEXT NOT NULL,
    recurrence TEXT NOT NULL DEFAULT 'once',
    weekdays_json TEXT NOT NULL DEFAULT '',
    day_of_month INTEGER NOT NULL DEFAULT 0,
    end_at BIGINT,
    occurrence_limit INTEGER NOT NULL DEFAULT 0,
    occurrence_count INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'active',
    lease_token TEXT NOT NULL DEFAULT '',
    lease_until BIGINT,
    last_run_at BIGINT,
    last_message_id TEXT NOT NULL DEFAULT '',
    last_error TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    pending_message_id TEXT NOT NULL DEFAULT '',
    pending_poll_secret BLOB
);

CREATE INDEX idx_scheduled_sends_due ON scheduled_sends (status, next_run_at);
CREATE INDEX idx_scheduled_sends_device ON scheduled_sends (device_id, status);
