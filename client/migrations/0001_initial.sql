PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL DEFAULT 'workspace',
    display_name TEXT NOT NULL,
    canonical_path TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'active',
    defaults_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    app_server_thread_id TEXT UNIQUE,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'idle',
    archived INTEGER NOT NULL DEFAULT 0,
    last_activity_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_client_threads_project ON threads(project_id, archived, last_activity_at DESC);

CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    app_server_turn_id TEXT,
    ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    UNIQUE(thread_id, ordinal)
);

CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    app_server_item_id TEXT,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    content_text TEXT,
    structured_detail TEXT,
    occurred_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE(turn_id, ordinal)
);

CREATE TABLE IF NOT EXISTS command_inbox (
    command_id TEXT PRIMARY KEY,
    server_sequence INTEGER NOT NULL UNIQUE,
    payload TEXT NOT NULL,
    status TEXT NOT NULL,
    received_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    result TEXT,
    error_code TEXT
);
CREATE INDEX IF NOT EXISTS idx_command_inbox_pending ON command_inbox(status, server_sequence);

CREATE TABLE IF NOT EXISTS event_outbox (
    event_id TEXT PRIMARY KEY,
    stream_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    sent_at TEXT,
    UNIQUE(stream_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_event_outbox_created ON event_outbox(created_at);

CREATE TABLE IF NOT EXISTS history_outbox (
    batch_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    to_cursor TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    sent_at TEXT,
    UNIQUE(thread_id, to_cursor)
);

CREATE TABLE IF NOT EXISTS stream_sequences (
    stream_id TEXT PRIMARY KEY,
    next_sequence INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS directory_refs (
    id TEXT PRIMARY KEY,
    canonical_path TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_directory_refs_expiry ON directory_refs(expires_at);

CREATE TABLE IF NOT EXISTS pending_app_requests (
    approval_id TEXT PRIMARY KEY,
    app_request_id TEXT NOT NULL,
    method TEXT NOT NULL,
    params TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    decided_at TEXT
);

CREATE TABLE IF NOT EXISTS runtime_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
