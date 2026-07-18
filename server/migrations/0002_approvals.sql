CREATE TABLE IF NOT EXISTS approvals (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    project_id TEXT,
    thread_id TEXT,
    method TEXT NOT NULL,
    params TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    requested_at TEXT NOT NULL,
    decided_at TEXT,
    decision TEXT,
    last_error TEXT
);
CREATE INDEX IF NOT EXISTS idx_approvals_user_status
    ON approvals(user_id, status, requested_at DESC);

ALTER TABLE devices ADD COLUMN active_turn_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN pending_approval_count INTEGER NOT NULL DEFAULT 0;
