ALTER TABLE threads ADD COLUMN display_title_override TEXT;
ALTER TABLE threads ADD COLUMN title_revision INTEGER NOT NULL DEFAULT 0;
