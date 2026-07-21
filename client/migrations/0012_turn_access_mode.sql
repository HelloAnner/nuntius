ALTER TABLE turns ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'ask'
    CHECK (access_mode IN ('full', 'ask'));
