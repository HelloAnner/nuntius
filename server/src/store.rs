use crate::{config::DATABASE_FILE, protocol::*};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{
    ConnectOptions, Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, str::FromStr, sync::Arc};
use time::{Duration, OffsetDateTime};

#[derive(Clone)]
pub struct ServerStore {
    pool: SqlitePool,
    queue_epoch: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: String,
    pub login_name: String,
    pub password_hash: String,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub user_id: String,
    pub login_name: String,
    pub csrf_token_hash: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct DeviceAuthRecord {
    pub device_id: String,
    pub user_id: String,
    pub public_key: String,
    pub key_version: i64,
}

#[derive(Debug, Clone)]
pub struct StoredCommand {
    pub queue_epoch: String,
    pub sequence: i64,
    pub command: DeviceCommand,
    pub status: CommandStatus,
    pub newly_created: bool,
}

impl ServerStore {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(DATABASE_FILE);
        let url = format!("sqlite://{}", path.to_string_lossy());
        let options = SqliteConnectOptions::from_str(&url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(30))
            .disable_statement_logging();
        // Migrations can rewrite sqlite_schema (for example when widening an
        // existing CHECK constraint). Never let the connection that performed
        // a migration enter the runtime pool with a stale parsed schema.
        let migration_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        sqlx::migrate!("./migrations").run(&migration_pool).await?;
        migration_pool.close().await;

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        let queue_epoch = match sqlx::query_scalar::<_, String>(
            "SELECT value FROM server_runtime_state WHERE key='command_queue_epoch'",
        )
        .fetch_optional(&pool)
        .await?
        {
            Some(value) => value,
            None => {
                let value = new_id("queue");
                sqlx::query("INSERT INTO server_runtime_state(key,value,updated_at) VALUES('command_queue_epoch',?,?)")
                    .bind(&value)
                    .bind(now())
                    .execute(&pool)
                    .await?;
                value
            }
        };
        sqlx::query("UPDATE commands SET queue_epoch=? WHERE queue_epoch='legacy'")
            .bind(&queue_epoch)
            .execute(&pool)
            .await?;
        crate::config::set_private_file_permissions(&path)?;
        for suffix in ["-wal", "-shm"] {
            let sidecar = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            if sidecar.exists() {
                crate::config::set_private_file_permissions(&sidecar)?;
            }
        }
        Ok(Self {
            pool,
            queue_epoch: queue_epoch.into(),
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn queue_epoch(&self) -> &str {
        &self.queue_epoch
    }

    pub async fn ready(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn maintenance(&self, event_retention_hours: i64) -> Result<()> {
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        let event_cutoff = (OffsetDateTime::now_utc() - Duration::hours(event_retention_hours))
            .format(&time::format_description::well_known::Rfc3339)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM web_sessions WHERE expires_at<=? OR revoked_at IS NOT NULL")
            .bind(now_unix)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM device_access_tokens WHERE expires_at<=? OR revoked_at IS NOT NULL",
        )
        .bind(now_unix)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM device_auth_challenges WHERE expires_at<=? OR consumed_at IS NOT NULL",
        )
        .bind(now_unix)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM pairing_codes WHERE expires_at<=? OR consumed_at IS NOT NULL OR cancelled_at IS NOT NULL")
            .bind(now_unix).execute(&mut *tx).await?;
        let expired = sqlx::query("UPDATE commands SET status='expired',completed_at=? WHERE status IN ('accepted','waiting_device') AND expires_at<=? RETURNING payload")
            .bind(now()).bind(now_unix).fetch_all(&mut *tx).await?;
        expire_approval_payloads(&mut tx, expired).await?;
        // Retention must not monopolize SQLite's single writer. The journal can
        // be very large after a disconnected device replays durable summaries;
        // delete one indexed page at a time on each maintenance tick.
        sqlx::query("DELETE FROM event_journal WHERE cursor IN (SELECT cursor FROM event_journal WHERE created_at<? ORDER BY created_at,cursor LIMIT 5000)")
            .bind(event_cutoff)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn backup(&self, destination: &Path) -> Result<()> {
        if destination.exists() {
            bail!(
                "backup destination already exists: {}",
                destination.display()
            );
        }
        let escaped = destination.to_string_lossy().replace('\'', "''");
        // `destination` is generated by the CLI inside the configured data
        // directory; single quotes are escaped before auditing the dynamic SQL.
        sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{escaped}'")))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn initialized(&self) -> Result<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE status = 'active'")
            .fetch_one(&self.pool)
            .await?;
        Ok(count > 0)
    }

    pub async fn create_owner(&self, login_name: &str, password_hash: &str) -> Result<UserRecord> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE status='active'")
            .fetch_one(&mut *tx)
            .await?;
        if count != 0 {
            bail!("server is already initialized");
        }
        let id = new_id("usr");
        let timestamp = now();
        sqlx::query("INSERT INTO users(id, login_name, password_hash, created_at, password_changed_at) VALUES(?,?,?,?,?)")
            .bind(&id).bind(login_name).bind(password_hash).bind(&timestamp).bind(&timestamp)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(UserRecord {
            id,
            login_name: login_name.into(),
            password_hash: password_hash.into(),
        })
    }

    pub async fn user_by_login(&self, login_name: &str) -> Result<Option<UserRecord>> {
        let row = sqlx::query("SELECT id, login_name, password_hash FROM users WHERE login_name = ? AND status = 'active'")
            .bind(login_name).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| UserRecord {
            id: r.get("id"),
            login_name: r.get("login_name"),
            password_hash: r.get("password_hash"),
        }))
    }

    pub async fn create_session(
        &self,
        user: &UserRecord,
        token_hash: &str,
        csrf_hash: &str,
        ttl_hours: i64,
        user_agent: Option<&str>,
    ) -> Result<(String, i64)> {
        let id = new_id("ses");
        let created = now();
        let expires_at = (OffsetDateTime::now_utc() + Duration::hours(ttl_hours)).unix_timestamp();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO web_sessions(id,user_id,token_hash,csrf_token_hash,created_at,last_seen_at,expires_at,user_agent_summary) VALUES(?,?,?,?,?,?,?,?)")
            .bind(&id).bind(&user.id).bind(token_hash).bind(csrf_hash).bind(&created).bind(&created).bind(expires_at).bind(user_agent)
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO web_csrf_tokens(session_id,token_hash,created_at) VALUES(?,?,?)")
            .bind(&id)
            .bind(csrf_hash)
            .bind(&created)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((id, expires_at))
    }

    pub async fn session_by_token_hash(&self, token_hash: &str) -> Result<Option<SessionRecord>> {
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        let row = sqlx::query("SELECT s.id,s.user_id,u.login_name,s.csrf_token_hash,s.expires_at FROM web_sessions s JOIN users u ON u.id=s.user_id WHERE s.token_hash=? AND s.revoked_at IS NULL AND s.expires_at>?")
            .bind(token_hash).bind(now_unix).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| SessionRecord {
            id: r.get("id"),
            user_id: r.get("user_id"),
            login_name: r.get("login_name"),
            csrf_token_hash: r.get("csrf_token_hash"),
            expires_at: r.get("expires_at"),
        }))
    }

    pub async fn touch_session(&self, session_id: &str) -> Result<()> {
        sqlx::query("UPDATE web_sessions SET last_seen_at=? WHERE id=?")
            .bind(now())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn rotate_csrf(&self, session_id: &str, csrf_hash: &str) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let stamp = now();
        sqlx::query("UPDATE web_sessions SET csrf_token_hash=?,last_seen_at=? WHERE id=?")
            .bind(csrf_hash)
            .bind(&stamp)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO web_csrf_tokens(session_id,token_hash,created_at) VALUES(?,?,?)",
        )
        .bind(session_id)
        .bind(csrf_hash)
        .bind(&stamp)
        .execute(&mut *tx)
        .await?;
        // A small bounded token set preserves multi-tab behavior without retaining
        // every token ever issued for a long-lived session.
        sqlx::query("DELETE FROM web_csrf_tokens WHERE session_id=? AND token_hash NOT IN (SELECT token_hash FROM web_csrf_tokens WHERE session_id=? ORDER BY created_at DESC LIMIT 16)")
            .bind(session_id).bind(session_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn csrf_token_valid(&self, session_id: &str, csrf_hash: &str) -> Result<bool> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM web_csrf_tokens WHERE session_id=? AND token_hash=?")
                .bind(session_id)
                .bind(csrf_hash)
                .fetch_optional(&self.pool)
                .await?;
        Ok(found.is_some())
    }

    pub async fn revoke_session(&self, session_id: &str) -> Result<()> {
        sqlx::query("UPDATE web_sessions SET revoked_at=? WHERE id=?")
            .bind(now())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_pairing_code(
        &self,
        user_id: &str,
        code_hash: &str,
        ttl_minutes: i64,
    ) -> Result<(String, i64)> {
        let id = new_id("pair");
        let expires_at =
            (OffsetDateTime::now_utc() + Duration::minutes(ttl_minutes)).unix_timestamp();
        sqlx::query("INSERT INTO pairing_codes(id,user_id,code_hash,created_at,expires_at) VALUES(?,?,?,?,?)")
            .bind(&id).bind(user_id).bind(code_hash).bind(now()).bind(expires_at).execute(&self.pool).await?;
        Ok((id, expires_at))
    }

    pub async fn pair_device(
        &self,
        request: &PairDeviceRequest,
        code_hash: &str,
    ) -> Result<String> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        let row = sqlx::query("SELECT id,user_id FROM pairing_codes WHERE code_hash=? AND consumed_at IS NULL AND cancelled_at IS NULL AND expires_at>?")
            .bind(code_hash).bind(now_unix).fetch_optional(&mut *tx).await?
            .ok_or_else(|| anyhow!("invalid or expired pairing code"))?;
        let pairing_id: String = row.get("id");
        let user_id: String = row.get("user_id");
        let consumed = sqlx::query("UPDATE pairing_codes SET consumed_at=? WHERE id=? AND consumed_at IS NULL AND cancelled_at IS NULL AND expires_at>?")
            .bind(now()).bind(&pairing_id).bind(now_unix).execute(&mut *tx).await?.rows_affected();
        if consumed != 1 {
            bail!("pairing code was already consumed");
        }
        let device_id = new_id("dev");
        sqlx::query("INSERT INTO devices(id,user_id,display_name,status,public_key,agent_version,os_family,architecture,created_at) VALUES(?,?,?,'active',?,?,?,?,?)")
            .bind(&device_id).bind(&user_id).bind(&request.display_name).bind(&request.public_key)
            .bind(&request.agent_version).bind(&request.os_family).bind(&request.architecture).bind(now())
            .execute(&mut *tx).await?;
        let project_id = new_id("prj");
        sqlx::query("INSERT INTO projects(id,user_id,device_id,kind,display_name,status) VALUES(?,?,?,'system_unassigned','未归类','system_unassigned')")
            .bind(&project_id).bind(&user_id).bind(&device_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(device_id)
    }

    pub async fn create_challenge(&self, device_id: &str, nonce: &str) -> Result<(String, i64)> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let active: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM devices WHERE id=? AND status='active'")
                .bind(device_id)
                .fetch_optional(&mut *tx)
                .await?;
        if active.is_none() {
            bail!("device is not active");
        }
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        sqlx::query("DELETE FROM device_auth_challenges WHERE device_id=? AND (expires_at<=? OR consumed_at IS NOT NULL)")
            .bind(device_id).bind(now_unix).execute(&mut *tx).await?;
        let pending: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM device_auth_challenges WHERE device_id=?")
                .bind(device_id)
                .fetch_one(&mut *tx)
                .await?;
        if pending >= 16 {
            bail!("too many outstanding device challenges");
        }
        let id = new_id("chal");
        let expires_at = (OffsetDateTime::now_utc() + Duration::minutes(2)).unix_timestamp();
        sqlx::query(
            "INSERT INTO device_auth_challenges(id,device_id,nonce,expires_at) VALUES(?,?,?,?)",
        )
        .bind(&id)
        .bind(device_id)
        .bind(nonce)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((id, expires_at))
    }

    pub async fn challenge_for_token(
        &self,
        challenge_id: &str,
        device_id: &str,
    ) -> Result<Option<(String, DeviceAuthRecord)>> {
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        let row = sqlx::query("SELECT c.nonce,d.id device_id,d.user_id,d.public_key,d.key_version FROM device_auth_challenges c JOIN devices d ON d.id=c.device_id WHERE c.id=? AND c.device_id=? AND c.consumed_at IS NULL AND c.expires_at>? AND d.status='active'")
            .bind(challenge_id).bind(device_id).bind(now_unix).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| {
            (
                r.get("nonce"),
                DeviceAuthRecord {
                    device_id: r.get("device_id"),
                    user_id: r.get("user_id"),
                    public_key: r.get("public_key"),
                    key_version: r.get("key_version"),
                },
            )
        }))
    }

    pub async fn consume_challenge_and_create_token(
        &self,
        challenge_id: &str,
        device: &DeviceAuthRecord,
        token_hash: &str,
        ttl_minutes: i64,
    ) -> Result<i64> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query("UPDATE device_auth_challenges SET consumed_at=? WHERE id=? AND device_id=? AND consumed_at IS NULL AND expires_at>?")
            .bind(now()).bind(challenge_id).bind(&device.device_id).bind(OffsetDateTime::now_utc().unix_timestamp()).execute(&mut *tx).await?.rows_affected();
        if updated != 1 {
            bail!("challenge already consumed");
        }
        let expires_at =
            (OffsetDateTime::now_utc() + Duration::minutes(ttl_minutes)).unix_timestamp();
        sqlx::query("INSERT INTO device_access_tokens(id,device_id,token_hash,key_version,created_at,expires_at) VALUES(?,?,?,?,?,?)")
            .bind(new_id("dtok")).bind(&device.device_id).bind(token_hash).bind(device.key_version).bind(now()).bind(expires_at)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(expires_at)
    }

    pub async fn device_by_access_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<DeviceAuthRecord>> {
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        let row = sqlx::query("SELECT d.id device_id,d.user_id,d.public_key,d.key_version FROM device_access_tokens t JOIN devices d ON d.id=t.device_id WHERE t.token_hash=? AND t.revoked_at IS NULL AND t.expires_at>? AND d.status='active' AND t.key_version=d.key_version")
            .bind(token_hash).bind(now_unix).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| DeviceAuthRecord {
            device_id: r.get("device_id"),
            user_id: r.get("user_id"),
            public_key: r.get("public_key"),
            key_version: r.get("key_version"),
        }))
    }

    pub async fn user_id_for_device(&self, device_id: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT user_id FROM devices WHERE id=? AND status='active'")
                .bind(device_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn list_devices(&self, user_id: &str) -> Result<Vec<DeviceSummary>> {
        let rows = sqlx::query("SELECT d.*, (SELECT COUNT(*) FROM projects p WHERE p.device_id=d.id AND p.removed_at IS NULL AND p.kind='workspace') project_count FROM devices d WHERE d.user_id=? ORDER BY COALESCE(d.last_seen_at,d.created_at) DESC")
            .bind(user_id).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| DeviceSummary {
                id: r.get("id"),
                display_name: r.get("display_name"),
                status: if r.get::<String, _>("status") == "revoked" {
                    DeviceStatus::Revoked
                } else {
                    DeviceStatus::Offline
                },
                last_seen_at: r.get("last_seen_at"),
                agent_version: r.get("agent_version"),
                codex_version: r.get("codex_version"),
                os_family: r.get("os_family"),
                architecture: r.get("architecture"),
                project_count: r.get("project_count"),
                active_turn_count: r.get("active_turn_count"),
                pending_approval_count: r.get("pending_approval_count"),
                history_completeness: parse_completeness(
                    &r.get::<String, _>("history_completeness"),
                ),
                history_last_synced_at: r.get("history_last_synced_at"),
                transport_security: r
                    .get::<Option<String>, _>("transport_security")
                    .as_deref()
                    .map(parse_transport),
                app_server_status: r.get("app_server_status"),
                storage_status: r.get("storage_status"),
                inbox_depth: r.get("inbox_depth"),
                outbox_depth: r.get("outbox_depth"),
                history_backfill_depth: r.get("history_backfill_depth"),
                providers: serde_json::from_str(&r.get::<String, _>("provider_statuses_json"))
                    .unwrap_or_default(),
            })
            .collect())
    }

    pub async fn mark_device_seen(
        &self,
        device_id: &str,
        agent_version: &str,
        security: TransportSecurity,
        health: Option<&DeviceHealth>,
    ) -> Result<()> {
        let codex = health.and_then(|h| h.codex_version.as_deref());
        let providers = health
            .map(|health| serde_json::to_string(&health.providers))
            .transpose()?;
        sqlx::query("UPDATE devices SET last_seen_at=?,agent_version=?,codex_version=COALESCE(?,codex_version),transport_security=?,active_turn_count=COALESCE(?,active_turn_count),pending_approval_count=COALESCE(?,pending_approval_count),app_server_status=COALESCE(?,app_server_status),storage_status=COALESCE(?,storage_status),inbox_depth=COALESCE(?,inbox_depth),outbox_depth=COALESCE(?,outbox_depth),history_backfill_depth=COALESCE(?,history_backfill_depth),provider_statuses_json=COALESCE(?,provider_statuses_json) WHERE id=? AND status='active'")
            .bind(now()).bind(agent_version).bind(codex).bind(transport_string(security)).bind(health.map(|value|value.active_turn_count)).bind(health.map(|value|value.pending_approval_count))
            .bind(health.map(|value|value.app_server_status.as_str())).bind(health.map(|value|value.storage_status.as_str()))
            .bind(health.map(|value|value.inbox_depth)).bind(health.map(|value|value.outbox_depth)).bind(health.map(|value|value.history_backfill_depth))
            .bind(providers)
            .bind(device_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn device_is_active_for_user(&self, user_id: &str, device_id: &str) -> Result<bool> {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM devices WHERE id=? AND user_id=? AND status='active'",
        )
        .bind(device_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(found.is_some())
    }

    pub async fn device_display_name(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT display_name FROM devices WHERE id=? AND user_id=? AND status='active'",
        )
        .bind(device_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn rename_device(
        &self,
        user_id: &str,
        device_id: &str,
        display_name: &str,
    ) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE devices SET display_name=? WHERE id=? AND user_id=? AND status='active'",
        )
        .bind(display_name)
        .bind(device_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn append_audit(
        &self,
        user_id: Option<&str>,
        kind: &str,
        subject_id: Option<&str>,
        metadata: &Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO audit_events(id,user_id,kind,subject_id,metadata,created_at) VALUES(?,?,?,?,?,?)")
            .bind(new_id("audit")).bind(user_id).bind(kind).bind(subject_id)
            .bind(serde_json::to_string(metadata)?).bind(now()).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn mark_history_inventory_complete(&self, device_id: &str) -> Result<()> {
        sqlx::query("UPDATE devices SET history_completeness='complete',history_last_synced_at=? WHERE id=? AND status='active'")
            .bind(now()).bind(device_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn revoke_device(&self, user_id: &str, device_id: &str) -> Result<bool> {
        let timestamp = now();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query("UPDATE devices SET status='revoked',revoked_at=?,key_version=key_version+1 WHERE id=? AND user_id=? AND status='active'")
            .bind(&timestamp).bind(device_id).bind(user_id).execute(&mut *tx).await?.rows_affected();
        sqlx::query(
            "UPDATE device_access_tokens SET revoked_at=? WHERE device_id=? AND revoked_at IS NULL",
        )
        .bind(&timestamp)
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(changed == 1)
    }

    pub async fn list_projects(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Vec<ProjectSummary>> {
        let rows = sqlx::query("SELECT p.*,(SELECT COUNT(*) FROM threads t WHERE t.project_id=p.id AND t.archived=0) thread_count FROM projects p WHERE p.user_id=? AND p.device_id=? AND p.removed_at IS NULL ORDER BY p.kind='system_unassigned' DESC,julianday(p.last_activity_at) DESC,p.display_name")
            .bind(user_id).bind(device_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(project_from_row).collect())
    }

    pub async fn remove_project(
        &self,
        user_id: &str,
        device_id: &str,
        project_id: &str,
    ) -> Result<bool> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let project = sqlx::query(
            "SELECT kind,removed_at FROM projects WHERE id=? AND device_id=? AND user_id=?",
        )
        .bind(project_id)
        .bind(device_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(project) = project else {
            return Ok(false);
        };
        if project.get::<String, _>("kind") != "workspace" {
            bail!("system project cannot be removed");
        }
        if project.get::<Option<String>, _>("removed_at").is_some() {
            return Ok(false);
        }
        let changed = remove_project_records(&mut tx, device_id, project_id).await?;
        tx.commit().await?;
        Ok(changed)
    }

    pub async fn list_threads(
        &self,
        user_id: &str,
        device_id: Option<&str>,
        project_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ThreadSummary>> {
        let rows = match (device_id, project_id) {
            (_, Some(project)) => sqlx::query("SELECT * FROM threads WHERE user_id=? AND project_id=? AND archived=0 ORDER BY julianday(COALESCE(created_at,last_activity_at)) DESC,id DESC LIMIT ? OFFSET ?")
                .bind(user_id).bind(project).bind(limit).bind(offset).fetch_all(&self.pool).await?,
            (Some(device), None) => sqlx::query("SELECT * FROM threads WHERE user_id=? AND device_id=? AND archived=0 ORDER BY julianday(COALESCE(created_at,last_activity_at)) DESC,id DESC LIMIT ? OFFSET ?")
                .bind(user_id).bind(device).bind(limit).bind(offset).fetch_all(&self.pool).await?,
            (None, None) => sqlx::query("SELECT * FROM threads WHERE user_id=? AND archived=0 ORDER BY julianday(COALESCE(created_at,last_activity_at)) DESC,id DESC LIMIT ? OFFSET ?")
                .bind(user_id).bind(limit).bind(offset).fetch_all(&self.pool).await?,
        };
        Ok(rows.into_iter().map(thread_from_row).collect())
    }

    pub async fn upsert_created_thread(&self, user_id: &str, thread: &ThreadSummary) -> Result<()> {
        let project_exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM projects WHERE id=? AND device_id=? AND user_id=? AND removed_at IS NULL",
        )
        .bind(&thread.project_id)
        .bind(&thread.device_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        if project_exists.is_none() {
            bail!("created thread references an unavailable project")
        }
        sqlx::query("INSERT INTO threads(id,user_id,device_id,project_id,provider,app_server_thread_id,title,status,archived,history_completeness,created_at,last_synced_at,last_activity_at,history_revision) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,0) ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id,provider=excluded.provider,app_server_thread_id=COALESCE(excluded.app_server_thread_id,threads.app_server_thread_id),title=excluded.title,status=excluded.status,archived=excluded.archived,created_at=COALESCE(excluded.created_at,threads.created_at),last_activity_at=COALESCE(excluded.last_activity_at,threads.last_activity_at) WHERE threads.user_id=excluded.user_id AND threads.device_id=excluded.device_id")
            .bind(&thread.id)
            .bind(user_id)
            .bind(&thread.device_id)
            .bind(&thread.project_id)
            .bind(thread.provider.as_str())
            .bind(&thread.app_server_thread_id)
            .bind(&thread.title)
            .bind(&thread.status)
            .bind(thread.archived)
            .bind("backfilling")
            .bind(&thread.created_at)
            .bind(&thread.last_synced_at)
            .bind(&thread.last_activity_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn thread_belongs_to_user(
        &self,
        user_id: &str,
        thread_id: &str,
    ) -> Result<Option<(String, String)>> {
        let row = sqlx::query("SELECT device_id,project_id FROM threads WHERE id=? AND user_id=?")
            .bind(thread_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("device_id"), r.get("project_id"))))
    }

    pub async fn thread_command_target(
        &self,
        user_id: &str,
        thread_id: &str,
    ) -> Result<Option<(String, String)>> {
        let row = sqlx::query("SELECT h.device_id,h.project_id FROM threads h JOIN devices d ON d.id=h.device_id JOIN projects p ON p.id=h.project_id WHERE h.id=? AND h.user_id=? AND h.archived=0 AND d.status='active' AND p.removed_at IS NULL AND p.kind='workspace' AND p.status='active'")
            .bind(thread_id).bind(user_id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("device_id"), r.get("project_id"))))
    }

    pub async fn insert_attachment(
        &self,
        id: &str,
        user_id: &str,
        device_id: &str,
        thread_id: &str,
        upload_key: &str,
        original_name: &str,
        mime_type: &str,
        extension: &str,
        byte_size: i64,
        sha256: &str,
        width: u32,
        height: u32,
    ) -> Result<AttachmentView> {
        sqlx::query("INSERT INTO attachments(id,user_id,device_id,thread_id,original_name,mime_type,extension,byte_size,sha256,width,height,status,upload_key,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,'ready',?,?)")
            .bind(id).bind(user_id).bind(device_id).bind(thread_id).bind(original_name)
            .bind(mime_type).bind(extension).bind(byte_size).bind(sha256)
            .bind(i64::from(width)).bind(i64::from(height)).bind(upload_key).bind(now())
            .execute(&self.pool).await?;
        Ok(AttachmentView {
            id: id.into(),
            original_name: original_name.into(),
            mime_type: mime_type.into(),
            byte_size,
            sha256: sha256.into(),
            width,
            height,
        })
    }

    pub async fn attachment_by_upload_key(
        &self,
        user_id: &str,
        thread_id: &str,
        upload_key: &str,
    ) -> Result<Option<AttachmentView>> {
        let row = sqlx::query("SELECT * FROM attachments WHERE user_id=? AND thread_id=? AND upload_key=? AND status<>'deleted'")
            .bind(user_id).bind(thread_id).bind(upload_key).fetch_optional(&self.pool).await?;
        Ok(row.map(|row| attachment_view_from_row(&row)))
    }

    pub async fn attachment_ref_for_user(
        &self,
        user_id: &str,
        attachment_id: &str,
    ) -> Result<Option<AttachmentRef>> {
        let row =
            sqlx::query("SELECT * FROM attachments WHERE id=? AND user_id=? AND status<>'deleted'")
                .bind(attachment_id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|row| attachment_ref_from_row(&row)))
    }

    pub async fn attachment_ref_for_device(
        &self,
        user_id: &str,
        device_id: &str,
        attachment_id: &str,
    ) -> Result<Option<AttachmentRef>> {
        let row = sqlx::query("SELECT * FROM attachments WHERE id=? AND user_id=? AND device_id=? AND status<>'deleted'")
            .bind(attachment_id).bind(user_id).bind(device_id).fetch_optional(&self.pool).await?;
        Ok(row.map(|row| attachment_ref_from_row(&row)))
    }

    pub async fn resolve_message_attachments(
        &self,
        user_id: &str,
        thread_id: &str,
        attachment_ids: &[String],
    ) -> Result<Vec<AttachmentRef>> {
        if attachment_ids.len() > crate::attachments::MAX_IMAGES_PER_MESSAGE {
            bail!(
                "a message may contain at most {} images",
                crate::attachments::MAX_IMAGES_PER_MESSAGE
            );
        }
        let mut unique = std::collections::HashSet::new();
        let mut attachments = Vec::with_capacity(attachment_ids.len());
        for id in attachment_ids {
            if !unique.insert(id.as_str()) {
                bail!("attachment ids must be unique");
            }
            let row = sqlx::query("SELECT * FROM attachments WHERE id=? AND user_id=? AND thread_id=? AND status='ready'")
                .bind(id).bind(user_id).bind(thread_id).fetch_optional(&self.pool).await?
                .context("attachment is unavailable for this thread")?;
            attachments.push(attachment_ref_from_row(&row));
        }
        Ok(attachments)
    }

    pub async fn delete_unreferenced_attachment(
        &self,
        user_id: &str,
        attachment_id: &str,
    ) -> Result<Option<AttachmentRef>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT * FROM attachments a WHERE a.id=? AND a.user_id=? AND a.status='ready' AND NOT EXISTS(SELECT 1 FROM command_attachments c WHERE c.attachment_id=a.id) AND NOT EXISTS(SELECT 1 FROM item_attachments i WHERE i.attachment_id=a.id)")
            .bind(attachment_id).bind(user_id).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let attachment = attachment_ref_from_row(&row);
        sqlx::query("DELETE FROM attachments WHERE id=? AND user_id=?")
            .bind(attachment_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(attachment))
    }

    pub async fn project_belongs_to_user(
        &self,
        user_id: &str,
        device_id: &str,
        project_id: &str,
    ) -> Result<bool> {
        let one: Option<i64> = sqlx::query_scalar("SELECT 1 FROM projects WHERE id=? AND device_id=? AND user_id=? AND removed_at IS NULL")
            .bind(project_id).bind(device_id).bind(user_id).fetch_optional(&self.pool).await?;
        Ok(one.is_some())
    }

    pub async fn project_accepts_commands(
        &self,
        user_id: &str,
        device_id: &str,
        project_id: &str,
    ) -> Result<bool> {
        let one: Option<i64> = sqlx::query_scalar("SELECT 1 FROM projects p JOIN devices d ON d.id=p.device_id WHERE p.id=? AND p.device_id=? AND p.user_id=? AND p.removed_at IS NULL AND p.kind='workspace' AND p.status='active' AND d.status='active'")
            .bind(project_id).bind(device_id).bind(user_id).fetch_optional(&self.pool).await?;
        Ok(one.is_some())
    }

    pub async fn history_turns(
        &self,
        user_id: &str,
        thread_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<HistoryTurnView>> {
        let rows = sqlx::query("SELECT t.* FROM turns t JOIN threads h ON h.id=t.thread_id WHERE h.user_id=? AND t.thread_id=? ORDER BY COALESCE(julianday(t.started_at),julianday(t.completed_at)),t.ordinal,t.id LIMIT ? OFFSET ?")
            .bind(user_id).bind(thread_id).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| HistoryTurnView {
                id: r.get("id"),
                thread_id: r.get("thread_id"),
                ordinal: r.get("ordinal"),
                status: r.get("status"),
                started_at: r.get("started_at"),
                completed_at: r.get("completed_at"),
            })
            .collect())
    }

    pub async fn history_items(
        &self,
        user_id: &str,
        turn_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<HistoryItemView>> {
        let rows = sqlx::query("SELECT i.* FROM items i JOIN turns t ON t.id=i.turn_id JOIN threads h ON h.id=t.thread_id WHERE h.user_id=? AND i.turn_id=? ORDER BY julianday(i.occurred_at),i.ordinal,i.id LIMIT ? OFFSET ?")
            .bind(user_id).bind(turn_id).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let mut item = item_from_row(row);
            item.attachments = self.item_attachments(&item.id).await?;
            items.push(item);
        }
        Ok(items)
    }

    async fn item_attachments(&self, item_id: &str) -> Result<Vec<AttachmentView>> {
        let rows = sqlx::query("SELECT a.* FROM item_attachments ia JOIN attachments a ON a.id=ia.attachment_id WHERE ia.item_id=? ORDER BY ia.ordinal")
            .bind(item_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(attachment_view_from_row).collect())
    }

    pub async fn insert_command(
        &self,
        user_id: &str,
        idempotency_key: &str,
        fingerprint: &str,
        command: &DeviceCommand,
        expires_at_unix: i64,
    ) -> Result<StoredCommand> {
        let payload = serde_json::to_string(command)?;
        let kind = command.command.name();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(row) = sqlx::query("SELECT queue_epoch,server_sequence,payload,status,request_fingerprint FROM commands WHERE user_id=? AND device_id=? AND idempotency_key=?")
            .bind(user_id).bind(&command.device_id).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            let existing_fingerprint: String = row.get("request_fingerprint");
            if existing_fingerprint != fingerprint { bail!("idempotency key reused with different request"); }
            let stored: DeviceCommand = serde_json::from_str(&row.get::<String,_>("payload"))?;
            return Ok(StoredCommand { queue_epoch: row.get("queue_epoch"), sequence: row.get("server_sequence"), command: stored, status: parse_command_status(&row.get::<String,_>("status")), newly_created: false });
        }
        if let DeviceCommandKind::ApprovalDecide { approval_id, .. } = &command.command {
            let claimed = sqlx::query("UPDATE approvals SET status='responding' WHERE id=? AND user_id=? AND device_id=? AND status='pending'")
                .bind(approval_id).bind(user_id).bind(&command.device_id).execute(&mut *tx).await?.rows_affected();
            if claimed != 1 {
                bail!("approval is no longer pending");
            }
        }
        let sequence: i64 = sqlx::query_scalar("INSERT INTO device_command_sequences(device_id,next_sequence) VALUES(?,2) ON CONFLICT(device_id) DO UPDATE SET next_sequence=device_command_sequences.next_sequence+1 RETURNING next_sequence-1")
            .bind(&command.device_id).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO commands(id,user_id,device_id,project_id,thread_id,kind,payload,idempotency_key,request_fingerprint,queue_epoch,server_sequence,status,accepted_at,expires_at) VALUES(?,?,?,?,?,?,?,?,?,?,?, 'accepted',?,?)")
            .bind(&command.command_id).bind(user_id).bind(&command.device_id).bind(&command.project_id).bind(&command.thread_id)
            .bind(kind).bind(payload).bind(idempotency_key).bind(fingerprint).bind(self.queue_epoch()).bind(sequence).bind(&command.issued_at).bind(expires_at_unix)
            .execute(&mut *tx).await?;
        let attachments: &[AttachmentRef] = match &command.command {
            DeviceCommandKind::TurnStart { attachments, .. }
            | DeviceCommandKind::TurnSteer { attachments, .. } => attachments,
            _ => &[],
        };
        for (index, attachment) in attachments.iter().enumerate() {
            sqlx::query(
                "INSERT INTO command_attachments(command_id,attachment_id,ordinal) VALUES(?,?,?)",
            )
            .bind(&command.command_id)
            .bind(&attachment.id)
            .bind(index as i64 + 1)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE attachments SET last_referenced_at=? WHERE id=?")
                .bind(now())
                .bind(&attachment.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(StoredCommand {
            queue_epoch: self.queue_epoch().into(),
            sequence,
            command: command.clone(),
            status: CommandStatus::Accepted,
            newly_created: true,
        })
    }

    pub async fn command_by_idempotency(
        &self,
        user_id: &str,
        device_id: &str,
        idempotency_key: &str,
        fingerprint: &str,
    ) -> Result<Option<StoredCommand>> {
        let row = sqlx::query("SELECT queue_epoch,server_sequence,payload,status,request_fingerprint FROM commands WHERE user_id=? AND device_id=? AND idempotency_key=?")
            .bind(user_id).bind(device_id).bind(idempotency_key).fetch_optional(&self.pool).await?;
        row.map(|row| {
            if row.get::<String, _>("request_fingerprint") != fingerprint {
                bail!("idempotency key reused with different request");
            }
            Ok(StoredCommand {
                queue_epoch: row.get("queue_epoch"),
                sequence: row.get("server_sequence"),
                command: serde_json::from_str(&row.get::<String, _>("payload"))?,
                status: parse_command_status(&row.get::<String, _>("status")),
                newly_created: false,
            })
        })
        .transpose()
    }

    pub async fn pending_commands(
        &self,
        device_id: &str,
        _after_sequence: i64,
        limit: i64,
    ) -> Result<Vec<StoredCommand>> {
        let now_unix = OffsetDateTime::now_utc().unix_timestamp();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let expired = sqlx::query("UPDATE commands SET status='expired',completed_at=? WHERE device_id=? AND status IN ('accepted','waiting_device') AND expires_at<=? RETURNING payload")
            .bind(now()).bind(device_id).bind(now_unix).fetch_all(&mut *tx).await?;
        expire_approval_payloads(&mut tx, expired).await?;
        // Replay every non-terminal command. A maximum sequence cannot represent holes when
        // commands complete out of order.
        let rows = sqlx::query("SELECT queue_epoch,server_sequence,payload,status FROM commands WHERE device_id=? AND status IN ('accepted','waiting_device','device_accepted','applying') ORDER BY server_sequence LIMIT ?")
            .bind(device_id).bind(limit).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        rows.into_iter()
            .map(|r| {
                Ok(StoredCommand {
                    queue_epoch: r.get("queue_epoch"),
                    sequence: r.get("server_sequence"),
                    command: serde_json::from_str(&r.get::<String, _>("payload"))?,
                    status: parse_command_status(&r.get::<String, _>("status")),
                    newly_created: false,
                })
            })
            .collect()
    }

    pub async fn mark_command_waiting(&self, command_id: &str) -> Result<()> {
        sqlx::query("UPDATE commands SET status='waiting_device' WHERE id=? AND status='accepted'")
            .bind(command_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_command_ack(
        &self,
        device_id: &str,
        command_id: &str,
        stage: &str,
        result: Option<&Value>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<CommandStatus> {
        let incoming = match stage {
            "persisted" => CommandStatus::DeviceAccepted,
            "applying" => CommandStatus::Applying,
            "completed" => CommandStatus::Completed,
            "failed" => CommandStatus::Failed,
            "rejected" => CommandStatus::Rejected,
            "unknown" => CommandStatus::Unknown,
            "expired" => CommandStatus::Expired,
            _ => bail!("invalid command ACK stage"),
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT status,payload FROM commands WHERE id=? AND device_id=?")
            .bind(command_id)
            .bind(device_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("command ACK target not found"))?;
        let current = parse_command_status(&row.get::<String, _>("status"));
        if command_status_terminal(current) {
            return Ok(current);
        }
        let transition_allowed = match current {
            CommandStatus::Accepted | CommandStatus::WaitingDevice => true,
            CommandStatus::DeviceAccepted => !matches!(
                incoming,
                CommandStatus::Accepted | CommandStatus::WaitingDevice
            ),
            CommandStatus::Applying => {
                command_status_terminal(incoming) || incoming == CommandStatus::Applying
            }
            _ => false,
        };
        if !transition_allowed {
            return Ok(current);
        }
        let terminal = matches!(
            incoming,
            CommandStatus::Completed
                | CommandStatus::Failed
                | CommandStatus::Rejected
                | CommandStatus::Unknown
                | CommandStatus::Expired
        );
        let result_json = result.map(serde_json::to_string).transpose()?;
        sqlx::query("UPDATE commands SET status=?,device_accepted_at=CASE WHEN ?='device_accepted' THEN COALESCE(device_accepted_at,?) ELSE device_accepted_at END,completed_at=CASE WHEN ? THEN ? ELSE completed_at END,result=COALESCE(?,result),error_code=COALESCE(?,error_code),error_message=COALESCE(?,error_message) WHERE id=? AND device_id=?")
            .bind(incoming.as_str()).bind(incoming.as_str()).bind(now()).bind(terminal).bind(now()).bind(result_json).bind(error_code).bind(error_message).bind(command_id).bind(device_id)
            .execute(&mut *tx).await?;
        let device_command =
            serde_json::from_str::<DeviceCommand>(&row.get::<String, _>("payload"))?;
        if terminal {
            match device_command.command {
                DeviceCommandKind::ApprovalDecide {
                    approval_id,
                    request,
                } => {
                    if incoming == CommandStatus::Completed {
                        sqlx::query("UPDATE approvals SET status='decided',decided_at=?,decision=?,last_error=NULL WHERE id=? AND device_id=?")
                            .bind(now()).bind(request.decision).bind(approval_id).bind(device_id).execute(&mut *tx).await?;
                    } else {
                        let approval_status = match incoming {
                            CommandStatus::Unknown => "unknown",
                            CommandStatus::Expired => "expired",
                            _ => "failed",
                        };
                        sqlx::query("UPDATE approvals SET status=?,decided_at=?,last_error=? WHERE id=? AND device_id=?")
                            .bind(approval_status).bind(now()).bind(error_code.unwrap_or("decision_failed"))
                            .bind(approval_id).bind(device_id).execute(&mut *tx).await?;
                    }
                }
                DeviceCommandKind::ProjectDelete { project_id }
                    if incoming == CommandStatus::Completed =>
                {
                    remove_project_records(&mut tx, device_id, &project_id).await?;
                }
                _ => {}
            }
        }
        tx.commit().await?;
        Ok(incoming)
    }

    pub async fn command_view(
        &self,
        user_id: &str,
        command_id: &str,
    ) -> Result<Option<CommandView>> {
        let row = sqlx::query("SELECT id,device_id,status,kind,accepted_at,completed_at,error_code,error_message,result FROM commands WHERE id=? AND user_id=?")
            .bind(command_id).bind(user_id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| CommandView {
            id: r.get("id"),
            device_id: r.get("device_id"),
            status: parse_command_status(&r.get::<String, _>("status")),
            kind: r.get("kind"),
            accepted_at: r.get("accepted_at"),
            completed_at: r.get("completed_at"),
            error_code: r.get("error_code"),
            error_message: r.get("error_message"),
            result: r
                .get::<Option<String>, _>("result")
                .and_then(|s| serde_json::from_str(&s).ok()),
        }))
    }

    pub async fn upsert_approval_event(&self, user_id: &str, event: &NuntiusEvent) -> Result<()> {
        let approval_id = event
            .payload
            .get("approvalId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("approval event has no approvalId"))?;
        let method = event
            .payload
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let params = event.payload.get("params").cloned().unwrap_or(Value::Null);
        sqlx::query("INSERT INTO approvals(id,user_id,device_id,project_id,thread_id,method,params,status,requested_at) VALUES(?,?,?,?,?,?,?,'pending',?) ON CONFLICT(id) DO UPDATE SET method=excluded.method,params=excluded.params")
            .bind(approval_id).bind(user_id).bind(&event.device_id).bind(&event.project_id).bind(&event.thread_id).bind(method).bind(serde_json::to_string(&params)?).bind(&event.occurred_at).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn expire_thread_approvals(
        &self,
        user_id: &str,
        device_id: &str,
        thread_id: &str,
        decided_at: &str,
    ) -> Result<u64> {
        Ok(sqlx::query("UPDATE approvals SET status='expired',decided_at=COALESCE(decided_at,?),decision=COALESCE(decision,'cancel'),last_error=COALESCE(last_error,'provider_turn_terminal') WHERE user_id=? AND device_id=? AND thread_id=? AND status IN ('pending','responding')")
            .bind(decided_at)
            .bind(user_id)
            .bind(device_id)
            .bind(thread_id)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn resolve_approval_event(&self, user_id: &str, event: &NuntiusEvent) -> Result<u64> {
        let approval_id = event
            .payload
            .get("approvalId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("approval resolution event has no approvalId"))?;
        let status = event
            .payload
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("approval resolution event has no status"))?;
        if !matches!(status, "decided" | "unknown" | "expired" | "failed") {
            bail!("invalid approval resolution status")
        }
        let decision = event.payload.get("decision").and_then(Value::as_str);
        let last_error = if status == "decided" {
            None
        } else {
            Some("device_approval_resolution")
        };
        Ok(sqlx::query("UPDATE approvals SET status=?,decided_at=COALESCE(decided_at,?),decision=COALESCE(?,decision),last_error=? WHERE id=? AND user_id=? AND device_id=? AND status IN ('pending','responding')")
            .bind(status)
            .bind(&event.occurred_at)
            .bind(decision)
            .bind(last_error)
            .bind(approval_id)
            .bind(user_id)
            .bind(&event.device_id)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn list_approvals(
        &self,
        user_id: &str,
        pending_only: bool,
    ) -> Result<Vec<ApprovalView>> {
        let rows = if pending_only {
            sqlx::query("SELECT * FROM approvals WHERE user_id=? AND status='pending' ORDER BY requested_at DESC").bind(user_id).fetch_all(&self.pool).await?
        } else {
            sqlx::query(
                "SELECT * FROM approvals WHERE user_id=? ORDER BY requested_at DESC LIMIT 500",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?
        };
        rows.into_iter()
            .map(|row| {
                Ok(ApprovalView {
                    id: row.get("id"),
                    device_id: row.get("device_id"),
                    project_id: row.get("project_id"),
                    thread_id: row.get("thread_id"),
                    method: row.get("method"),
                    params: serde_json::from_str(&row.get::<String, _>("params"))?,
                    status: row.get("status"),
                    requested_at: row.get("requested_at"),
                    decided_at: row.get("decided_at"),
                    decision: row.get("decision"),
                })
            })
            .collect()
    }

    pub async fn approval_device(
        &self,
        user_id: &str,
        approval_id: &str,
    ) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT device_id FROM approvals WHERE id=? AND user_id=? AND status IN ('pending','responding')",
        )
        .bind(approval_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn append_event(&self, user_id: &str, event: &NuntiusEvent) -> Result<i64> {
        let payload = serde_json::to_string(event)?;
        let result = sqlx::query("INSERT OR IGNORE INTO event_journal(event_id,user_id,device_id,event_type,payload,created_at) VALUES(?,?,?,?,?,?)")
            .bind(&event.event_id).bind(user_id).bind(&event.device_id).bind(&event.event_type).bind(&payload).bind(now()).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            let row = sqlx::query(
                "SELECT cursor,user_id,device_id,payload FROM event_journal WHERE event_id=?",
            )
            .bind(&event.event_id)
            .fetch_one(&self.pool)
            .await?;
            if row.get::<String, _>("user_id") != user_id
                || row.get::<String, _>("device_id") != event.device_id
                || row.get::<String, _>("payload") != payload
            {
                bail!("event id reused with different identity or payload");
            }
            return Ok(row.get("cursor"));
        }
        Ok(result.last_insert_rowid())
    }

    pub async fn replay_events(
        &self,
        user_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Vec<(i64, NuntiusEvent)>> {
        let rows = sqlx::query("SELECT cursor,payload FROM event_journal WHERE user_id=? AND cursor>? ORDER BY cursor LIMIT ?")
            .bind(user_id).bind(after).bind(limit).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.get("cursor"),
                    serde_json::from_str(&r.get::<String, _>("payload"))?,
                ))
            })
            .collect()
    }

    pub async fn event_bounds(&self, user_id: &str) -> Result<(Option<i64>, Option<i64>)> {
        let row = sqlx::query(
            "SELECT MIN(cursor) minimum,MAX(cursor) maximum FROM event_journal WHERE user_id=?",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.get("minimum"), row.get("maximum")))
    }

    pub async fn ingest_history_batch(
        &self,
        user_id: &str,
        batch: &HistoryBatch,
    ) -> Result<String> {
        if batch.batch_id.is_empty()
            || batch.batch_id.len() > 128
            || batch.thread_id.is_empty()
            || batch.thread_id.len() > 128
            || batch.to_cursor.is_empty()
            || batch.to_cursor.len() > 256
            || batch
                .from_cursor
                .as_ref()
                .is_some_and(|value| value.len() > 256)
            || batch.inventory_revision < 1
            || batch.payload_hash.len() != 64
        {
            bail!("history batch identity fields are invalid");
        }
        if batch.records.is_empty() || batch.records.len() > 200 {
            bail!("history batch record count is outside 1..=200");
        }
        let encoded_records = serde_json::to_vec(&batch.records)?;
        if encoded_records.len() > 768 * 1024 {
            bail!("history batch payload exceeds 768 KiB");
        }
        // Reserve the single SQLite writer before reading validation state.
        // A deferred transaction can otherwise deadlock with another writer
        // when both readers later try to upgrade, producing immediate BUSY
        // errors despite the configured busy timeout.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(row) =
            sqlx::query("SELECT to_cursor,device_id,thread_id,inventory_revision FROM history_sync_batches WHERE batch_id=?")
                .bind(&batch.batch_id)
                .fetch_optional(&mut *tx)
                .await?
        {
            if row.get::<String, _>("device_id") != batch.device_id
                || row.get::<String, _>("thread_id") != batch.thread_id
                || row.get::<String, _>("to_cursor") != batch.to_cursor
                || row.get::<i64, _>("inventory_revision") != batch.inventory_revision
            {
                bail!("history batch identity conflict");
            }
            // An acknowledgement can be lost across an upgrade. New serde
            // defaults change the reserialized records and therefore the hash,
            // but a previously committed batch with the same durable identity
            // is already authoritative and must remain idempotently ackable.
            return Ok(row.get("to_cursor"));
        }
        let actual_hash = hex::encode(Sha256::digest(&encoded_records));
        if actual_hash != batch.payload_hash {
            bail!("history batch payload hash mismatch");
        }
        let owner: Option<String> =
            sqlx::query_scalar("SELECT user_id FROM devices WHERE id=? AND status='active'")
                .bind(&batch.device_id)
                .fetch_optional(&mut *tx)
                .await?;
        if owner.as_deref() != Some(user_id) {
            bail!("history device ownership mismatch");
        }
        let removed_thread: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM removed_project_threads WHERE thread_id=? AND device_id=?",
        )
        .bind(&batch.thread_id)
        .bind(&batch.device_id)
        .fetch_optional(&mut *tx)
        .await?;
        if removed_thread.is_some() {
            return Ok(batch.to_cursor.clone());
        }
        let conflicting_link: Option<i64> = sqlx::query_scalar("SELECT 1 FROM history_sync_batches WHERE device_id=? AND thread_id=? AND inventory_revision=? AND from_cursor IS ? AND to_cursor<>?")
            .bind(&batch.device_id).bind(&batch.thread_id).bind(batch.inventory_revision).bind(&batch.from_cursor).bind(&batch.to_cursor)
            .fetch_optional(&mut *tx).await?;
        if conflicting_link.is_some() {
            bail!("history cursor chain forks");
        }
        let fallback_project: String = sqlx::query_scalar("SELECT id FROM projects WHERE device_id=? AND kind='system_unassigned' AND removed_at IS NULL")
            .bind(&batch.device_id).fetch_one(&mut *tx).await?;
        let existing_thread =
            sqlx::query("SELECT user_id,device_id,history_revision FROM threads WHERE id=?")
                .bind(&batch.thread_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(row) = &existing_thread
            && (row.get::<String, _>("user_id") != user_id
                || row.get::<String, _>("device_id") != batch.device_id)
        {
            bail!("history thread ownership conflict");
        }
        let stale = existing_thread
            .as_ref()
            .is_some_and(|row| row.get::<i64, _>("history_revision") > batch.inventory_revision);

        for record in &batch.records {
            if let Some(thread) = &record.thread {
                if thread.id != batch.thread_id || thread.device_id != batch.device_id {
                    bail!("history thread target mismatch");
                }
                let project_exists: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM projects WHERE id=? AND device_id=? AND user_id=? AND removed_at IS NULL")
                        .bind(&thread.project_id)
                        .bind(&batch.device_id)
                        .bind(user_id)
                        .fetch_optional(&mut *tx)
                        .await?;
                let project_id = if project_exists.is_some() {
                    &thread.project_id
                } else {
                    &fallback_project
                };
                if !stale {
                    sqlx::query("INSERT INTO threads(id,user_id,device_id,project_id,provider,app_server_thread_id,title,status,archived,history_completeness,created_at,last_synced_at,last_activity_at,history_revision) VALUES(?,?,?,?,?,?,?,?,?,'backfilling',?,?,?,?) ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id,provider=excluded.provider,app_server_thread_id=COALESCE(excluded.app_server_thread_id,threads.app_server_thread_id),title=excluded.title,status=excluded.status,archived=excluded.archived,created_at=COALESCE(excluded.created_at,threads.created_at),last_synced_at=excluded.last_synced_at,last_activity_at=excluded.last_activity_at,history_revision=excluded.history_revision WHERE threads.user_id=excluded.user_id AND threads.device_id=excluded.device_id AND excluded.history_revision>=threads.history_revision")
                    .bind(&thread.id).bind(user_id).bind(&batch.device_id).bind(project_id).bind(thread.provider.as_str()).bind(&thread.app_server_thread_id).bind(&thread.title).bind(&thread.status).bind(thread.archived)
                    .bind(&thread.created_at).bind(&thread.last_synced_at).bind(&thread.last_activity_at).bind(batch.inventory_revision).execute(&mut *tx).await?;
                }
            }
        }
        let thread_exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM threads WHERE id=? AND user_id=? AND device_id=?")
                .bind(&batch.thread_id)
                .bind(user_id)
                .bind(&batch.device_id)
                .fetch_optional(&mut *tx)
                .await?;
        if thread_exists.is_none() {
            bail!("history batch does not contain its new thread record");
        }
        for record in &batch.records {
            if let Some(turn) = &record.turn {
                if turn.thread_id != batch.thread_id {
                    bail!("history turn target mismatch");
                }
                let existing_owner: Option<String> =
                    sqlx::query_scalar("SELECT thread_id FROM turns WHERE id=?")
                        .bind(&turn.id)
                        .fetch_optional(&mut *tx)
                        .await?;
                if existing_owner
                    .as_deref()
                    .is_some_and(|id| id != batch.thread_id)
                {
                    bail!("history turn ownership conflict");
                }
                if !stale {
                    // A provider can rebuild durable IDs while preserving the
                    // chronological slot. The incoming inventory revision is
                    // authoritative, so replace the stale row occupying that
                    // slot instead of letting the secondary UNIQUE constraint
                    // poison every reconnect with the same batch.
                    if let Some(conflicting_id) = sqlx::query_scalar::<_, String>(
                        "SELECT id FROM turns WHERE thread_id=? AND ordinal=? AND id<>?",
                    )
                    .bind(&turn.thread_id)
                    .bind(turn.ordinal)
                    .bind(&turn.id)
                    .fetch_optional(&mut *tx)
                    .await?
                    {
                        sqlx::query("DELETE FROM turns WHERE id=? AND thread_id=?")
                            .bind(conflicting_id)
                            .bind(&turn.thread_id)
                            .execute(&mut *tx)
                            .await?;
                    }
                    sqlx::query("INSERT INTO turns(id,thread_id,ordinal,status,started_at,completed_at,snapshot_revision) VALUES(?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET ordinal=excluded.ordinal,status=excluded.status,started_at=COALESCE(excluded.started_at,turns.started_at),completed_at=COALESCE(excluded.completed_at,turns.completed_at),snapshot_revision=excluded.snapshot_revision,revision=turns.revision+1")
                        .bind(&turn.id).bind(&turn.thread_id).bind(turn.ordinal).bind(&turn.status).bind(&turn.started_at).bind(&turn.completed_at).bind(batch.inventory_revision).execute(&mut *tx).await?;
                }
            }
        }
        for record in &batch.records {
            if let Some(item) = &record.item {
                let turn_owner: Option<String> =
                    sqlx::query_scalar("SELECT thread_id FROM turns WHERE id=?")
                        .bind(&item.turn_id)
                        .fetch_optional(&mut *tx)
                        .await?;
                if turn_owner.as_deref() != Some(batch.thread_id.as_str()) {
                    bail!("history item turn mismatch or missing prior turn batch");
                }
                let existing_owner: Option<String> = sqlx::query_scalar(
                    "SELECT t.thread_id FROM items i JOIN turns t ON t.id=i.turn_id WHERE i.id=?",
                )
                .bind(&item.id)
                .fetch_optional(&mut *tx)
                .await?;
                if existing_owner
                    .as_deref()
                    .is_some_and(|id| id != batch.thread_id)
                {
                    bail!("history item ownership conflict");
                }
                if stale {
                    continue;
                }
                let detail = item
                    .structured_detail
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                let content_hash =
                    hex::encode(Sha256::digest(serde_json::to_vec(&serde_json::json!({
                        "text": &item.content_text,
                        "detail": &item.structured_detail,
                        "attachments": &item.attachments,
                    }))?));
                if let Some(conflicting_id) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM items WHERE turn_id=? AND ordinal=? AND id<>?",
                )
                .bind(&item.turn_id)
                .bind(item.ordinal)
                .bind(&item.id)
                .fetch_optional(&mut *tx)
                .await?
                {
                    sqlx::query("DELETE FROM items WHERE id=? AND turn_id=?")
                        .bind(conflicting_id)
                        .bind(&item.turn_id)
                        .execute(&mut *tx)
                        .await?;
                }
                let current: Option<(i64,)> =
                    sqlx::query_as("SELECT revision FROM items WHERE id=?")
                        .bind(&item.id)
                        .fetch_optional(&mut *tx)
                        .await?;
                if current.map(|v| v.0).unwrap_or(-1) <= item.revision {
                    sqlx::query("INSERT INTO items(id,turn_id,ordinal,kind,status,revision,content_hash,content_text,structured_detail,is_truncated,occurred_at,completed_at,snapshot_revision) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET ordinal=excluded.ordinal,kind=excluded.kind,status=excluded.status,revision=excluded.revision,content_hash=excluded.content_hash,content_text=excluded.content_text,structured_detail=excluded.structured_detail,is_truncated=excluded.is_truncated,occurred_at=excluded.occurred_at,completed_at=excluded.completed_at,snapshot_revision=excluded.snapshot_revision WHERE excluded.revision>=items.revision")
                        .bind(&item.id).bind(&item.turn_id).bind(item.ordinal).bind(&item.kind).bind(&item.status).bind(item.revision)
                        .bind(content_hash).bind(&item.content_text).bind(detail).bind(item.is_truncated).bind(&item.occurred_at).bind(&item.completed_at).bind(batch.inventory_revision).execute(&mut *tx).await?;
                    sqlx::query("DELETE FROM item_attachments WHERE item_id=?")
                        .bind(&item.id)
                        .execute(&mut *tx)
                        .await?;
                    for (index, attachment) in item.attachments.iter().enumerate() {
                        let stored = sqlx::query("SELECT * FROM attachments WHERE id=? AND user_id=? AND device_id=? AND thread_id=? AND status<>'deleted'")
                            .bind(&attachment.id)
                            .bind(user_id)
                            .bind(&batch.device_id)
                            .bind(&batch.thread_id)
                            .fetch_optional(&mut *tx)
                            .await?
                            .context("history references an unavailable attachment")?;
                        let stored = attachment_view_from_row(&stored);
                        if &stored != attachment {
                            bail!("history attachment metadata mismatch");
                        }
                        sqlx::query("INSERT INTO item_attachments(item_id,attachment_id,ordinal) VALUES(?,?,?)")
                            .bind(&item.id)
                            .bind(&attachment.id)
                            .bind(index as i64 + 1)
                            .execute(&mut *tx)
                            .await?;
                    }
                } else {
                    // Presence in the authoritative snapshot is independent of
                    // content revision. Keep the newer server copy, but mark it
                    // seen so final-chunk cleanup cannot delete a valid item.
                    sqlx::query("UPDATE items SET snapshot_revision=? WHERE id=?")
                        .bind(batch.inventory_revision)
                        .bind(&item.id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        sqlx::query("INSERT INTO history_sync_batches(batch_id,device_id,thread_id,from_cursor,to_cursor,payload_hash,record_count,received_at,committed_at,inventory_revision) VALUES(?,?,?,?,?,?,?,?,?,?)")
            .bind(&batch.batch_id).bind(&batch.device_id).bind(&batch.thread_id).bind(&batch.from_cursor).bind(&batch.to_cursor).bind(&batch.payload_hash)
            .bind(batch.records.len() as i64).bind(now()).bind(now()).bind(batch.inventory_revision).execute(&mut *tx).await?;
        let chain_complete = if batch.complete && !stale {
            let rows = sqlx::query("SELECT from_cursor,to_cursor FROM history_sync_batches WHERE device_id=? AND thread_id=? AND inventory_revision=?")
                .bind(&batch.device_id).bind(&batch.thread_id).bind(batch.inventory_revision).fetch_all(&mut *tx).await?;
            let links = rows
                .into_iter()
                .map(|row| {
                    (
                        row.get::<Option<String>, _>("from_cursor"),
                        row.get::<String, _>("to_cursor"),
                    )
                })
                .collect::<std::collections::HashMap<_, _>>();
            let mut cursor: Option<String> = None;
            let mut reached = false;
            for _ in 0..=links.len() {
                let Some(next) = links.get(&cursor) else {
                    break;
                };
                if next == &batch.to_cursor {
                    reached = true;
                    break;
                }
                cursor = Some(next.clone());
            }
            reached
        } else {
            false
        };
        if chain_complete {
            // The device batch chain describes the complete SQLite snapshot.
            // Removing rows not marked by this revision prevents stale or
            // formerly duplicated messages from surviving forever remotely.
            sqlx::query("DELETE FROM items WHERE turn_id IN (SELECT id FROM turns WHERE thread_id=?) AND snapshot_revision<?")
                .bind(&batch.thread_id)
                .bind(batch.inventory_revision)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM turns WHERE thread_id=? AND snapshot_revision<?")
                .bind(&batch.thread_id)
                .bind(batch.inventory_revision)
                .execute(&mut *tx)
                .await?;
        }
        if !stale {
            sqlx::query("UPDATE threads SET history_cursor=?,last_synced_at=?,history_completeness=?,history_revision=? WHERE id=? AND history_revision<=?")
                .bind(&batch.to_cursor).bind(now()).bind(if chain_complete { "complete" } else { "backfilling" }).bind(batch.inventory_revision)
                .bind(&batch.thread_id).bind(batch.inventory_revision).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE devices SET history_completeness=CASE WHEN history_completeness='not_started' THEN 'backfilling' ELSE history_completeness END,history_last_synced_at=? WHERE id=?")
            .bind(now()).bind(&batch.device_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(batch.to_cursor.clone())
    }

    pub async fn upsert_project_summary(
        &self,
        user_id: &str,
        project: &ProjectSummary,
        summary_version: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let device_owner: Option<String> =
            sqlx::query_scalar("SELECT user_id FROM devices WHERE id=? AND status='active'")
                .bind(&project.device_id)
                .fetch_optional(&mut *tx)
                .await?;
        if device_owner.as_deref() != Some(user_id) {
            bail!("project summary device ownership mismatch");
        }
        if let Some(row) = sqlx::query("SELECT user_id,device_id,kind FROM projects WHERE id=?")
            .bind(&project.id)
            .fetch_optional(&mut *tx)
            .await?
            && (row.get::<String, _>("user_id") != user_id
                || row.get::<String, _>("device_id") != project.device_id
                || row.get::<String, _>("kind") == "system_unassigned")
        {
            bail!("project summary identity conflict");
        }
        if project.kind == ProjectKind::SystemUnassigned {
            bail!("device cannot replace the server-owned unassigned project");
        }
        sqlx::query("INSERT INTO projects(id,user_id,device_id,kind,display_name,path_hint,status,repo_name,branch,is_dirty,summary_version,thread_count,last_activity_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,path_hint=excluded.path_hint,status=excluded.status,repo_name=excluded.repo_name,branch=excluded.branch,is_dirty=excluded.is_dirty,thread_count=excluded.thread_count,last_activity_at=excluded.last_activity_at,summary_version=excluded.summary_version WHERE excluded.summary_version>projects.summary_version")
            .bind(&project.id).bind(user_id).bind(&project.device_id).bind(project_kind_string(project.kind)).bind(&project.display_name).bind(&project.path_hint).bind(&project.status)
            .bind(&project.repo_name).bind(&project.branch).bind(project.is_dirty).bind(summary_version).bind(project.thread_count).bind(&project.last_activity_at).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn remove_project_records(
    tx: &mut Transaction<'_, Sqlite>,
    device_id: &str,
    project_id: &str,
) -> Result<bool> {
    let stamp = now();
    sqlx::query("INSERT OR IGNORE INTO removed_project_threads(thread_id,project_id,device_id,removed_at) SELECT id,project_id,device_id,? FROM threads WHERE project_id=? AND device_id=?")
        .bind(&stamp)
        .bind(project_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM approvals WHERE project_id=? AND device_id=?")
        .bind(project_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM history_sync_batches WHERE thread_id IN (SELECT id FROM threads WHERE project_id=? AND device_id=?)")
        .bind(project_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM threads WHERE project_id=? AND device_id=?")
        .bind(project_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?;
    let changed = sqlx::query("UPDATE projects SET status='removed',thread_count=0,removed_at=COALESCE(removed_at,?) WHERE id=? AND device_id=? AND kind='workspace' AND removed_at IS NULL")
        .bind(&stamp)
        .bind(project_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    Ok(changed == 1)
}

fn project_from_row(r: sqlx::sqlite::SqliteRow) -> ProjectSummary {
    ProjectSummary {
        id: r.get("id"),
        device_id: r.get("device_id"),
        kind: if r.get::<String, _>("kind") == "system_unassigned" {
            ProjectKind::SystemUnassigned
        } else {
            ProjectKind::Workspace
        },
        display_name: r.get("display_name"),
        path_hint: r.get("path_hint"),
        status: r.get("status"),
        repo_name: r.get("repo_name"),
        branch: r.get("branch"),
        is_dirty: r.get::<Option<i64>, _>("is_dirty").map(|v| v != 0),
        thread_count: r.get("thread_count"),
        last_activity_at: r.get("last_activity_at"),
    }
}

fn thread_from_row(r: sqlx::sqlite::SqliteRow) -> ThreadSummary {
    ThreadSummary {
        id: r.get("id"),
        device_id: r.get("device_id"),
        project_id: r.get("project_id"),
        provider: match r.get::<String, _>("provider").as_str() {
            "kimi" => AgentProvider::Kimi,
            "pi" => AgentProvider::Pi,
            _ => AgentProvider::Codex,
        },
        app_server_thread_id: r.get("app_server_thread_id"),
        title: r.get("title"),
        status: r.get("status"),
        archived: r.get::<i64, _>("archived") != 0,
        history_completeness: parse_completeness(&r.get::<String, _>("history_completeness")),
        created_at: r.get("created_at"),
        last_synced_at: r.get("last_synced_at"),
        last_activity_at: r.get("last_activity_at"),
    }
}

fn item_from_row(r: sqlx::sqlite::SqliteRow) -> HistoryItemView {
    HistoryItemView {
        id: r.get("id"),
        turn_id: r.get("turn_id"),
        ordinal: r.get("ordinal"),
        kind: r.get("kind"),
        status: r.get("status"),
        revision: r.get("revision"),
        content_text: r.get("content_text"),
        structured_detail: r
            .get::<Option<String>, _>("structured_detail")
            .and_then(|s| serde_json::from_str(&s).ok()),
        is_truncated: r.get::<i64, _>("is_truncated") != 0,
        occurred_at: r.get("occurred_at"),
        completed_at: r.get("completed_at"),
        attachments: Vec::new(),
    }
}

fn attachment_view_from_row(r: &sqlx::sqlite::SqliteRow) -> AttachmentView {
    AttachmentView {
        id: r.get("id"),
        original_name: r.get("original_name"),
        mime_type: r.get("mime_type"),
        byte_size: r.get("byte_size"),
        sha256: r.get("sha256"),
        width: r.get::<i64, _>("width") as u32,
        height: r.get::<i64, _>("height") as u32,
    }
}

fn attachment_ref_from_row(r: &sqlx::sqlite::SqliteRow) -> AttachmentRef {
    let view = attachment_view_from_row(r);
    AttachmentRef {
        id: view.id,
        original_name: view.original_name,
        mime_type: view.mime_type,
        extension: r.get("extension"),
        byte_size: view.byte_size,
        sha256: view.sha256,
        width: view.width,
        height: view.height,
    }
}

fn parse_command_status(value: &str) -> CommandStatus {
    match value {
        "accepted" => CommandStatus::Accepted,
        "waiting_device" => CommandStatus::WaitingDevice,
        "device_accepted" => CommandStatus::DeviceAccepted,
        "applying" => CommandStatus::Applying,
        "completed" => CommandStatus::Completed,
        "failed" => CommandStatus::Failed,
        "rejected" => CommandStatus::Rejected,
        "unknown" => CommandStatus::Unknown,
        "expired" => CommandStatus::Expired,
        _ => CommandStatus::Unknown,
    }
}

fn command_status_terminal(status: CommandStatus) -> bool {
    matches!(
        status,
        CommandStatus::Completed
            | CommandStatus::Failed
            | CommandStatus::Rejected
            | CommandStatus::Unknown
            | CommandStatus::Expired
    )
}

async fn expire_approval_payloads(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    rows: Vec<sqlx::sqlite::SqliteRow>,
) -> Result<()> {
    for row in rows {
        let command: DeviceCommand = serde_json::from_str(&row.get::<String, _>("payload"))?;
        if let DeviceCommandKind::ApprovalDecide { approval_id, .. } = command.command {
            sqlx::query("UPDATE approvals SET status='expired',decided_at=?,last_error='expired_before_delivery' WHERE id=? AND status='responding'")
                .bind(now()).bind(approval_id).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

fn parse_completeness(value: &str) -> HistoryCompleteness {
    match value {
        "backfilling" => HistoryCompleteness::Backfilling,
        "complete" => HistoryCompleteness::Complete,
        "partial" => HistoryCompleteness::Partial,
        "error" => HistoryCompleteness::Error,
        _ => HistoryCompleteness::NotStarted,
    }
}
fn parse_transport(value: &str) -> TransportSecurity {
    if value == "secure" {
        TransportSecurity::Secure
    } else if value == "local" {
        TransportSecurity::Local
    } else {
        TransportSecurity::Insecure
    }
}
fn transport_string(value: TransportSecurity) -> &'static str {
    match value {
        TransportSecurity::Secure => "secure",
        TransportSecurity::Insecure => "insecure",
        TransportSecurity::Local => "local",
    }
}
fn project_kind_string(value: ProjectKind) -> &'static str {
    match value {
        ProjectKind::Workspace => "workspace",
        ProjectKind::SystemUnassigned => "system_unassigned",
    }
}

pub fn unix_to_rfc3339(timestamp: i64) -> Result<String> {
    Ok(OffsetDateTime::from_unix_timestamp(timestamp)?
        .format(&time::format_description::well_known::Rfc3339)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn web_session_survives_store_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let (session_id, queue_epoch) = {
            let store = ServerStore::open(temp.path()).await.unwrap();
            let user = store.create_owner("owner", "test-hash").await.unwrap();
            let (session_id, _) = store
                .create_session(
                    &user,
                    "persistent-session-hash",
                    "persistent-csrf-hash",
                    1,
                    Some("restart-test"),
                )
                .await
                .unwrap();
            (session_id, store.queue_epoch().to_owned())
        };

        let reopened = ServerStore::open(temp.path()).await.unwrap();
        assert_eq!(reopened.queue_epoch(), queue_epoch);
        let session = reopened
            .session_by_token_hash("persistent-session-hash")
            .await
            .unwrap()
            .expect("persisted session should remain valid after reopening SQLite");
        assert_eq!(session.id, session_id);
        assert_eq!(session.login_name, "owner");
    }

    #[tokio::test]
    async fn renamed_device_name_is_the_reconnect_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let store = ServerStore::open(temp.path()).await.unwrap();
        let user = store.create_owner("owner", "test-hash").await.unwrap();
        store
            .create_pairing_code(&user.id, "pair-hash", 10)
            .await
            .unwrap();
        let device_id = store
            .pair_device(
                &PairDeviceRequest {
                    code: "unused".into(),
                    display_name: "Old name".into(),
                    public_key: "key".into(),
                    agent_version: "test".into(),
                    os_family: "test".into(),
                    architecture: "test".into(),
                },
                "pair-hash",
            )
            .await
            .unwrap();

        assert!(
            store
                .rename_device(&user.id, &device_id, "Studio Mac")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .device_display_name(&user.id, &device_id)
                .await
                .unwrap()
                .as_deref(),
            Some("Studio Mac")
        );
    }

    #[tokio::test]
    async fn provider_migration_accepts_pi_threads() {
        let temp = tempfile::tempdir().unwrap();
        let store = ServerStore::open(temp.path()).await.unwrap();
        let user = store.create_owner("owner", "test-hash").await.unwrap();
        store
            .create_pairing_code(&user.id, "pair-hash", 10)
            .await
            .unwrap();
        let device_id = store
            .pair_device(
                &PairDeviceRequest {
                    code: "unused".into(),
                    display_name: "Device".into(),
                    public_key: "key".into(),
                    agent_version: "test".into(),
                    os_family: "test".into(),
                    architecture: "test".into(),
                },
                "pair-hash",
            )
            .await
            .unwrap();
        store
            .upsert_project_summary(
                &user.id,
                &ProjectSummary {
                    id: "prj_pi".into(),
                    device_id: device_id.clone(),
                    kind: ProjectKind::Workspace,
                    display_name: "Pi workspace".into(),
                    path_hint: Some("workspace".into()),
                    status: "active".into(),
                    repo_name: None,
                    branch: None,
                    is_dirty: None,
                    thread_count: 1,
                    last_activity_at: Some(now()),
                },
                1,
            )
            .await
            .unwrap();

        store
            .upsert_created_thread(
                &user.id,
                &ThreadSummary {
                    id: "thr_pi".into(),
                    device_id: device_id.clone(),
                    project_id: "prj_pi".into(),
                    provider: AgentProvider::Pi,
                    app_server_thread_id: Some("app_pi".into()),
                    title: "Pi thread".into(),
                    status: "idle".into(),
                    archived: false,
                    history_completeness: HistoryCompleteness::Complete,
                    created_at: Some(now()),
                    last_synced_at: Some(now()),
                    last_activity_at: Some(now()),
                },
            )
            .await
            .unwrap();

        let threads = store
            .list_threads(&user.id, Some(&device_id), Some("prj_pi"), 10, 0)
            .await
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].provider, AgentProvider::Pi);
    }

    #[tokio::test]
    async fn completed_project_delete_command_removes_server_records() {
        let temp = tempfile::tempdir().unwrap();
        let store = ServerStore::open(temp.path()).await.unwrap();
        let user = store.create_owner("owner", "test-hash").await.unwrap();
        store
            .create_pairing_code(&user.id, "pair-hash", 10)
            .await
            .unwrap();
        let device_id = store
            .pair_device(
                &PairDeviceRequest {
                    code: "unused".into(),
                    display_name: "Device".into(),
                    public_key: "key".into(),
                    agent_version: "test".into(),
                    os_family: "test".into(),
                    architecture: "test".into(),
                },
                "pair-hash",
            )
            .await
            .unwrap();
        store
            .upsert_project_summary(
                &user.id,
                &ProjectSummary {
                    id: "prj_remove".into(),
                    device_id: device_id.clone(),
                    kind: ProjectKind::Workspace,
                    display_name: "Remove me".into(),
                    path_hint: Some("workspace".into()),
                    status: "active".into(),
                    repo_name: None,
                    branch: None,
                    is_dirty: None,
                    thread_count: 1,
                    last_activity_at: Some(now()),
                },
                1,
            )
            .await
            .unwrap();
        store
            .upsert_created_thread(
                &user.id,
                &ThreadSummary {
                    id: "thr_remove".into(),
                    device_id: device_id.clone(),
                    project_id: "prj_remove".into(),
                    provider: AgentProvider::Codex,
                    app_server_thread_id: Some("app_remove".into()),
                    title: "Old thread".into(),
                    status: "idle".into(),
                    archived: false,
                    history_completeness: HistoryCompleteness::Complete,
                    created_at: Some(now()),
                    last_synced_at: Some(now()),
                    last_activity_at: Some(now()),
                },
            )
            .await
            .unwrap();
        let command = DeviceCommand {
            command_id: "cmd_remove".into(),
            device_id: device_id.clone(),
            project_id: Some("prj_remove".into()),
            thread_id: None,
            issued_at: now(),
            expires_at: now(),
            command: DeviceCommandKind::ProjectDelete {
                project_id: "prj_remove".into(),
            },
        };
        store
            .insert_command(
                &user.id,
                "remove-idem",
                "remove-fingerprint",
                &command,
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .update_command_ack(
                    &device_id,
                    "cmd_remove",
                    "completed",
                    Some(&serde_json::json!({"projectId":"prj_remove"})),
                    None,
                    None,
                )
                .await
                .unwrap(),
            CommandStatus::Completed
        );
        assert!(
            store
                .list_projects(&user.id, &device_id)
                .await
                .unwrap()
                .iter()
                .all(|project| project.id != "prj_remove")
        );
        assert!(
            store
                .list_threads(&user.id, Some(&device_id), None, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let tombstone: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM removed_project_threads WHERE thread_id='thr_remove'",
        )
        .fetch_optional(store.pool())
        .await
        .unwrap();
        assert_eq!(tombstone, Some(1));
        assert!(
            !store
                .remove_project(&user.id, &device_id, "prj_remove")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn command_idempotency_returns_original_command() {
        let temp = tempfile::tempdir().unwrap();
        let store = ServerStore::open(temp.path()).await.unwrap();
        let user = store.create_owner("owner", "test-hash").await.unwrap();
        store
            .create_pairing_code(&user.id, "pair-hash", 10)
            .await
            .unwrap();
        let device_id = store
            .pair_device(
                &PairDeviceRequest {
                    code: "unused".into(),
                    display_name: "Device".into(),
                    public_key: "key".into(),
                    agent_version: "test".into(),
                    os_family: "test".into(),
                    architecture: "test".into(),
                },
                "pair-hash",
            )
            .await
            .unwrap();
        let make_command = |id: &str| DeviceCommand {
            command_id: id.into(),
            device_id: device_id.clone(),
            project_id: None,
            thread_id: None,
            issued_at: now(),
            expires_at: now(),
            command: DeviceCommandKind::Refresh,
        };
        let first = store
            .insert_command(
                &user.id,
                "idem",
                "fingerprint",
                &make_command("cmd_1"),
                i64::MAX,
            )
            .await
            .unwrap();
        assert!(first.newly_created);
        let retry = store
            .insert_command(
                &user.id,
                "idem",
                "fingerprint",
                &make_command("cmd_2"),
                i64::MAX,
            )
            .await
            .unwrap();
        assert!(!retry.newly_created);
        assert_eq!(retry.command.command_id, "cmd_1");
        assert_eq!(retry.sequence, first.sequence);
        assert!(
            store
                .insert_command(
                    &user.id,
                    "idem",
                    "different",
                    &make_command("cmd_3"),
                    i64::MAX
                )
                .await
                .is_err()
        );
        let second = store
            .insert_command(
                &user.id,
                "idem-2",
                "fingerprint-2",
                &make_command("cmd_2"),
                i64::MAX,
            )
            .await
            .unwrap();
        assert_eq!(second.sequence, first.sequence + 1);
        assert_eq!(
            store
                .update_command_ack(&device_id, "cmd_1", "persisted", None, None, None)
                .await
                .unwrap(),
            CommandStatus::DeviceAccepted
        );
        assert_eq!(
            store
                .update_command_ack(&device_id, "cmd_1", "applying", None, None, None)
                .await
                .unwrap(),
            CommandStatus::Applying
        );
        assert_eq!(
            store
                .update_command_ack(
                    &device_id,
                    "cmd_1",
                    "completed",
                    Some(&serde_json::json!({"ok":true})),
                    None,
                    None,
                )
                .await
                .unwrap(),
            CommandStatus::Completed
        );
        // A delayed ACK can never regress a terminal result.
        assert_eq!(
            store
                .update_command_ack(&device_id, "cmd_1", "persisted", None, None, None)
                .await
                .unwrap(),
            CommandStatus::Completed
        );
        let (session_id, _) = store
            .create_session(&user, "session-hash", "csrf-one", 1, None)
            .await
            .unwrap();
        store.rotate_csrf(&session_id, "csrf-two").await.unwrap();
        assert!(
            store
                .csrf_token_valid(&session_id, "csrf-one")
                .await
                .unwrap()
        );
        assert!(
            store
                .csrf_token_valid(&session_id, "csrf-two")
                .await
                .unwrap()
        );
        let history_records = vec![
            HistoryRecord {
                thread: Some(ThreadSummary {
                    id: "thr_history".into(),
                    device_id: device_id.clone(),
                    project_id: "unknown-local-project".into(),
                    provider: AgentProvider::Codex,
                    app_server_thread_id: Some("app_history".into()),
                    title: "Imported".into(),
                    status: "idle".into(),
                    archived: false,
                    history_completeness: HistoryCompleteness::Complete,
                    created_at: Some(now()),
                    last_synced_at: Some(now()),
                    last_activity_at: Some(now()),
                }),
                turn: None,
                item: None,
            },
            HistoryRecord {
                thread: None,
                turn: Some(HistoryTurnView {
                    id: "trn_history".into(),
                    thread_id: "thr_history".into(),
                    ordinal: 1,
                    status: "completed".into(),
                    started_at: Some(now()),
                    completed_at: Some(now()),
                }),
                item: None,
            },
            HistoryRecord {
                thread: None,
                turn: None,
                item: Some(HistoryItemView {
                    id: "itm_history_user".into(),
                    turn_id: "trn_history".into(),
                    ordinal: 1,
                    kind: "user_message".into(),
                    status: "completed".into(),
                    revision: 1,
                    content_text: Some("question".into()),
                    structured_detail: None,
                    is_truncated: false,
                    occurred_at: now(),
                    completed_at: Some(now()),
                    attachments: Vec::new(),
                }),
            },
            HistoryRecord {
                thread: None,
                turn: None,
                item: Some(HistoryItemView {
                    id: "itm_history_agent".into(),
                    turn_id: "trn_history".into(),
                    ordinal: 2,
                    kind: "agent_message".into(),
                    status: "completed".into(),
                    revision: 1,
                    content_text: Some("done".into()),
                    structured_detail: None,
                    is_truncated: false,
                    occurred_at: now(),
                    completed_at: Some(now()),
                    attachments: Vec::new(),
                }),
            },
        ];
        let history_hash = hex::encode(Sha256::digest(
            serde_json::to_vec(&history_records).unwrap(),
        ));
        let history_batch = HistoryBatch {
            batch_id: "hbatch_test".into(),
            device_id: device_id.clone(),
            thread_id: "thr_history".into(),
            from_cursor: None,
            to_cursor: "cursor_test".into(),
            inventory_revision: 1,
            payload_hash: history_hash,
            complete: true,
            records: history_records,
        };
        assert_eq!(
            store
                .ingest_history_batch(&user.id, &history_batch)
                .await
                .unwrap(),
            "cursor_test"
        );
        let imported = store
            .list_threads(&user.id, Some(&device_id), None, 10, 0)
            .await
            .unwrap();
        assert_eq!(
            imported[0].history_completeness,
            HistoryCompleteness::Complete
        );
        let stored_messages = sqlx::query_scalar::<_, String>(
            "SELECT i.content_text FROM items i JOIN turns t ON t.id=i.turn_id WHERE t.thread_id=? ORDER BY i.ordinal",
        )
        .bind("thr_history")
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert_eq!(stored_messages, vec!["question", "done"]);

        let mut upgraded_replay = history_batch.clone();
        upgraded_replay.payload_hash = "0".repeat(64);
        assert_eq!(
            store
                .ingest_history_batch(&user.id, &upgraded_replay)
                .await
                .unwrap(),
            "cursor_test"
        );

        let pruned_records = history_batch
            .records
            .iter()
            .filter(|record| {
                record
                    .item
                    .as_ref()
                    .is_none_or(|item| item.id != "itm_history_agent")
            })
            .cloned()
            .collect::<Vec<_>>();
        let pruned_batch = HistoryBatch {
            batch_id: "hbatch_pruned".into(),
            device_id: device_id.clone(),
            thread_id: "thr_history".into(),
            from_cursor: None,
            to_cursor: "cursor_pruned".into(),
            inventory_revision: 2,
            payload_hash: hex::encode(Sha256::digest(serde_json::to_vec(&pruned_records).unwrap())),
            complete: true,
            records: pruned_records,
        };
        store
            .ingest_history_batch(&user.id, &pruned_batch)
            .await
            .unwrap();
        let stored_messages = sqlx::query_scalar::<_, String>(
            "SELECT i.content_text FROM items i JOIN turns t ON t.id=i.turn_id WHERE t.thread_id=? ORDER BY i.ordinal",
        )
        .bind("thr_history")
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert_eq!(stored_messages, vec!["question"]);

        let mut replaced_records = pruned_batch.records.clone();
        for record in &mut replaced_records {
            if let Some(turn) = &mut record.turn {
                turn.id = "trn_history_replaced".into();
            }
            if let Some(item) = &mut record.item {
                item.id = "itm_history_replaced".into();
                item.turn_id = "trn_history_replaced".into();
                item.content_text = Some("replacement question".into());
            }
        }
        let replaced_batch = HistoryBatch {
            batch_id: "hbatch_replaced_ids".into(),
            device_id: device_id.clone(),
            thread_id: "thr_history".into(),
            from_cursor: None,
            to_cursor: "cursor_replaced_ids".into(),
            inventory_revision: 3,
            payload_hash: hex::encode(Sha256::digest(
                serde_json::to_vec(&replaced_records).unwrap(),
            )),
            complete: true,
            records: replaced_records,
        };
        store
            .ingest_history_batch(&user.id, &replaced_batch)
            .await
            .unwrap();
        let replaced_identity = sqlx::query_as::<_, (String, String, String)>(
            "SELECT t.id,i.id,i.content_text FROM turns t JOIN items i ON i.turn_id=t.id WHERE t.thread_id=? ORDER BY i.ordinal LIMIT 1",
        )
        .bind("thr_history")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(
            replaced_identity,
            (
                "trn_history_replaced".into(),
                "itm_history_replaced".into(),
                "replacement question".into()
            )
        );

        store
            .create_pairing_code(&user.id, "pair-hash-second", 10)
            .await
            .unwrap();
        let second_device = store
            .pair_device(
                &PairDeviceRequest {
                    code: "unused".into(),
                    display_name: "Second Device".into(),
                    public_key: "second-key".into(),
                    agent_version: "test".into(),
                    os_family: "test".into(),
                    architecture: "test".into(),
                },
                "pair-hash-second",
            )
            .await
            .unwrap();
        let second_records = vec![
            HistoryRecord {
                thread: Some(ThreadSummary {
                    id: "thr_history_second".into(),
                    device_id: second_device.clone(),
                    project_id: "unknown-second-project".into(),
                    provider: AgentProvider::Codex,
                    app_server_thread_id: Some("app_history".into()),
                    title: "Second imported thread".into(),
                    status: "idle".into(),
                    archived: false,
                    history_completeness: HistoryCompleteness::Complete,
                    created_at: Some(now()),
                    last_synced_at: Some(now()),
                    last_activity_at: Some(now()),
                }),
                turn: None,
                item: None,
            },
            HistoryRecord {
                thread: None,
                turn: Some(HistoryTurnView {
                    id: "trn_history_second".into(),
                    thread_id: "thr_history_second".into(),
                    ordinal: 1,
                    status: "completed".into(),
                    started_at: Some(now()),
                    completed_at: Some(now()),
                }),
                item: None,
            },
            HistoryRecord {
                thread: None,
                turn: None,
                item: Some(HistoryItemView {
                    id: "itm_history_second".into(),
                    turn_id: "trn_history_second".into(),
                    ordinal: 1,
                    kind: "user_message".into(),
                    status: "completed".into(),
                    revision: 1,
                    content_text: Some("from second device".into()),
                    structured_detail: None,
                    is_truncated: false,
                    occurred_at: now(),
                    completed_at: Some(now()),
                    attachments: Vec::new(),
                }),
            },
        ];
        let second_batch = HistoryBatch {
            batch_id: "hbatch_second".into(),
            device_id: second_device.clone(),
            thread_id: "thr_history_second".into(),
            from_cursor: None,
            to_cursor: "cursor_second".into(),
            inventory_revision: 1,
            payload_hash: hex::encode(Sha256::digest(serde_json::to_vec(&second_records).unwrap())),
            complete: true,
            records: second_records,
        };
        store
            .ingest_history_batch(&user.id, &second_batch)
            .await
            .unwrap();
        let all_devices = store
            .list_threads(&user.id, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(all_devices.len(), 2);
        assert!(
            all_devices
                .iter()
                .any(|thread| thread.device_id == second_device)
        );
        let mut corrupt_batch = history_batch.clone();
        corrupt_batch.batch_id = "hbatch_corrupt".into();
        corrupt_batch.payload_hash = "bad".into();
        assert!(
            store
                .ingest_history_batch(&user.id, &corrupt_batch)
                .await
                .is_err()
        );
        let approval = NuntiusEvent {
            event_id: "evt_approval".into(),
            user_id: Some(user.id.clone()),
            device_id: device_id.clone(),
            project_id: None,
            thread_id: Some("thr_approval".into()),
            turn_id: None,
            stream_id: format!("device:{device_id}"),
            seq: 1,
            event_type: "approval.requested".into(),
            durability: "durable".into(),
            occurred_at: now(),
            payload: serde_json::json!({"approvalId":"apr_test","method":"item/commandExecution/requestApproval","params":{"command":"echo test"}}),
        };
        store
            .upsert_approval_event(&user.id, &approval)
            .await
            .unwrap();
        assert_eq!(store.list_approvals(&user.id, true).await.unwrap().len(), 1);
        assert_eq!(
            store.approval_device(&user.id, "apr_test").await.unwrap(),
            Some(device_id.clone())
        );
        let mut resolved = approval.clone();
        resolved.event_id = "evt_approval_resolved".into();
        resolved.event_type = "approval.resolved".into();
        resolved.payload = serde_json::json!({
            "approvalId":"apr_test",
            "status":"decided",
            "decision":"accept"
        });
        assert_eq!(
            store
                .resolve_approval_event(&user.id, &resolved)
                .await
                .unwrap(),
            1
        );
        assert!(
            store
                .list_approvals(&user.id, true)
                .await
                .unwrap()
                .is_empty()
        );
        let mut expiring = approval.clone();
        expiring.event_id = "evt_approval_expiring".into();
        expiring.payload = serde_json::json!({
            "approvalId":"apr_expiring",
            "method":"item/commandExecution/requestApproval",
            "params":{"command":"echo test"}
        });
        store
            .upsert_approval_event(&user.id, &expiring)
            .await
            .unwrap();
        assert_eq!(
            store
                .expire_thread_approvals(
                    &user.id,
                    &device_id,
                    "thr_approval",
                    "2026-07-21T12:00:00Z",
                )
                .await
                .unwrap(),
            1
        );
        assert!(
            store
                .list_approvals(&user.id, true)
                .await
                .unwrap()
                .is_empty()
        );
        let backup = temp.path().join("backup.db");
        store.backup(&backup).await.unwrap();
        assert!(backup.is_file());
    }
}
