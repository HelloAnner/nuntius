use crate::{executor::CommandExecutor, protocol::AgentProvider};
use anyhow::{Context, Result};
use directories::BaseDirs;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const RECENT_ROLLOUT_WINDOW: Duration = Duration::from_secs(30 * 60);
const STALLED_AFTER: TimeDuration = TimeDuration::minutes(30);
const INVENTORY_SCAN_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RolloutStamp {
    length: u64,
    modified_nanos: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RolloutRuntimeState {
    Active,
    Completed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RolloutRuntimeSignal {
    state: RolloutRuntimeState,
    turn_id: Option<String>,
    occurred_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RolloutInventory {
    app_id: String,
    cwd: String,
    created_at: Option<String>,
    updated_at_ms: i64,
    title: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct RolloutCursor {
    offset: u64,
    signal: Option<RolloutRuntimeSignal>,
}

struct RetryEntry {
    signal: Option<RolloutRuntimeSignal>,
    failures: u32,
    next_attempt: Instant,
}

impl RetryEntry {
    fn new(signal: Option<RolloutRuntimeSignal>) -> Self {
        Self {
            signal,
            failures: 0,
            next_attempt: Instant::now(),
        }
    }

    fn update_signal(&mut self, signal: Option<RolloutRuntimeSignal>) {
        if signal.is_some() && self.signal != signal {
            self.signal = signal;
            self.failures = 0;
            self.next_attempt = Instant::now();
        }
    }

    fn back_off(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let multiplier = 1_u64 << self.failures.min(5);
        let delay = Duration::from_millis(
            (POLL_INTERVAL.as_millis() as u64)
                .saturating_mul(multiplier)
                .min(MAX_RETRY_DELAY.as_millis() as u64),
        );
        self.next_attempt = Instant::now() + delay;
    }
}

/// Watches Codex's durable rollout files. App Server notifications only cover
/// work started through our own process; rollout changes also cover terminal,
/// IDE and `codex exec` sessions that share this workstation's CODEX_HOME.
pub async fn run(executor: CommandExecutor) {
    let roots = codex_history_roots();
    let mut known = scan_roots(roots.clone()).await.unwrap_or_else(|error| {
        tracing::warn!(error=?error,"cannot seed Codex rollout monitor");
        HashMap::new()
    });
    let mut cursors = HashMap::<PathBuf, RolloutCursor>::new();
    let mut retry = HashMap::<String, RetryEntry>::new();
    let mut inventory_imported = 0_usize;
    // Runtime projection is the startup priority. A terminal/IDE turn must be
    // shown as running before the slower full-device inventory backfill starts.
    for (path, (thread_id, stamp)) in &known {
        if !stamp_is_recent(stamp, RECENT_ROLLOUT_WINDOW) {
            continue;
        }
        if !is_archived_rollout(path) {
            match ensure_rollout_inventory(&executor, path, thread_id, stamp).await {
                Ok(true) => inventory_imported += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(path=%path.display(),error=?error,"cannot seed recent Codex rollout inventory")
                }
            }
        }
        match advance_rollout_cursor(path.clone(), RolloutCursor::default(), stamp.length).await {
            Ok(cursor) => {
                let signal = cursor.signal.clone();
                cursors.insert(path.clone(), cursor);
                if let Some(signal) = signal.as_ref()
                    && let Err(error) = apply_rollout_runtime(&executor, thread_id, signal).await
                {
                    tracing::warn!(%thread_id,error=?error,"cannot seed recent Codex runtime state");
                }
                retry.insert(thread_id.clone(), RetryEntry::new(signal));
            }
            Err(error) => {
                tracing::warn!(path=%path.display(),error=?error,"cannot seed recent Codex rollout lifecycle");
            }
        }
    }
    // Rollout files are also the durable inventory fallback when Codex's state
    // DB and `thread/list` omit older sessions. This metadata-only pass is
    // intentionally second so it can never delay current runtime visibility.
    for (path, (thread_id, stamp)) in &known {
        if is_archived_rollout(path) || stamp_is_recent(stamp, RECENT_ROLLOUT_WINDOW) {
            continue;
        }
        match ensure_rollout_inventory(&executor, path, thread_id, stamp).await {
            Ok(true) => inventory_imported += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(path=%path.display(),error=?error,"cannot seed Codex rollout inventory")
            }
        }
    }
    if inventory_imported > 0 {
        tracing::info!(inventory_imported, "Codex rollout inventory backfilled");
    }
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ticks = 0_u64;

    loop {
        interval.tick().await;
        ticks += 1;
        match scan_roots(roots.clone()).await {
            Ok(current) => {
                for (path, (thread_id, stamp)) in &current {
                    if known.get(path).map(|(_, previous)| previous) != Some(stamp) {
                        if !is_archived_rollout(path)
                            && let Err(error) =
                                ensure_rollout_inventory(&executor, path, thread_id, stamp).await
                        {
                            tracing::warn!(path=%path.display(),error=?error,"cannot import changed Codex rollout inventory");
                        }
                        let cursor = cursors.remove(path).unwrap_or_default();
                        let signal = match advance_rollout_cursor(
                            path.clone(),
                            cursor,
                            stamp.length,
                        )
                        .await
                        {
                            Ok(cursor) => {
                                let signal = cursor.signal.clone();
                                cursors.insert(path.clone(), cursor);
                                signal
                            }
                            Err(error) => {
                                tracing::warn!(path=%path.display(),error=?error,"cannot read changed Codex rollout lifecycle");
                                None
                            }
                        };
                        retry
                            .entry(thread_id.clone())
                            .and_modify(|entry| entry.update_signal(signal.clone()))
                            .or_insert_with(|| RetryEntry::new(signal));
                    }
                }
                known = current;
            }
            Err(error) => tracing::warn!(error=?error,"Codex rollout monitor scan failed"),
        }

        // A failed read remains queued: a rollout can be observed between the
        // append and fsync/JSON completion, and must be retried without waiting
        // for another filesystem timestamp change.
        let ready = retry
            .iter()
            .filter(|(_, entry)| entry.next_attempt <= Instant::now())
            .take(8)
            .map(|(app_id, _)| app_id.clone())
            .collect::<Vec<_>>();
        for app_id in ready {
            let signal = retry.get(&app_id).and_then(|entry| entry.signal.clone());
            // Project the lifecycle before the expensive history hydration.
            // A large thread/read must never keep an actually running turn out
            // of the browser while its transcript is still reconciling.
            let runtime_result = if let Some(signal) = signal.as_ref() {
                apply_rollout_runtime(&executor, &app_id, signal).await
            } else {
                Ok(false)
            };
            let reconcile_result = executor.reconcile_app_thread(&app_id).await;
            if reconcile_result.is_ok() && runtime_result.is_ok() {
                retry.remove(&app_id);
                continue;
            }
            if let Err(error) = reconcile_result {
                tracing::warn!(%app_id,error=?error,"changed Codex rollout reconciliation failed; will retry");
            }
            if let Err(error) = runtime_result {
                tracing::warn!(%app_id,error=?error,"cannot apply Codex rollout runtime state; will retry");
            }
            if let Some(entry) = retry.get_mut(&app_id) {
                entry.back_off();
            }
        }

        // The state DB is a second source of truth and catches sources whose
        // rollout location differs from the conventional CODEX_HOME layout.
        if ticks == 1 || ticks.is_multiple_of(40) {
            match executor.reconcile_recent(false).await {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "recent Codex sessions reconciled")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(error=?error,"recent Codex reconciliation failed"),
            }
        }
        if ticks.is_multiple_of(80)
            && let Err(error) = executor.reconcile_recent(true).await
        {
            tracing::warn!(error=?error,"recent archived Codex reconciliation failed");
        }

        // Kimi keeps its durable history behind the local web service rather
        // than rollout files, so poll its session inventory at a lower rate.
        // This also picks up work started from another Kimi CLI on the device.
        if ticks == 1 || ticks.is_multiple_of(8) {
            match executor.reconcile_provider_recent(AgentProvider::Kimi, false).await {
                Ok(count) if count > 0 => tracing::info!(count, "recent Kimi sessions reconciled"),
                Ok(_) => {}
                Err(error) => tracing::warn!(error=?error,"recent Kimi reconciliation failed"),
            }
        }
        if ticks.is_multiple_of(160)
            && let Err(error) = executor.reconcile_provider_recent(AgentProvider::Kimi, true).await
        {
            tracing::warn!(error=?error,"recent archived Kimi reconciliation failed");
        }

        // Pi's durable history lives in `~/.pi/agent/sessions`; poll it at the
        // same rate to pick up sessions from external `pi` CLI processes.
        if ticks == 1 || ticks.is_multiple_of(8) {
            match executor.reconcile_provider_recent(AgentProvider::Pi, false).await {
                Ok(count) if count > 0 => tracing::info!(count, "recent Pi sessions reconciled"),
                Ok(_) => {}
                Err(error) => tracing::warn!(error=?error,"recent Pi reconciliation failed"),
            }
        }
        if (ticks == 1 || ticks.is_multiple_of(80))
            && let Err(error) = mark_stalled_rollouts(&executor).await
        {
            tracing::warn!(error=?error, "cannot mark stale Codex turns as stalled");
        }
    }
}

fn stamp_is_recent(stamp: &RolloutStamp, max_age: Duration) -> bool {
    let now_nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    now_nanos.saturating_sub(stamp.modified_nanos) <= max_age.as_nanos()
}

fn is_archived_rollout(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "archived_sessions")
}

async fn ensure_rollout_inventory(
    executor: &CommandExecutor,
    path: &Path,
    app_id: &str,
    stamp: &RolloutStamp,
) -> Result<bool> {
    if executor.store.local_thread_id(app_id).await?.is_some() {
        return Ok(false);
    }
    let path = path.to_owned();
    let app_id = app_id.to_owned();
    let stamp = *stamp;
    let inventory = tokio::task::spawn_blocking(move || {
        read_rollout_inventory_blocking(&path, &app_id, &stamp)
    })
    .await
    .context("rollout inventory reader stopped")??;
    let Some(inventory) = inventory else {
        return Ok(false);
    };
    executor
        .import_rollout_inventory(&serde_json::json!({
            "id": inventory.app_id,
            "cwd": inventory.cwd,
            "preview": inventory.title,
            "status": {"type":"notLoaded"},
            "createdAt": inventory.created_at,
            "updatedAt": inventory.updated_at_ms,
        }))
        .await
}

fn read_rollout_inventory_blocking(
    path: &Path,
    expected_app_id: &str,
    stamp: &RolloutStamp,
) -> std::io::Result<Option<RolloutInventory>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    let mut scanned = 0_usize;
    let mut inventory: Option<RolloutInventory> = None;
    while scanned < INVENTORY_SCAN_LIMIT {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        scanned = scanned.saturating_add(bytes);
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            // The writer may be between the JSON payload and its trailing
            // newline. Session metadata is already sufficient for inventory;
            // keep it and let the normal change watcher retry the title later.
            Err(_) if inventory.is_some() => break,
            Err(error) => {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
            }
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            let Some(app_id) = value.pointer("/payload/id").and_then(Value::as_str) else {
                continue;
            };
            if app_id != expected_app_id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "rollout filename and session metadata disagree",
                ));
            }
            let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) else {
                continue;
            };
            inventory = Some(RolloutInventory {
                app_id: app_id.to_owned(),
                cwd: cwd.to_owned(),
                created_at: value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                updated_at_ms: (stamp.modified_nanos / 1_000_000).min(i64::MAX as u128) as i64,
                title: None,
            });
            continue;
        }
        if value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some("user_message")
            && let Some(title) = value.pointer("/payload/message").and_then(Value::as_str)
        {
            if let Some(inventory) = inventory.as_mut() {
                inventory.title = Some(title.to_owned());
            }
            break;
        }
    }
    Ok(inventory)
}

async fn advance_rollout_cursor(
    path: PathBuf,
    cursor: RolloutCursor,
    observed_length: u64,
) -> std::io::Result<RolloutCursor> {
    tokio::task::spawn_blocking(move || {
        advance_rollout_cursor_blocking(&path, cursor, observed_length)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn advance_rollout_cursor_blocking(
    path: &Path,
    mut cursor: RolloutCursor,
    observed_length: u64,
) -> std::io::Result<RolloutCursor> {
    if cursor.offset > observed_length {
        cursor = RolloutCursor::default();
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended)?;
    let complete_length = appended
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    for line in appended[..complete_length].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_slice(line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        observe_rollout_value(&mut cursor.signal, &value);
    }
    cursor.offset += complete_length as u64;
    Ok(cursor)
}

fn observe_rollout_value(signal: &mut Option<RolloutRuntimeSignal>, value: &Value) {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return;
    }
    let Some(event_type) = value.pointer("/payload/type").and_then(Value::as_str) else {
        return;
    };
    let state = match event_type {
        "task_started" => RolloutRuntimeState::Active,
        "task_complete" => RolloutRuntimeState::Completed,
        "turn_aborted" => RolloutRuntimeState::Interrupted,
        _ => return,
    };
    let turn_id = value
        .pointer("/payload/turn_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| signal.as_ref().and_then(|current| current.turn_id.clone()));
    *signal = Some(RolloutRuntimeSignal {
        state,
        turn_id,
        occurred_at: value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
    });
}

async fn apply_rollout_runtime(
    executor: &CommandExecutor,
    app_id: &str,
    signal: &RolloutRuntimeSignal,
) -> Result<bool> {
    let Some(thread_id) = executor.store.local_thread_id(app_id).await? else {
        return Ok(false);
    };
    match signal.state {
        RolloutRuntimeState::Active => {
            executor
                .store
                .mark_app_turn_started(&thread_id, signal.turn_id.as_deref())
                .await?;
        }
        RolloutRuntimeState::Completed => {
            executor
                .store
                .complete_app_turn(&thread_id, signal.turn_id.as_deref(), "completed")
                .await?;
        }
        RolloutRuntimeState::Interrupted => {
            executor
                .store
                .complete_app_turn(&thread_id, signal.turn_id.as_deref(), "interrupted")
                .await?;
        }
    }
    publish_thread(executor, &thread_id).await?;
    Ok(true)
}

async fn publish_thread(executor: &CommandExecutor, thread_id: &str) -> Result<()> {
    executor.sync_thread(thread_id).await?;
    let thread = executor
        .store
        .thread(thread_id, &executor.device_id)
        .await?
        .context("rollout thread disappeared before publishing")?;
    executor
        .emit(
            "thread.summary",
            Some(&thread.project_id),
            Some(&thread.id),
            None,
            serde_json::to_value(&thread)?,
            true,
        )
        .await?;
    if let Some(project) = executor
        .store
        .project(&thread.project_id, &executor.device_id)
        .await?
    {
        executor
            .emit(
                "project.summary",
                Some(&project.summary.id),
                Some(&thread.id),
                None,
                serde_json::to_value(&project.summary)?,
                true,
            )
            .await?;
    }
    Ok(())
}

async fn mark_stalled_rollouts(executor: &CommandExecutor) -> Result<()> {
    let cutoff = (OffsetDateTime::now_utc() - STALLED_AFTER).format(&Rfc3339)?;
    for thread_id in executor.store.mark_stalled_codex_threads(&cutoff).await? {
        publish_thread(executor, &thread_id).await?;
    }
    Ok(())
}

fn codex_history_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex")));
    home.into_iter()
        .flat_map(|home| [home.join("sessions"), home.join("archived_sessions")])
        .collect()
}

async fn scan_roots(
    roots: Vec<PathBuf>,
) -> std::io::Result<HashMap<PathBuf, (String, RolloutStamp)>> {
    tokio::task::spawn_blocking(move || {
        let mut found = HashMap::new();
        for root in roots {
            scan_directory(&root, &mut found)?;
        }
        Ok(found)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn scan_directory(
    directory: &Path,
    found: &mut HashMap<PathBuf, (String, RolloutStamp)>,
) -> std::io::Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            scan_directory(&path, found)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(thread_id) = rollout_thread_id(&path) else {
            continue;
        };
        let metadata = entry.metadata()?;
        let modified_nanos = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        found.insert(
            path,
            (
                thread_id,
                RolloutStamp {
                    length: metadata.len(),
                    modified_nanos,
                },
            ),
        );
    }
    Ok(())
}

fn rollout_thread_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let candidate = stem.get(stem.len().checked_sub(36)?..)?;
    uuid::Uuid::parse_str(candidate)
        .ok()
        .map(|id| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rollout_uuid_only_from_codex_filename() {
        let path =
            Path::new("rollout-2026-07-19T09-00-00-019f75f6-b2f3-7d21-a5ae-4e7b76c83d09.jsonl");
        assert_eq!(
            rollout_thread_id(path).as_deref(),
            Some("019f75f6-b2f3-7d21-a5ae-4e7b76c83d09")
        );
        assert!(rollout_thread_id(Path::new("not-a-rollout.jsonl")).is_none());
    }

    #[test]
    fn rollout_lifecycle_is_authoritative_for_external_turns() {
        let mut signal = None;
        observe_rollout_value(
            &mut signal,
            &serde_json::json!({
                "timestamp":"2026-07-20T09:52:09Z",
                "type":"event_msg",
                "payload":{"type":"task_started","turn_id":"turn_live"}
            }),
        );
        assert_eq!(
            signal,
            Some(RolloutRuntimeSignal {
                state: RolloutRuntimeState::Active,
                turn_id: Some("turn_live".into()),
                occurred_at: Some("2026-07-20T09:52:09Z".into()),
            })
        );

        // Completion events do not consistently repeat the turn id. Preserve
        // the active lifecycle identity instead of completing an arbitrary row.
        observe_rollout_value(
            &mut signal,
            &serde_json::json!({
                "timestamp":"2026-07-20T09:54:00Z",
                "type":"event_msg",
                "payload":{"type":"task_complete"}
            }),
        );
        assert_eq!(
            signal,
            Some(RolloutRuntimeSignal {
                state: RolloutRuntimeState::Completed,
                turn_id: Some("turn_live".into()),
                occurred_at: Some("2026-07-20T09:54:00Z".into()),
            })
        );
    }

    #[test]
    fn unrelated_rollout_events_do_not_erase_active_state() {
        let active = RolloutRuntimeSignal {
            state: RolloutRuntimeState::Active,
            turn_id: Some("turn_live".into()),
            occurred_at: Some("2026-07-20T09:52:09Z".into()),
        };
        let mut signal = Some(active.clone());
        observe_rollout_value(
            &mut signal,
            &serde_json::json!({
                "timestamp":"2026-07-20T09:52:10Z",
                "type":"response_item",
                "payload":{"type":"reasoning"}
            }),
        );
        assert_eq!(signal, Some(active));
    }

    #[test]
    fn rollout_inventory_recovers_workspace_and_first_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let app_id = "019f4a0b-820e-7351-9958-7e43f8bc25ac";
        let path = temp
            .path()
            .join(format!("rollout-2026-07-10T11-21-36-{app_id}.jsonl"));
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-07-10T03:22:46.144Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"019f4a0b-820e-7351-9958-7e43f8bc25ac\",\"cwd\":\"/tmp/coworker\"}}\n",
                "{\"timestamp\":\"2026-07-10T03:22:50.944Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"介绍一下这个仓库\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\"}}\n"
            ),
        )
        .unwrap();
        let inventory = read_rollout_inventory_blocking(
            &path,
            app_id,
            &RolloutStamp {
                length: 512,
                modified_nanos: 1_783_653_696_000_000_000,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(inventory.app_id, app_id);
        assert_eq!(inventory.cwd, "/tmp/coworker");
        assert_eq!(
            inventory.created_at.as_deref(),
            Some("2026-07-10T03:22:46.144Z")
        );
        assert_eq!(inventory.updated_at_ms, 1_783_653_696_000);
        assert_eq!(inventory.title.as_deref(), Some("介绍一下这个仓库"));
        assert!(!is_archived_rollout(&path));
        assert!(is_archived_rollout(Path::new(
            "/tmp/.codex/archived_sessions/rollout.jsonl"
        )));
    }
}
