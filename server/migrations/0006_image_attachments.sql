PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    original_name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    extension TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'ready',
    upload_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_referenced_at TEXT,
    UNIQUE(user_id, id),
    UNIQUE(user_id, thread_id, upload_key)
);
CREATE INDEX IF NOT EXISTS idx_attachments_thread ON attachments(thread_id, created_at);
CREATE INDEX IF NOT EXISTS idx_attachments_staged ON attachments(status, created_at);

CREATE TABLE IF NOT EXISTS item_attachments (
    item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY(item_id, attachment_id),
    UNIQUE(item_id, ordinal)
);

CREATE TABLE IF NOT EXISTS command_attachments (
    command_id TEXT NOT NULL REFERENCES commands(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY(command_id, attachment_id),
    UNIQUE(command_id, ordinal)
);
