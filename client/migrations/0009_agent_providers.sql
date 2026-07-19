ALTER TABLE threads ADD COLUMN provider TEXT NOT NULL DEFAULT 'codex'
    CHECK (provider IN ('codex', 'kimi'));

ALTER TABLE threads ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'full'
    CHECK (access_mode IN ('full', 'ask'));

ALTER TABLE pending_app_requests ADD COLUMN provider TEXT NOT NULL DEFAULT 'codex'
    CHECK (provider IN ('codex', 'kimi'));

ALTER TABLE removed_app_threads ADD COLUMN provider TEXT NOT NULL DEFAULT 'codex'
    CHECK (provider IN ('codex', 'kimi'));

CREATE INDEX idx_threads_provider_session
    ON threads(provider, app_server_thread_id);
