/* Cached `--version` probes for provider CLIs.

   `nuntius-client` runs as a launchd `ProcessType=Background` agent, so child
   processes inherit a heavily clamped CPU QoS: a Node-based `cli --version`
   that finishes in ~0.5s from an interactive terminal can take 5-10s here.
   Probing naively (3s timeout, no caching, abandoned children left running)
   made providers look unavailable while the timed-out probes kept spinning in
   the background. This helper gives probes a generous timeout, kills children
   whose probe is abandoned, and caches the outcome so health refreshes and
   the local console stay cheap. */

use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::process::Command;

/// How long a single probe may run before it is abandoned (and killed).
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Successful probes are reused for this long.
const SUCCESS_TTL: Duration = Duration::from_secs(300);
/// Failed probes are retried at most this often.
const FAILURE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct CachedProbe {
    finished_at: Instant,
    version: Option<String>,
}

fn cache() -> &'static tokio::sync::Mutex<HashMap<String, CachedProbe>> {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, CachedProbe>>> = OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Run `command args…` (typically `--version`) and return trimmed stdout when
/// the command exits successfully with non-empty output; `None` otherwise.
pub async fn command_version(command: &str, args: &[&str]) -> Option<String> {
    let key = format!("{command}\u{1f}{}", args.join("\u{1f}"));
    {
        let guard = cache().lock().await;
        if let Some(cached) = guard.get(&key) {
            let ttl = if cached.version.is_some() { SUCCESS_TTL } else { FAILURE_TTL };
            if cached.finished_at.elapsed() < ttl {
                return cached.version.clone();
            }
        }
    }
    let version = probe_once(command, args).await;
    cache().lock().await.insert(
        key,
        CachedProbe {
            finished_at: Instant::now(),
            version: version.clone(),
        },
    );
    version
}

async fn probe_once(command: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(command);
    // kill_on_drop ensures a probe abandoned via the timeout cannot keep
    // running (and burning throttled CPU) after the caller has moved on.
    child.args(args).kill_on_drop(true);
    tokio::time::timeout(PROBE_TIMEOUT, child.output())
        .await
        .ok()
        .and_then(|result| result.ok())
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_trimmed_stdout() {
        let marker = format!("nuntius-probe-{}", std::process::id());
        let version = command_version("/bin/echo", &[&marker]).await;
        assert_eq!(version.as_deref(), Some(marker.as_str()));
    }

    #[tokio::test]
    async fn missing_command_is_unavailable() {
        let version =
            command_version("/definitely/missing/nuntius-probe-command", &["--version"]).await;
        assert_eq!(version, None);
    }

    #[tokio::test]
    async fn failures_are_cached_and_retried_consistently() {
        let first = command_version("/definitely/missing/nuntius-probe-cached", &[]).await;
        let second = command_version("/definitely/missing/nuntius-probe-cached", &[]).await;
        assert_eq!(first, None);
        assert_eq!(second, None);
    }
}
