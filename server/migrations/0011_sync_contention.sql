-- Keep retention scans off the hot writer path as the durable event journal grows.
CREATE INDEX IF NOT EXISTS idx_event_journal_created
    ON event_journal(created_at, cursor);
