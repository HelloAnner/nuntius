-- no-transaction
-- SQLite cannot ALTER an existing column CHECK constraint. The provider
-- columns were added in migration 0009, so widen only that exact constraint
-- in the three affected table definitions while preserving all rows, indexes
-- and foreign-key relationships.
PRAGMA writable_schema = ON;

UPDATE sqlite_schema
SET sql = replace(
    sql,
    'CHECK (provider IN (''codex'', ''kimi''))',
    'CHECK (provider IN (''codex'', ''kimi'', ''pi''))'
)
WHERE type = 'table'
  AND name IN ('threads', 'pending_app_requests', 'removed_app_threads')
  AND instr(sql, 'CHECK (provider IN (''codex'', ''kimi''))') > 0;

PRAGMA writable_schema = OFF;

-- Force every SQLite connection to invalidate its parsed schema cache after
-- the narrowly scoped sqlite_schema update above.
CREATE TABLE provider_constraint_schema_refresh (id INTEGER PRIMARY KEY);
DROP TABLE provider_constraint_schema_refresh;

PRAGMA integrity_check;
PRAGMA foreign_key_check;
