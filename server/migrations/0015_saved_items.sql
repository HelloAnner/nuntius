CREATE TABLE saved_items (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source_thread_id TEXT NOT NULL,
    source_item_id TEXT NOT NULL,
    content_markdown TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(user_id, idempotency_key)
);

CREATE UNIQUE INDEX idx_saved_items_source
    ON saved_items(user_id, source_item_id);

CREATE INDEX idx_saved_items_user_created
    ON saved_items(user_id, created_at DESC, id DESC);
