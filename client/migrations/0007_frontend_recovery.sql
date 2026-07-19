PRAGMA foreign_keys = ON;

-- Approval state belongs to SQLite, not browser localStorage. These fields make
-- the local projection reconstructible after a page or client restart.
ALTER TABLE pending_app_requests ADD COLUMN project_id TEXT;
ALTER TABLE pending_app_requests ADD COLUMN thread_id TEXT;
ALTER TABLE pending_app_requests ADD COLUMN decision TEXT;
ALTER TABLE pending_app_requests ADD COLUMN error_message TEXT;

-- A bounded browser replay journal is independent from the server outbox: the
-- server ACK may delete an outbox event while a local browser is disconnected.
CREATE TABLE IF NOT EXISTS browser_event_journal (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_browser_event_cursor
    ON browser_event_journal(cursor);
