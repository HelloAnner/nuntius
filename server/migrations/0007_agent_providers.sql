ALTER TABLE threads ADD COLUMN provider TEXT NOT NULL DEFAULT 'codex'
    CHECK (provider IN ('codex', 'kimi'));

ALTER TABLE devices ADD COLUMN provider_statuses_json TEXT NOT NULL DEFAULT '[]';

CREATE INDEX idx_threads_device_provider_session
    ON threads(device_id, provider, app_server_thread_id);
