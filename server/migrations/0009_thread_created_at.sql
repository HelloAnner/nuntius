PRAGMA foreign_keys = ON;

-- Keep creation order stable when a thread receives new activity. Existing
-- projections are backfilled until the connected Client sends its authoritative
-- creation timestamp in the next history inventory.
ALTER TABLE threads ADD COLUMN created_at TEXT;
UPDATE threads SET created_at = last_activity_at WHERE created_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_threads_user_created
    ON threads(user_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_threads_project_created
    ON threads(project_id, archived, created_at DESC, id DESC);
