-- no-transaction
-- Migration 0007 added the provider CHECK after the original threads table
-- was created. SQLite cannot ALTER that column constraint in place, so widen
-- the one exact stored table definition without copying the large history DB.
PRAGMA writable_schema = ON;

UPDATE sqlite_schema
SET sql = replace(
    sql,
    'CHECK (provider IN (''codex'', ''kimi''))',
    'CHECK (provider IN (''codex'', ''kimi'', ''pi''))'
)
WHERE type = 'table'
  AND name = 'threads'
  AND instr(sql, 'CHECK (provider IN (''codex'', ''kimi''))') > 0;

PRAGMA writable_schema = OFF;

-- Force every pooled connection to invalidate its parsed schema cache.
CREATE TABLE server_provider_constraint_schema_refresh (id INTEGER PRIMARY KEY);
DROP TABLE server_provider_constraint_schema_refresh;
