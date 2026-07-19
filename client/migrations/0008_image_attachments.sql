PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    original_name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    extension TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    local_path TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_client_attachments_thread ON attachments(thread_id, created_at);

CREATE TABLE IF NOT EXISTS item_attachments (
    item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY(item_id, attachment_id),
    UNIQUE(item_id, ordinal)
);
