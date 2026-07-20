use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    process::Command,
    sync::{Mutex, OwnedMutexGuard, mpsc, watch},
};

pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;
static STAGE_UPDATE_LOCK: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

pub fn build_sha() -> &'static str {
    option_env!("NUNTIUS_BUILD_SHA").unwrap_or("development")
}

pub fn build_sequence() -> u64 {
    option_env!("NUNTIUS_BUILD_SEQUENCE")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn release_is_stale(installed_sequence: u64, available_sequence: u64) -> bool {
    installed_sequence > 0 && available_sequence <= installed_sequence
}

#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub binary_name: String,
    pub expected_target: String,
    pub data_dir: PathBuf,
    pub retry_interval: Duration,
    pub trusted_server_url: String,
}

impl UpdateConfig {
    pub fn client(
        binary_name: impl Into<String>,
        expected_target: impl Into<String>,
        data_dir: PathBuf,
        retry_interval: Duration,
        trusted_server_url: impl Into<String>,
    ) -> Self {
        Self {
            binary_name: binary_name.into(),
            expected_target: expected_target.into(),
            data_dir,
            retry_interval,
            trusted_server_url: trusted_server_url.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub name: String,
    pub version: String,
    pub build_sha: String,
    #[serde(default)]
    pub release_sequence: u64,
    pub target: String,
}

impl BuildInfo {
    pub fn current(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            build_sha: build_sha().into(),
            release_sequence: build_sequence(),
            target: build_target(),
        }
    }
}

pub fn build_target() -> String {
    option_env!("NUNTIUS_BUILD_TARGET")
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientRelease {
    pub release_id: String,
    pub commit_sha: String,
    pub release_sequence: u64,
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug)]
pub struct PreparedUpdate {
    _stage_guard: OwnedMutexGuard<()>,
    staged_path: PathBuf,
    executable_path: PathBuf,
    previous_path: PathBuf,
    marker_path: PathBuf,
    from_sha: String,
    to_sha: String,
    release_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MarkerPhase {
    Installed,
    Booting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMarker {
    phase: MarkerPhase,
    from_sha: String,
    to_sha: String,
    #[serde(default)]
    release_sequence: u64,
    executable_path: PathBuf,
    previous_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RejectedRelease {
    commit_sha: String,
    #[serde(default)]
    release_sequence: u64,
}

pub fn spawn_client_update_worker(
    config: UpdateConfig,
    mut desired: watch::Receiver<Option<ClientRelease>>,
    ready: mpsc::Sender<PreparedUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = match http_client() {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(error=?error, "cannot initialize self-updater HTTP client");
                return;
            }
        };

        loop {
            let release = desired.borrow().clone();
            let Some(release) = release else {
                if desired.changed().await.is_err() {
                    return;
                }
                continue;
            };
            match prepare_client_update(&client, &config, &release).await {
                Ok(Some(update)) => {
                    tracing::info!(from=%update.from_sha, to=%update.to_sha, "self-update prepared");
                    if ready.send(update).await.is_err() {
                        tracing::warn!("self-update receiver closed");
                    }
                    return;
                }
                Ok(None) => {
                    if desired.changed().await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(error=?error,release_id=%release.release_id,"client update failed; retrying");
                    tokio::select! {
                        _ = tokio::time::sleep(config.retry_interval) => {}
                        changed = desired.changed() => if changed.is_err() { return; },
                    }
                }
            }
        }
    })
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("nuntius-updater/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?)
}

async fn prepare_client_update(
    client: &reqwest::Client,
    config: &UpdateConfig,
    release: &ClientRelease,
) -> Result<Option<PreparedUpdate>> {
    validate_client_release(config, release)?;
    if release.commit_sha == build_sha() {
        return Ok(None);
    }
    if rejected_release(&config.data_dir)?
        .is_some_and(|rejected| rejected.commit_sha == release.commit_sha)
    {
        tracing::warn!(
            release_id = %release.release_id,
            commit_sha = %release.commit_sha,
            "ignoring client release that previously failed its startup probation"
        );
        return Ok(None);
    }
    if release_is_stale(build_sequence(), release.release_sequence) {
        tracing::warn!(
            installed_sequence = build_sequence(),
            available_sequence = release.release_sequence,
            available = %release.commit_sha,
            "ignoring stale client release"
        );
        return Ok(None);
    }
    let archive = download_archive(client, release).await?;
    Ok(Some(
        stage_update(
            config,
            &release.commit_sha,
            release.release_sequence,
            &archive,
        )
        .await?,
    ))
}

async fn download_archive(client: &reqwest::Client, release: &ClientRelease) -> Result<Vec<u8>> {
    let response = client
        .get(&release.url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .context("download update archive")?
        .error_for_status()
        .context("update archive returned an error")?;
    if response.content_length().unwrap_or(0) > MAX_ARCHIVE_BYTES as u64 {
        bail!("update archive exceeds size limit");
    }
    let archive = response
        .bytes()
        .await
        .context("read update archive")?
        .to_vec();
    if archive.len() as u64 != release.size {
        bail!("update archive size does not match release metadata");
    }
    verify_archive(&archive, &release.sha256)?;
    Ok(archive)
}

fn verify_archive(archive: &[u8], expected_digest: &str) -> Result<()> {
    if archive.is_empty() || archive.len() > MAX_ARCHIVE_BYTES {
        bail!("update archive has an invalid size");
    }
    let actual_digest = hex::encode(Sha256::digest(archive));
    if actual_digest != expected_digest {
        bail!(
            "update archive checksum mismatch: expected {}, got {}",
            expected_digest,
            actual_digest
        );
    }
    Ok(())
}

async fn stage_update(
    config: &UpdateConfig,
    commit_sha: &str,
    release_sequence: u64,
    archive: &[u8],
) -> Result<PreparedUpdate> {
    let stage_guard = STAGE_UPDATE_LOCK.clone().lock_owned().await;
    let binary = extract_binary(&archive, &config.binary_name)?;
    let executable_path = std::env::current_exe().context("resolve current executable")?;
    let directory = executable_path
        .parent()
        .context("current executable has no parent directory")?;
    let staged_path = directory.join(format!(".{}.update-{}", config.binary_name, commit_sha));
    write_executable(&staged_path, &binary)?;
    probe_binary(
        &staged_path,
        &config.binary_name,
        commit_sha,
        release_sequence,
        &config.expected_target,
    )
    .await
    .inspect_err(|_| {
        let _ = fs::remove_file(&staged_path);
    })?;

    let previous_path = directory.join(format!("{}.previous", config.binary_name));
    Ok(PreparedUpdate {
        _stage_guard: stage_guard,
        staged_path,
        executable_path,
        previous_path,
        marker_path: marker_path(&config.data_dir),
        from_sha: build_sha().into(),
        to_sha: commit_sha.to_owned(),
        release_sequence,
    })
}

fn validate_client_release(config: &UpdateConfig, release: &ClientRelease) -> Result<()> {
    validate_commit_sha(&release.commit_sha)?;
    validate_digest(&release.sha256)?;
    if release.release_id.is_empty()
        || release.release_id.len() > 128
        || !release
            .release_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        || release.release_sequence == 0
        || release.size == 0
        || release.size > MAX_ARCHIVE_BYTES as u64
    {
        bail!("invalid client release metadata");
    }
    if release.target != config.expected_target {
        bail!(
            "update target mismatch: expected {}, got {}",
            config.expected_target,
            release.target
        );
    }
    let base =
        reqwest::Url::parse(&config.trusted_server_url).context("parse trusted server URL")?;
    let url = reqwest::Url::parse(&release.url).context("parse client release URL")?;
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || !url.path().starts_with("/api/v1/client-releases/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("client release URL is outside the paired server origin");
    }
    Ok(())
}

fn validate_commit_sha(value: &str) -> Result<()> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid update commit SHA");
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid update archive SHA-256");
    }
    Ok(())
}

fn extract_binary(archive: &[u8], binary_name: &str) -> Result<Vec<u8>> {
    let decoder = GzDecoder::new(Cursor::new(archive));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("read update tar entries")? {
        let mut entry = entry.context("read update tar entry")?;
        let path = entry.path().context("read update tar path")?;
        if path.file_name().and_then(|name| name.to_str()) != Some(binary_name) {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            bail!("update binary tar entry is not a regular file");
        }
        let mut binary = Vec::new();
        entry
            .by_ref()
            .take(MAX_BINARY_BYTES + 1)
            .read_to_end(&mut binary)
            .context("extract update binary")?;
        if binary.is_empty() || binary.len() as u64 > MAX_BINARY_BYTES {
            bail!("update binary has an invalid size");
        }
        return Ok(binary);
    }
    bail!("update archive does not contain {binary_name}")
}

fn write_executable(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(contents)?;
    file.sync_all()?;
    set_executable(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

async fn probe_binary(
    path: &Path,
    expected_name: &str,
    expected_sha: &str,
    expected_sequence: u64,
    expected_target: &str,
) -> Result<()> {
    let output = Command::new(path)
        .arg("build-info")
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("run update probe {}", path.display()))?;
    if !output.status.success() {
        bail!("updated binary failed its build-info probe");
    }
    let info: BuildInfo = serde_json::from_slice(&output.stdout)
        .context("decode updated binary build information")?;
    if info.name != expected_name
        || info.build_sha != expected_sha
        || (expected_sequence > 0 && info.release_sequence != expected_sequence)
        || info.target != expected_target
    {
        bail!(
            "updated binary identity mismatch: name={}, sha={}, sequence={}, target={}",
            info.name,
            info.build_sha,
            info.release_sequence,
            info.target,
        );
    }
    Ok(())
}

impl PreparedUpdate {
    #[cfg(unix)]
    pub fn activate(self) -> Result<()> {
        use std::os::unix::process::CommandExt;

        let previous_temporary = self.previous_path.with_extension("previous.tmp");
        copy_synced(&self.executable_path, &previous_temporary)?;
        replace_file(&previous_temporary, &self.previous_path)?;
        let marker = UpdateMarker {
            phase: MarkerPhase::Installed,
            from_sha: self.from_sha.clone(),
            to_sha: self.to_sha.clone(),
            release_sequence: self.release_sequence,
            executable_path: self.executable_path.clone(),
            previous_path: self.previous_path.clone(),
        };
        write_marker(&self.marker_path, &marker)?;
        replace_file(&self.staged_path, &self.executable_path)?;

        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        let error = std::process::Command::new(&self.executable_path)
            .args(args)
            .exec();
        if let Err(rejection_error) = record_rejected_release(&marker, &self.marker_path) {
            tracing::warn!(error=?rejection_error, build=%marker.to_sha, "cannot quarantine failed client release");
        }
        let rollback = restore_previous(&marker, &self.marker_path);
        if let Err(rollback_error) = rollback {
            return Err(anyhow::anyhow!(error)).context(format!(
                "self-update exec failed and rollback also failed: {rollback_error:#}"
            ));
        }
        Err(anyhow::anyhow!(error)).context("restart into updated binary")
    }

    #[cfg(not(unix))]
    pub fn activate(self) -> Result<()> {
        let _ = self;
        bail!("self-update activation is currently supported only on Unix")
    }

    pub fn target_sha(&self) -> &str {
        &self.to_sha
    }
}

pub fn handle_startup(data_dir: &Path) -> Result<()> {
    let path = marker_path(data_dir);
    let Some(mut marker) = read_marker(&path)? else {
        return Ok(());
    };
    let current = build_sha();
    if current == marker.from_sha {
        remove_marker(&path)?;
        return Ok(());
    }
    if current != marker.to_sha {
        bail!(
            "self-update marker expects {}, but running build is {}",
            marker.to_sha,
            current
        );
    }
    match marker.phase {
        MarkerPhase::Installed => {
            marker.phase = MarkerPhase::Booting;
            write_marker(&path, &marker)?;
            Ok(())
        }
        MarkerPhase::Booting => rollback_and_restart(&marker, &path),
    }
}

pub fn mark_healthy(data_dir: &Path) -> Result<()> {
    let path = marker_path(data_dir);
    let Some(marker) = read_marker(&path)? else {
        return Ok(());
    };
    if marker.to_sha == build_sha() && marker.phase == MarkerPhase::Booting {
        remove_marker(&path)?;
        tracing::info!(build=%marker.to_sha, "self-update marked healthy");
    }
    Ok(())
}

pub fn startup_update_pending(data_dir: &Path) -> Result<bool> {
    Ok(read_marker(&marker_path(data_dir))?.is_some())
}

#[cfg(unix)]
fn rollback_and_restart(marker: &UpdateMarker, marker_path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;

    if let Err(error) = record_rejected_release(marker, marker_path) {
        tracing::warn!(error=?error, build=%marker.to_sha, "cannot quarantine failed client release");
    }
    restore_previous(marker, marker_path)?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let error = std::process::Command::new(&marker.executable_path)
        .args(args)
        .exec();
    Err(anyhow::anyhow!(error)).context("restart into previous binary after failed update")
}

#[cfg(not(unix))]
fn rollback_and_restart(_marker: &UpdateMarker, _marker_path: &Path) -> Result<()> {
    bail!("automatic rollback is currently supported only on Unix")
}

fn restore_previous(marker: &UpdateMarker, marker_path: &Path) -> Result<()> {
    let rollback_temporary = marker.executable_path.with_extension("rollback.tmp");
    copy_synced(&marker.previous_path, &rollback_temporary)?;
    replace_file(&rollback_temporary, &marker.executable_path)?;
    remove_marker(marker_path)?;
    Ok(())
}

fn copy_synced(source: &Path, destination: &Path) -> Result<()> {
    let mut source_file =
        File::open(source).with_context(|| format!("open {}", source.display()))?;
    let mut destination_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(destination)
        .with_context(|| format!("create {}", destination.display()))?;
    std::io::copy(&mut source_file, &mut destination_file)?;
    destination_file.sync_all()?;
    set_executable(destination)?;
    Ok(())
}

fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(not(unix))]
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(source, destination).with_context(|| {
        format!(
            "replace {} with {}",
            destination.display(),
            source.display()
        )
    })?;
    Ok(())
}

fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join("run/self-update.json")
}

fn rejected_release_path(data_dir: &Path) -> PathBuf {
    data_dir.join("run/rejected-client-release.json")
}

fn record_rejected_release(marker: &UpdateMarker, marker_path: &Path) -> Result<()> {
    let data_dir = marker_path
        .parent()
        .and_then(Path::parent)
        .context("self-update marker is outside the client data directory")?;
    write_rejected_release(
        &rejected_release_path(data_dir),
        &RejectedRelease {
            commit_sha: marker.to_sha.clone(),
            release_sequence: marker.release_sequence,
        },
    )
}

fn write_rejected_release(path: &Path, rejected: &RejectedRelease) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(rejected)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    set_private(&temporary)?;
    replace_file(&temporary, path)?;
    Ok(())
}

fn rejected_release(data_dir: &Path) -> Result<Option<RejectedRelease>> {
    let path = rejected_release_path(data_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("decode rejected client release")?,
    ))
}

fn write_marker(path: &Path, marker: &UpdateMarker) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(marker)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    set_private(&temporary)?;
    replace_file(&temporary, path)?;
    Ok(())
}

fn read_marker(path: &Path) -> Result<Option<UpdateMarker>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("decode self-update marker")?,
    ))
}

fn remove_marker(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn set_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use tempfile::tempdir;

    #[test]
    fn release_sequence_never_moves_backwards_after_migration() {
        assert!(!release_is_stale(0, 0));
        assert!(!release_is_stale(0, 10));
        assert!(!release_is_stale(10, 11));
        assert!(release_is_stale(10, 10));
        assert!(release_is_stale(10, 9));
    }

    #[test]
    fn rejected_release_is_persisted_for_future_update_checks() {
        let root = tempdir().unwrap();
        let marker_path = marker_path(root.path());
        let marker = UpdateMarker {
            phase: MarkerPhase::Booting,
            from_sha: "a".repeat(40),
            to_sha: "b".repeat(40),
            release_sequence: 42,
            executable_path: root.path().join("nuntius-client"),
            previous_path: root.path().join("nuntius-client.previous"),
        };

        record_rejected_release(&marker, &marker_path).unwrap();

        let rejected = rejected_release(root.path()).unwrap().unwrap();
        assert_eq!(rejected.commit_sha, "b".repeat(40));
        assert_eq!(rejected.release_sequence, 42);
    }

    #[test]
    fn old_update_markers_default_the_release_sequence() {
        let marker: UpdateMarker = serde_json::from_value(serde_json::json!({
            "phase": "booting",
            "fromSha": "a",
            "toSha": "b",
            "executablePath": "/tmp/nuntius-client",
            "previousPath": "/tmp/nuntius-client.previous"
        }))
        .unwrap();

        assert_eq!(marker.release_sequence, 0);
    }

    #[test]
    fn extracts_named_binary_from_tar_gzip() {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let bytes = b"binary-contents";
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "nuntius-server", &bytes[..])
            .unwrap();
        let encoder = builder.into_inner().unwrap();
        let archive = encoder.finish().unwrap();

        assert_eq!(extract_binary(&archive, "nuntius-server").unwrap(), bytes);
    }

    #[test]
    fn rejects_client_release_outside_paired_server() {
        let config = UpdateConfig::client(
            "nuntius-client",
            "aarch64-apple-darwin",
            PathBuf::from("/tmp/nuntius-updater-test"),
            Duration::from_secs(60),
            "https://nuntius.example.com/",
        );
        let release = ClientRelease {
            release_id: "1-aaaaaaaaaaaa".into(),
            commit_sha: "a".repeat(40),
            release_sequence: 1,
            target: "aarch64-apple-darwin".into(),
            url: "https://evil.example.com/api/v1/client-releases/1/client.tar.gz".into(),
            sha256: "b".repeat(64),
            size: 1024,
        };
        assert!(validate_client_release(&config, &release).is_err());
    }

    #[test]
    fn accepts_client_release_from_paired_server() {
        let config = UpdateConfig::client(
            "nuntius-client",
            "aarch64-apple-darwin",
            PathBuf::from("/tmp/nuntius-updater-test"),
            Duration::from_secs(60),
            "http://127.0.0.1:8080/",
        );
        let release = ClientRelease {
            release_id: "1784512000000-aaaaaaaaaaaa".into(),
            commit_sha: "a".repeat(40),
            release_sequence: 1_784_512_000_000,
            target: "aarch64-apple-darwin".into(),
            url: "http://127.0.0.1:8080/api/v1/client-releases/1784512000000-aaaaaaaaaaaa/nuntius-client-macos-arm64.tar.gz".into(),
            sha256: "b".repeat(64),
            size: 1024,
        };
        assert!(validate_client_release(&config, &release).is_ok());
    }
}
