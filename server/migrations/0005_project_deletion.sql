PRAGMA foreign_keys = ON;

-- Late history batches may race a project deletion. Remember the deleted local
-- thread ids so those batches are acknowledged without recreating the thread
-- under the server-owned "unassigned" project.
CREATE TABLE IF NOT EXISTS removed_project_threads (
    thread_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    removed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_removed_project_threads_project
    ON removed_project_threads(device_id, project_id);
