PRAGMA foreign_keys = ON;

-- Force one complete authoritative snapshot after upgrading. The server uses
-- it to remove stale projection rows left behind by older sync behavior.
DELETE FROM runtime_state WHERE key LIKE 'history_hash:%';

CREATE INDEX IF NOT EXISTS idx_client_turns_chronology
    ON turns(thread_id, started_at, ordinal, id);
CREATE INDEX IF NOT EXISTS idx_client_items_chronology
    ON items(turn_id, occurred_at, ordinal, id);
