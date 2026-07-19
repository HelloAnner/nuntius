PRAGMA foreign_keys = ON;

-- Project rows and their Nuntius-owned thread/history records are physically
-- removed. These tombstones keep Codex's durable rollout scanner from
-- immediately importing the same project again. An explicit project creation
-- for the same canonical path clears both tables.
CREATE TABLE IF NOT EXISTS removed_project_paths (
    canonical_path TEXT PRIMARY KEY,
    project_id TEXT NOT NULL UNIQUE,
    removed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS removed_app_threads (
    app_server_thread_id TEXT PRIMARY KEY,
    canonical_path TEXT NOT NULL,
    removed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_removed_app_threads_path
    ON removed_app_threads(canonical_path);
