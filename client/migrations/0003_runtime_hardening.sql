PRAGMA foreign_keys = ON;

CREATE INDEX IF NOT EXISTS idx_pending_app_requests_status
    ON pending_app_requests(status, created_at);

CREATE INDEX IF NOT EXISTS idx_history_outbox_created
    ON history_outbox(created_at, batch_id);
