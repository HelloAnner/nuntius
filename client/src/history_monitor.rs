use crate::executor::CommandExecutor;
use anyhow::{Context, Result};
use directories::BaseDirs;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const RECENT_ROLLOUT_WINDOW: Duration = Duration::from_secs(30 * 60);
const STALLED_AFTER: TimeDuration = TimeDuration::minutes(30);

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
    // Seed only recent rollouts. This restores already-running terminal/IDE
    // turns immediately after a client restart without re-reading the device's
    // entire Codex history inventory.
    for (path, (thread_id, stamp)) in &known {
        if !stamp_is_recent(stamp, RECENT_ROLLOUT_WINDOW) {
            continue;
        }
        match advance_rollout_cursor(path.clone(), RolloutCursor::default(), stamp.length).await {
            Ok(cursor) => {
                let signal = cursor.signal.clone();
                cursors.insert(path.clone(), cursor);
                retry.insert(thread_id.clone(), RetryEntry::new(signal));
            }
            Err(error) => {
                tracing::warn!(path=%path.display(),error=?error,"cannot seed recent Codex rollout lifecycle");
            }
        }
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
            let reconcile_result = executor.reconcile_app_thread(&app_id).await;
            let runtime_result = if let Some(signal) = signal.as_ref() {
                apply_rollout_runtime(&executor, &app_id, signal).await
            } else {
                Ok(false)
            };
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
            match executor.reconcile_kimi_recent(false).await {
                Ok(count) if count > 0 => tracing::info!(count, "recent Kimi sessions reconciled"),
                Ok(_) => {}
                Err(error) => tracing::warn!(error=?error,"recent Kimi reconciliation failed"),
            }
        }
        if ticks.is_multiple_of(160)
            && let Err(error) = executor.reconcile_kimi_recent(true).await
        {
            tracing::warn!(error=?error,"recent archived Kimi reconciliation failed");
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
}
