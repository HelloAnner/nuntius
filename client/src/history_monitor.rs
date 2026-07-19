use crate::executor::CommandExecutor;
use directories::BaseDirs;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RolloutStamp {
    length: u64,
    modified_nanos: u128,
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
    let mut retry = HashSet::new();
    let mut interval = tokio::time::interval(Duration::from_millis(750));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ticks = 0_u64;

    loop {
        interval.tick().await;
        ticks += 1;
        match scan_roots(roots.clone()).await {
            Ok(current) => {
                for (path, (thread_id, stamp)) in &current {
                    if known.get(path).map(|(_, previous)| previous) != Some(stamp) {
                        retry.insert(thread_id.clone());
                    }
                }
                known = current;
            }
            Err(error) => tracing::warn!(error=?error,"Codex rollout monitor scan failed"),
        }

        // A failed read remains queued: a rollout can be observed between the
        // append and fsync/JSON completion, and must be retried without waiting
        // for another filesystem timestamp change.
        for app_id in retry.iter().take(8).cloned().collect::<Vec<_>>() {
            match executor.reconcile_app_thread(&app_id).await {
                Ok(()) => {
                    retry.remove(&app_id);
                }
                Err(error) => {
                    tracing::warn!(%app_id,error=?error,"changed Codex rollout reconciliation failed; will retry");
                }
            }
        }

        // The state DB is a second source of truth and catches sources whose
        // rollout location differs from the conventional CODEX_HOME layout.
        if ticks == 1 || ticks.is_multiple_of(3) {
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
    }
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
}
