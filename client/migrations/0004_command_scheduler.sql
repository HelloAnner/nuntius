PRAGMA foreign_keys = OFF;

ALTER TABLE command_inbox RENAME TO command_inbox_legacy;

CREATE TABLE command_inbox (
    command_id TEXT PRIMARY KEY,
    queue_epoch TEXT NOT NULL,
    server_sequence INTEGER NOT NULL,
    target_key TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 2,
    payload TEXT NOT NULL,
    status TEXT NOT NULL,
    received_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    result TEXT,
    error_code TEXT,
    error_message TEXT,
    UNIQUE(queue_epoch, server_sequence)
);

INSERT INTO command_inbox(
    command_id, queue_epoch, server_sequence, target_key, priority, payload,
    status, received_at, started_at, completed_at, result, error_code
)
SELECT
    command_id, 'legacy', server_sequence, 'device', 2, payload,
    status, received_at, started_at, completed_at, result, error_code
FROM command_inbox_legacy;

DROP TABLE command_inbox_legacy;

CREATE INDEX idx_command_inbox_pending
    ON command_inbox(status, received_at, command_id);
CREATE INDEX idx_command_inbox_target
    ON command_inbox(target_key, status, received_at, command_id);

PRAGMA foreign_keys = ON;
