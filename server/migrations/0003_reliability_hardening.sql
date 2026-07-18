PRAGMA foreign_keys = ON;

-- Keep several independently issued CSRF tokens valid for one browser session so
-- multiple tabs cannot invalidate each other merely by refreshing /auth/session.
CREATE TABLE IF NOT EXISTS web_csrf_tokens (
    session_id TEXT NOT NULL REFERENCES web_sessions(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(session_id, token_hash)
);

INSERT OR IGNORE INTO web_csrf_tokens(session_id, token_hash, created_at)
SELECT id, csrf_token_hash, created_at FROM web_sessions;

-- Allocate command sequence numbers atomically instead of using MAX()+1.
CREATE TABLE IF NOT EXISTS device_command_sequences (
    device_id TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    next_sequence INTEGER NOT NULL
);

INSERT OR IGNORE INTO device_command_sequences(device_id, next_sequence)
SELECT d.id, COALESCE(MAX(c.server_sequence), 0) + 1
FROM devices d
LEFT JOIN commands c ON c.device_id = d.id
GROUP BY d.id;

CREATE UNIQUE INDEX IF NOT EXISTS idx_commands_device_sequence
    ON commands(device_id, server_sequence);

ALTER TABLE history_sync_batches
    ADD COLUMN inventory_revision INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_history_batches_revision
    ON history_sync_batches(device_id, thread_id, inventory_revision, committed_at);

ALTER TABLE devices ADD COLUMN app_server_status TEXT;
ALTER TABLE devices ADD COLUMN storage_status TEXT;
ALTER TABLE devices ADD COLUMN inbox_depth INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN outbox_depth INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN history_backfill_depth INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_audit_events_created
    ON audit_events(created_at DESC);
