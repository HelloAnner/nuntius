ALTER TABLE threads ADD COLUMN needs_review INTEGER NOT NULL DEFAULT 0;

UPDATE threads
SET needs_review = 1,
    status = 'idle'
WHERE status = 'needs_review';
