PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS server_runtime_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

ALTER TABLE commands ADD COLUMN queue_epoch TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE commands ADD COLUMN error_message TEXT;

DROP INDEX IF EXISTS idx_commands_device_sequence;
CREATE UNIQUE INDEX IF NOT EXISTS idx_commands_epoch_sequence
    ON commands(device_id, queue_epoch, server_sequence);
