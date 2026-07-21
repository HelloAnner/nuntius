/* Cached provider command discovery and `--version` probes.

`nuntius-client` runs as a launchd `ProcessType=Background` agent. Two
details make ordinary PATH-based synchronous probes unreliable there:

- launchd keeps the PATH captured when the service was installed, while an
  NVM user may later move the `default` alias to another Node version;
- background QoS can make a Node-based `cli --version` take longer than the
  complete device-health deadline.

Bare commands therefore follow the current NVM default when the inherited
PATH points at an older NVM installation. Version probes run in the
background, cache their result, and accept successful CLIs that print their
version on either stdout or stderr. Command presence, not version output,
is the availability signal used by providers. */

use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
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

#[derive(Clone, Default)]
struct CachedProbe {
    finished_at: Option<Instant>,
    version: Option<String>,
    in_flight: bool,
}

fn cache() -> &'static tokio::sync::Mutex<HashMap<String, CachedProbe>> {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, CachedProbe>>> = OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Resolve a configured provider command without pinning it to the PATH that
/// launchd captured at service installation time.
///
/// Explicit paths and non-NVM PATH shims retain normal precedence. If PATH
/// resolves into `~/.nvm/versions/node/...` and the current NVM `default`
/// version contains the same command, the current default wins.
pub fn resolve_executable(command: &str) -> Option<PathBuf> {
    let configured = Path::new(command);
    if is_explicit_path(configured) {
        return executable_file(configured).then(|| configured.to_path_buf());
    }

    let path_match = env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .find_map(|directory| executable_in(&directory, command));
    let nvm_root = nvm_root();
    let nvm_default = nvm_root
        .as_deref()
        .and_then(|root| resolve_nvm_default_executable(root, command));

    match (path_match, nvm_default, nvm_root) {
        (Some(path), Some(default), Some(root)) if path.starts_with(root.join("versions/node")) => {
            Some(default)
        }
        (Some(path), _, _) => Some(path),
        (None, default, _) => default,
    }
}

/// Return the executable that should be spawned, preserving the configured
/// value so `Command::spawn` can produce the authoritative error when command
/// discovery did not find it.
pub fn resolve_command(command: &str) -> PathBuf {
    resolve_executable(command).unwrap_or_else(|| PathBuf::from(command))
}

/// Build a Tokio command with the selected NVM version's `bin` directory at
/// the front of PATH. NPM launchers commonly use `#!/usr/bin/env node`; merely
/// resolving the launcher itself to a new NVM directory would otherwise still
/// execute it with launchd's old Node binary.
pub fn provider_command(command: &str) -> Command {
    command_for_executable(resolve_command(command))
}

pub fn command_available(command: &str) -> bool {
    resolve_executable(command).is_some()
}

/// Return a cached CLI version immediately and schedule a refresh when the
/// cache is absent or stale. A slow version command never blocks provider
/// availability or the device heartbeat.
pub async fn command_version(command: &str, args: &[&str]) -> Option<String> {
    let executable = resolve_executable(command)?;
    let owned_args = args
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect::<Vec<_>>();
    let key = format!(
        "{}\u{1f}{}",
        executable.to_string_lossy(),
        owned_args.join("\u{1f}")
    );

    let stale_version = {
        let mut guard = cache().lock().await;
        let cached = guard.entry(key.clone()).or_default();
        if let Some(finished_at) = cached.finished_at {
            let ttl = if cached.version.is_some() {
                SUCCESS_TTL
            } else {
                FAILURE_TTL
            };
            if finished_at.elapsed() < ttl {
                return cached.version.clone();
            }
        }
        let stale = cached.version.clone();
        if cached.in_flight {
            return stale;
        }
        cached.in_flight = true;
        stale
    };

    tokio::spawn(async move {
        let version = probe_once(&executable, &owned_args).await;
        let mut guard = cache().lock().await;
        let cached = guard.entry(key).or_default();
        cached.finished_at = Some(Instant::now());
        cached.version = version;
        cached.in_flight = false;
    });

    stale_version
}

async fn probe_once(command: &Path, args: &[String]) -> Option<String> {
    let mut child = command_for_executable(command.to_path_buf());
    // kill_on_drop ensures a probe abandoned via the timeout cannot keep
    // running (and burning throttled CPU) after the caller has moved on.
    child.args(args).kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, child.output())
        .await
        .ok()
        .and_then(|result| result.ok())
        .filter(|output| output.status.success())?;
    non_empty_utf8(&output.stdout).or_else(|| non_empty_utf8(&output.stderr))
}

fn non_empty_utf8(bytes: &[u8]) -> Option<String> {
    String::from_utf8(bytes.to_vec())
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn command_for_executable(executable: PathBuf) -> Command {
    let mut command = Command::new(&executable);
    if let (Some(root), Some(parent)) = (nvm_root(), executable.parent())
        && executable.starts_with(root.join("versions/node"))
    {
        let inherited = env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(parent.to_path_buf()).chain(env::split_paths(&inherited));
        if let Ok(path) = env::join_paths(paths) {
            command.env("PATH", path);
        }
        command.env("NVM_BIN", parent);
    }
    command
}

fn is_explicit_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
}

fn executable_in(directory: &Path, command: &str) -> Option<PathBuf> {
    let direct = directory.join(command);
    if executable_file(&direct) {
        return Some(direct);
    }
    #[cfg(windows)]
    {
        if Path::new(command).extension().is_none() {
            let extensions = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
            for extension in extensions.split(';').filter(|value| !value.is_empty()) {
                let candidate = directory.join(format!("{command}{extension}"));
                if executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn nvm_root() -> Option<PathBuf> {
    env::var_os("NVM_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".nvm")))
        .filter(|path| path.is_dir())
}

fn resolve_nvm_default_executable(nvm_root: &Path, command: &str) -> Option<PathBuf> {
    let selector = std::fs::read_to_string(nvm_root.join("alias/default")).ok()?;
    resolve_nvm_selector_executable(nvm_root, selector.trim(), command, 0)
}

fn resolve_nvm_selector_executable(
    nvm_root: &Path,
    selector: &str,
    command: &str,
    depth: usize,
) -> Option<PathBuf> {
    if selector.is_empty() || depth > 8 {
        return None;
    }
    let normalized = selector.trim_start_matches('v');
    let exact_dir = nvm_root.join("versions/node").join(normalized);
    if exact_dir.is_dir()
        && let Some(executable) = executable_in(&exact_dir.join("bin"), command)
    {
        return Some(executable);
    }

    if selector
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
        && let Ok(alias) = std::fs::read_to_string(nvm_root.join("alias").join(selector))
        && let Some(executable) =
            resolve_nvm_selector_executable(nvm_root, alias.trim(), command, depth + 1)
    {
        return Some(executable);
    }

    let versions_root = nvm_root.join("versions/node");
    let mut candidates = std::fs::read_dir(versions_root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry
                .file_name()
                .to_string_lossy()
                .trim_start_matches('v')
                .to_owned();
            let version = numeric_version(&name)?;
            selector_matches_version(selector, &version).then_some((version, entry.path()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates
        .into_iter()
        .find_map(|(_, directory)| executable_in(&directory.join("bin"), command))
}

fn numeric_version(value: &str) -> Option<Vec<u64>> {
    let version = value
        .trim_start_matches('v')
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!version.is_empty()).then_some(version)
}

fn selector_matches_version(selector: &str, version: &[u64]) -> bool {
    let selector = selector.trim_start_matches('v');
    if matches!(selector, "node" | "stable" | "current" | "lts/*") {
        return true;
    }
    let Some(prefix) = numeric_version(selector) else {
        return false;
    };
    version.starts_with(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    async fn wait_for_version(command: &str, args: &[&str]) -> Option<String> {
        for _ in 0..100 {
            if let Some(version) = command_version(command, args).await {
                return Some(version);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    #[tokio::test]
    async fn captures_trimmed_stdout() {
        let marker = format!("nuntius-probe-stdout-{}", std::process::id());
        let version = wait_for_version("/bin/echo", &[&marker]).await;
        assert_eq!(version.as_deref(), Some(marker.as_str()));
    }

    #[tokio::test]
    async fn falls_back_to_trimmed_stderr() {
        let marker = format!("nuntius-probe-stderr-{}", std::process::id());
        let script = format!("printf '  {marker}\\n' >&2");
        let version = wait_for_version("/bin/sh", &["-c", &script]).await;
        assert_eq!(version.as_deref(), Some(marker.as_str()));
    }

    #[tokio::test]
    async fn missing_command_is_unavailable() {
        let command = "/definitely/missing/nuntius-probe-command";
        assert!(!command_available(command));
        assert_eq!(command_version(command, &["--version"]).await, None);
    }

    #[test]
    fn nvm_default_follows_version_changes_instead_of_a_captured_path() {
        let root = TempDir::new().unwrap();
        let nvm = root.path();
        fs::create_dir_all(nvm.join("alias")).unwrap();
        for version in ["22.14.0", "22.23.1"] {
            let bin = nvm.join("versions/node").join(version).join("bin");
            fs::create_dir_all(&bin).unwrap();
            let executable = bin.join("pi");
            fs::write(&executable, "#!/bin/sh\n").unwrap();
            make_executable(&executable);
        }

        fs::write(nvm.join("alias/default"), "22.23.1\n").unwrap();
        assert_eq!(
            resolve_nvm_default_executable(nvm, "pi"),
            Some(nvm.join("versions/node/22.23.1/bin/pi"))
        );

        fs::write(nvm.join("alias/default"), "22.14\n").unwrap();
        assert_eq!(
            resolve_nvm_default_executable(nvm, "pi"),
            Some(nvm.join("versions/node/22.14.0/bin/pi"))
        );
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }
}
