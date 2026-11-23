-- Address book per device, standing in for whatsmeow's contact store
-- (`whatsmeow_contacts`), which GOWA reads for chat and sender names.
-- Filled from app-state contact actions, push names, and verified names.
CREATE TABLE contacts (
    device_id TEXT NOT NULL,
    jid TEXT NOT NULL,
    first_name TEXT NOT NULL DEFAULT '',
    full_name TEXT NOT NULL DEFAULT '',
    push_name TEXT NOT NULL DEFAULT '',
    business_name TEXT NOT NULL DEFAULT '',
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (device_id, jid)
);
