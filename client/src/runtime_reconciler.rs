use crate::{
    agent::AgentThreadState,
    executor::CommandExecutor,
    protocol::{AgentProvider, ThreadSummary, now},
};
use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use std::time::Duration;

const CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);
const PROVIDER_CALL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CANDIDATES: i64 = 256;
const MAX_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuditOutcome {
    Unchanged,
    RepairedActive,
    RepairedTerminal,
    ConflictingEvidence,
}

/// Periodically checks only suspicious durable runtime rows. This loop is
/// deliberately separate from provider event streams and history discovery:
/// it remains useful when a completion event was lost and no file changes are
/// left for the normal monitors to observe.
pub async fn run(executor: CommandExecutor) {
    let start = tokio::time::Instant::now() + CHECK_INTERVAL;
    let mut interval = tokio::time::interval_at(start, CHECK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!(
        interval_seconds = CHECK_INTERVAL.as_secs(),
        "runtime database reconciler started"
    );

    loop {
        interval.tick().await;
        if let Err(error) = audit_once(&executor).await {
            tracing::warn!(error=?error, "runtime database reconciliation cycle failed");
        }
    }
}

async fn audit_once(executor: &CommandExecutor) -> Result<()> {
    let candidates = executor
        .store
        .runtime_reconciliation_candidates(&executor.device_id, MAX_CANDIDATES)
        .await?;
    let inspected = candidates.len();
    if inspected == 0 {
        tracing::debug!("runtime database reconciliation found no suspicious rows");
        return Ok(());
    }

    let results = stream::iter(candidates.into_iter().map(|thread| {
        let executor = executor.clone();
        async move {
            let thread_id = thread.id.clone();
            (thread_id, audit_thread(&executor, thread).await)
        }
    }))
    .buffer_unordered(MAX_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut repaired = 0_usize;
    let mut conflicts = 0_usize;
    let mut failed = 0_usize;
    for (thread_id, result) in results {
        match result {
            Ok(AuditOutcome::RepairedActive) => {
                repaired += 1;
                tracing::info!(%thread_id, "re-aligned active conversation runtime state");
            }
            Ok(AuditOutcome::RepairedTerminal) => {
                repaired += 1;
                tracing::info!(%thread_id, "closed stale conversation runtime state");
            }
            Ok(AuditOutcome::ConflictingEvidence) => {
                conflicts += 1;
                tracing::warn!(%thread_id, "provider runtime sources disagree; state left unchanged");
            }
            Ok(AuditOutcome::Unchanged) => {}
            Err(error) => {
                failed += 1;
                tracing::warn!(%thread_id,error=?error, "conversation runtime self-check failed");
            }
        }
    }
    tracing::info!(
        inspected,
        repaired,
        conflicts,
        failed,
        "runtime database reconciliation completed"
    );
    Ok(())
}

async fn audit_thread(executor: &CommandExecutor, thread: ThreadSummary) -> Result<AuditOutcome> {
    let provider_thread_id = thread
        .app_server_thread_id
        .as_deref()
        .context("conversation has no provider thread id")?;
    // Every subsequent database repair is conditional on no newer local write
    // having landed after this provider observation began.
    let observed_before = now();
    let first = provider_state(executor, thread.provider, provider_thread_id).await?;
    if is_identified_active(thread.provider, &first) {
        return align_active(executor, &thread, &first, &observed_before).await;
    }
    if !is_terminal(&first.status) && !is_ambiguous_non_running(thread.provider, &first) {
        return Ok(AuditOutcome::Unchanged);
    }

    // A full snapshot is independent corroboration for ambiguous Codex states
    // such as `notLoaded` or `active` without an in-progress turn id.
    let snapshot = provider_snapshot(executor, thread.provider, provider_thread_id).await;
    let snapshot_has_active = snapshot.as_ref().is_some_and(snapshot_has_active_turn);
    let second = provider_state(executor, thread.provider, provider_thread_id).await?;
    if is_identified_active(thread.provider, &second) {
        return align_active(executor, &thread, &second, &observed_before).await;
    }
    if snapshot_has_active {
        return Ok(AuditOutcome::ConflictingEvidence);
    }

    let twice_terminal = is_terminal(&first.status) && is_terminal(&second.status);
    let corroborated_ambiguous = snapshot.is_some()
        && is_ambiguous_non_running(thread.provider, &first)
        && (is_terminal(&second.status) || is_ambiguous_non_running(thread.provider, &second));
    if !twice_terminal && !corroborated_ambiguous {
        return Ok(AuditOutcome::Unchanged);
    }

    let turn_status = snapshot
        .as_ref()
        .and_then(snapshot_terminal_turn_status)
        .unwrap_or("completed");
    if !executor
        .store
        .close_terminal_runtime(&thread.id, &observed_before, turn_status)
        .await?
    {
        return Ok(AuditOutcome::Unchanged);
    }
    publish_repaired_thread(executor, &thread.id).await?;
    Ok(AuditOutcome::RepairedTerminal)
}

async fn align_active(
    executor: &CommandExecutor,
    thread: &ThreadSummary,
    state: &AgentThreadState,
    observed_before: &str,
) -> Result<AuditOutcome> {
    if !executor
        .store
        .align_active_runtime(&thread.id, state.active_turn_id.as_deref(), observed_before)
        .await?
    {
        return Ok(AuditOutcome::Unchanged);
    }
    publish_repaired_thread(executor, &thread.id).await?;
    Ok(AuditOutcome::RepairedActive)
}

async fn provider_state(
    executor: &CommandExecutor,
    provider: AgentProvider,
    provider_thread_id: &str,
) -> Result<AgentThreadState> {
    tokio::time::timeout(
        PROVIDER_CALL_TIMEOUT,
        executor.agents.thread_state(provider, provider_thread_id),
    )
    .await
    .context("provider runtime-state check timed out")?
}

async fn provider_snapshot(
    executor: &CommandExecutor,
    provider: AgentProvider,
    provider_thread_id: &str,
) -> Option<Value> {
    match tokio::time::timeout(
        PROVIDER_CALL_TIMEOUT,
        executor.agents.read_thread(provider, provider_thread_id),
    )
    .await
    {
        Ok(Ok(snapshot)) => Some(snapshot),
        Ok(Err(error)) => {
            tracing::debug!(provider=provider.as_str(),%provider_thread_id,error=?error,"provider snapshot unavailable during runtime self-check");
            None
        }
        Err(_) => {
            tracing::debug!(provider=provider.as_str(),%provider_thread_id,"provider snapshot timed out during runtime self-check");
            None
        }
    }
}

async fn publish_repaired_thread(executor: &CommandExecutor, thread_id: &str) -> Result<()> {
    executor.sync_thread(thread_id).await?;
    let thread = executor
        .store
        .thread(thread_id, &executor.device_id)
        .await?
        .context("reconciled conversation disappeared before publishing")?;
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

fn is_identified_active(provider: AgentProvider, state: &AgentThreadState) -> bool {
    state.status.eq_ignore_ascii_case("active")
        && (provider == AgentProvider::Kimi || state.active_turn_id.is_some())
}

fn is_ambiguous_non_running(provider: AgentProvider, state: &AgentThreadState) -> bool {
    state.status.eq_ignore_ascii_case("notLoaded")
        || (provider == AgentProvider::Codex
            && state.status.eq_ignore_ascii_case("active")
            && state.active_turn_id.is_none())
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "idle" | "completed" | "failed" | "interrupted" | "cancelled" | "canceled"
    )
}

fn value_status(value: Option<&Value>) -> Option<&str> {
    value.and_then(|value| {
        value
            .as_str()
            .or_else(|| value.get("type").and_then(Value::as_str))
    })
}

fn snapshot_has_active_turn(snapshot: &Value) -> bool {
    snapshot
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|turn| {
            value_status(turn.get("status")).is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "active" | "running" | "inprogress"
                )
            })
        })
}

fn snapshot_terminal_turn_status(snapshot: &Value) -> Option<&'static str> {
    let status = snapshot
        .get("turns")
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .find_map(|turn| value_status(turn.get("status")))?;
    match status.to_ascii_lowercase().as_str() {
        "completed" | "idle" => Some("completed"),
        "failed" | "error" => Some("failed"),
        "interrupted" => Some("interrupted"),
        "cancelled" => Some("cancelled"),
        "canceled" => Some("canceled"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_requires_an_actual_in_progress_turn() {
        assert!(!snapshot_has_active_turn(&json!({
            "status":"active",
            "turns":[{"id":"turn_done","status":"completed"}]
        })));
        assert!(snapshot_has_active_turn(&json!({
            "status":"active",
            "turns":[{"id":"turn_live","status":{"type":"inProgress"}}]
        })));
    }

    #[test]
    fn snapshot_preserves_provider_terminal_reason() {
        assert_eq!(
            snapshot_terminal_turn_status(&json!({
                "turns":[{"status":"completed"},{"status":"interrupted"}]
            })),
            Some("interrupted")
        );
    }

    #[test]
    fn codex_active_without_turn_identity_requires_corroboration() {
        let codex = AgentThreadState {
            status: "active".into(),
            active_turn_id: None,
        };
        assert!(!is_identified_active(AgentProvider::Codex, &codex));
        assert!(is_ambiguous_non_running(AgentProvider::Codex, &codex));
        assert!(is_identified_active(AgentProvider::Kimi, &codex));
    }
}
