PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    login_name TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    password_changed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS web_sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at TEXT,
    user_agent_summary TEXT,
    ip_prefix TEXT
);
CREATE INDEX IF NOT EXISTS idx_web_sessions_token ON web_sessions(token_hash, expires_at);

CREATE TABLE IF NOT EXISTS pairing_codes (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at TEXT,
    cancelled_at TEXT
);

CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    public_key TEXT NOT NULL,
    key_version INTEGER NOT NULL DEFAULT 1,
    agent_version TEXT,
    codex_version TEXT,
    os_family TEXT,
    architecture TEXT,
    transport_security TEXT,
    history_completeness TEXT NOT NULL DEFAULT 'not_started',
    history_last_synced_at TEXT,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    last_seen_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_devices_user ON devices(user_id, status, last_seen_at);

CREATE TABLE IF NOT EXISTS device_auth_challenges (
    id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    nonce TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at TEXT
);

CREATE TABLE IF NOT EXISTS device_access_tokens (
    id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    key_version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_device_tokens_hash ON device_access_tokens(token_hash, expires_at);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    display_name TEXT NOT NULL,
    path_hint TEXT,
    status TEXT NOT NULL,
    repo_name TEXT,
    branch TEXT,
    is_dirty INTEGER,
    summary_version INTEGER NOT NULL DEFAULT 1,
    thread_count INTEGER NOT NULL DEFAULT 0,
    last_activity_at TEXT,
    removed_at TEXT,
    UNIQUE(device_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_system_unassigned
    ON projects(device_id, kind) WHERE kind = 'system_unassigned' AND removed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_projects_user_device ON projects(user_id, device_id, removed_at);

CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    app_server_thread_id TEXT,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    history_completeness TEXT NOT NULL DEFAULT 'not_started',
    history_cursor TEXT,
    history_revision INTEGER NOT NULL DEFAULT 0,
    last_synced_at TEXT,
    last_activity_at TEXT,
    summary_version INTEGER NOT NULL DEFAULT 1,
    UNIQUE(device_id, app_server_thread_id)
);
CREATE INDEX IF NOT EXISTS idx_threads_user_activity ON threads(user_id, last_activity_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_threads_project ON threads(project_id, archived, last_activity_at DESC);

CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    started_at TEXT,
    completed_at TEXT,
    terminal_reason TEXT,
    UNIQUE(thread_id, ordinal)
);

CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    revision INTEGER NOT NULL,
    content_hash TEXT,
    content_text TEXT,
    structured_detail TEXT,
    is_truncated INTEGER NOT NULL DEFAULT 0,
    occurred_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE(turn_id, ordinal)
);
CREATE INDEX IF NOT EXISTS idx_items_turn ON items(turn_id, ordinal);

CREATE TABLE IF NOT EXISTS commands (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    project_id TEXT,
    thread_id TEXT,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    server_sequence INTEGER NOT NULL,
    status TEXT NOT NULL,
    accepted_at TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    device_accepted_at TEXT,
    completed_at TEXT,
    result TEXT,
    error_code TEXT,
    UNIQUE(user_id, device_id, idempotency_key)
);
CREATE INDEX IF NOT EXISTS idx_commands_pending ON commands(device_id, status, server_sequence);

CREATE TABLE IF NOT EXISTS event_journal (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_event_journal_user_cursor ON event_journal(user_id, cursor);

CREATE TABLE IF NOT EXISTS history_sync_batches (
    batch_id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    thread_id TEXT NOT NULL,
    from_cursor TEXT,
    to_cursor TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    record_count INTEGER NOT NULL,
    received_at TEXT NOT NULL,
    committed_at TEXT NOT NULL,
    UNIQUE(device_id, thread_id, to_cursor)
);

CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    user_id TEXT,
    kind TEXT NOT NULL,
    subject_id TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

