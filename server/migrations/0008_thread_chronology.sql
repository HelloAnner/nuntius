PRAGMA foreign_keys = ON;

-- A history revision is a complete device-side snapshot. Mark every projected
-- row so the final chunk can remove records that disappeared from SQLite on
-- the device (including duplicates created by older reconnect handling).
ALTER TABLE turns ADD COLUMN snapshot_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE items ADD COLUMN snapshot_revision INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_server_turns_chronology
    ON turns(thread_id, started_at, ordinal, id);
CREATE INDEX IF NOT EXISTS idx_server_items_chronology
    ON items(turn_id, occurred_at, ordinal, id);
CREATE INDEX IF NOT EXISTS idx_server_turns_snapshot
    ON turns(thread_id, snapshot_revision);
CREATE INDEX IF NOT EXISTS idx_server_items_snapshot
    ON items(turn_id, snapshot_revision);
