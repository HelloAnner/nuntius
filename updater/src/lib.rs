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
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::Command,
    sync::{Mutex, Notify, OwnedMutexGuard, mpsc},
};

pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/HelloAnner/nuntius/releases/download/continuous/manifest.json";
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRole {
    Server,
    Client,
}

#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub role: UpdateRole,
    pub binary_name: String,
    pub expected_target: String,
    pub data_dir: PathBuf,
    pub interval: Duration,
    pub manifest_url: String,
    pub required_server_info_url: Option<String>,
}

impl UpdateConfig {
    pub fn production(
        role: UpdateRole,
        binary_name: impl Into<String>,
        expected_target: impl Into<String>,
        data_dir: PathBuf,
        interval: Duration,
    ) -> Self {
        Self {
            role,
            binary_name: binary_name.into(),
            expected_target: expected_target.into(),
            data_dir,
            interval,
            manifest_url: std::env::var("NUNTIUS_UPDATE_MANIFEST_URL")
                .unwrap_or_else(|_| DEFAULT_MANIFEST_URL.into()),
            required_server_info_url: None,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateManifest {
    schema_version: u32,
    commit_sha: String,
    #[serde(default)]
    release_sequence: u64,
    server: ManifestAsset,
    client: ManifestAsset,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestAsset {
    url: String,
    sha256: String,
    target: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerBuildInfo {
    build_sha: String,
    #[serde(default)]
    release_sequence: u64,
}

#[derive(Debug)]
pub struct RelayPackage {
    pub commit_sha: String,
    pub release_sequence: u64,
    pub archive_sha256: String,
    pub archive: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct UpdateTrigger(Arc<Notify>);

impl UpdateTrigger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify(&self) {
        self.0.notify_waiters();
    }

    pub async fn wait(&self) {
        self.0.notified().await;
    }
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
    executable_path: PathBuf,
    previous_path: PathBuf,
}

pub fn spawn_update_loop(
    config: UpdateConfig,
    ready: mpsc::Sender<PreparedUpdate>,
) -> tokio::task::JoinHandle<()> {
    spawn_update_loop_triggered(config, ready, None)
}

pub fn spawn_update_loop_triggered(
    config: UpdateConfig,
    ready: mpsc::Sender<PreparedUpdate>,
    trigger: Option<UpdateTrigger>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = match http_client() {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(error=?error, "cannot initialize self-updater HTTP client");
                return;
            }
        };

        let mut interval = tokio::time::interval(config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if let Some(trigger) = &trigger {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = trigger.wait() => {}
                }
            } else {
                interval.tick().await;
            }
            match prepare_update(&client, &config).await {
                Ok(Some(update)) => {
                    tracing::info!(from=%update.from_sha, to=%update.to_sha, "self-update prepared");
                    if ready.send(update).await.is_err() {
                        tracing::warn!("self-update receiver closed");
                    }
                    return;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(error=?error, "self-update check failed"),
            }
        }
    })
}

pub async fn fetch_server_relay_package(
    manifest_url: &str,
    server_info_url: &str,
    expected_target: &str,
) -> Result<Option<RelayPackage>> {
    let client = http_client()?;
    let manifest = fetch_manifest(&client, manifest_url).await?;
    let server = client
        .get(server_info_url)
        .send()
        .await
        .context("query server build before relaying update")?
        .error_for_status()
        .context("server build query returned an error")?
        .json::<ServerBuildInfo>()
        .await
        .context("decode server build information")?;
    if server.build_sha == manifest.commit_sha {
        return Ok(None);
    }
    if release_is_stale(server.release_sequence, manifest.release_sequence) {
        tracing::warn!(
            installed_sequence = server.release_sequence,
            available_sequence = manifest.release_sequence,
            available = %manifest.commit_sha,
            "ignoring stale server update manifest"
        );
        return Ok(None);
    }
    if manifest.server.target != expected_target {
        bail!(
            "relay target mismatch: expected {}, got {}",
            expected_target,
            manifest.server.target
        );
    }
    let archive = download_archive(&client, &manifest.server).await?;
    Ok(Some(RelayPackage {
        commit_sha: manifest.commit_sha,
        release_sequence: manifest.release_sequence,
        archive_sha256: manifest.server.sha256,
        archive,
    }))
}

pub async fn prepare_relayed_update(
    config: &UpdateConfig,
    commit_sha: &str,
    release_sequence: u64,
    archive_sha256: &str,
    archive: &[u8],
) -> Result<Option<PreparedUpdate>> {
    if config.role != UpdateRole::Server {
        bail!("relayed updates are only accepted for the server role");
    }
    validate_commit_sha(commit_sha)?;
    validate_digest(archive_sha256)?;
    if commit_sha == build_sha() {
        return Ok(None);
    }
    // A client from before releaseSequence support omits this relay argument.
    // Accept sequence zero for one rolling-compatibility window; the staged
    // binary identity and checksum are still verified. New relays always send
    // a sequence and therefore receive the monotonic downgrade guard.
    if release_sequence > 0 && release_is_stale(build_sequence(), release_sequence) {
        tracing::warn!(
            installed_sequence = build_sequence(),
            available_sequence = release_sequence,
            available = %commit_sha,
            "ignoring stale relayed update"
        );
        return Ok(None);
    }
    verify_archive(archive, archive_sha256)?;
    Ok(Some(
        stage_update(config, commit_sha, release_sequence, archive).await?,
    ))
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("nuntius-updater/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?)
}

async fn fetch_manifest(client: &reqwest::Client, manifest_url: &str) -> Result<UpdateManifest> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let separator = if manifest_url.contains('?') { '&' } else { '?' };
    let url = format!("{manifest_url}{separator}nonce={nonce}");
    let manifest = client
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .context("download update manifest")?
        .error_for_status()
        .context("update manifest returned an error")?
        .json::<UpdateManifest>()
        .await
        .context("decode update manifest")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

async fn prepare_update(
    client: &reqwest::Client,
    config: &UpdateConfig,
) -> Result<Option<PreparedUpdate>> {
    let manifest = fetch_manifest(client, &config.manifest_url).await?;

    if manifest.commit_sha == build_sha() {
        return Ok(None);
    }

    if release_is_stale(build_sequence(), manifest.release_sequence) {
        tracing::warn!(
            installed_sequence = build_sequence(),
            available_sequence = manifest.release_sequence,
            available = %manifest.commit_sha,
            "ignoring stale update manifest"
        );
        return Ok(None);
    }

    if let Some(server_info_url) = &config.required_server_info_url {
        let server = client
            .get(server_info_url)
            .send()
            .await
            .context("query server build before client update")?
            .error_for_status()
            .context("server build query returned an error")?
            .json::<ServerBuildInfo>()
            .await
            .context("decode server build information")?;
        if server.build_sha != manifest.commit_sha {
            tracing::debug!(server=%server.build_sha, available=%manifest.commit_sha, "waiting for server to update first");
            return Ok(None);
        }
    }

    let asset = match config.role {
        UpdateRole::Server => &manifest.server,
        UpdateRole::Client => &manifest.client,
    };
    if asset.target != config.expected_target {
        bail!(
            "update target mismatch: expected {}, got {}",
            config.expected_target,
            asset.target
        );
    }

    let archive = download_archive(client, asset).await?;
    Ok(Some(
        stage_update(
            config,
            &manifest.commit_sha,
            manifest.release_sequence,
            &archive,
        )
        .await?,
    ))
}

async fn download_archive(client: &reqwest::Client, asset: &ManifestAsset) -> Result<Vec<u8>> {
    let response = client
        .get(&asset.url)
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
    verify_archive(&archive, &asset.sha256)?;
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
    })
}

fn validate_manifest(manifest: &UpdateManifest) -> Result<()> {
    if manifest.schema_version != 1 {
        bail!(
            "unsupported update manifest schema {}",
            manifest.schema_version
        );
    }
    validate_commit_sha(&manifest.commit_sha)?;
    for asset in [&manifest.server, &manifest.client] {
        validate_digest(&asset.sha256)?;
        if !asset
            .url
            .starts_with("https://github.com/HelloAnner/nuntius/")
        {
            bail!("update asset URL is outside the trusted repository");
        }
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
            executable_path: self.executable_path.clone(),
            previous_path: self.previous_path.clone(),
        };
        write_marker(&self.marker_path, &marker)?;
        replace_file(&self.staged_path, &self.executable_path)?;

        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        let error = std::process::Command::new(&self.executable_path)
            .args(args)
            .exec();
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

#[cfg(unix)]
fn rollback_and_restart(marker: &UpdateMarker, marker_path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;

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

    #[test]
    fn release_sequence_never_moves_backwards_after_migration() {
        assert!(!release_is_stale(0, 0));
        assert!(!release_is_stale(0, 10));
        assert!(!release_is_stale(10, 11));
        assert!(release_is_stale(10, 10));
        assert!(release_is_stale(10, 9));
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
    fn rejects_manifest_outside_trusted_repository() {
        let manifest = UpdateManifest {
            schema_version: 1,
            commit_sha: "a".repeat(40),
            release_sequence: 1,
            server: ManifestAsset {
                url: "https://example.com/server.tar.gz".into(),
                sha256: "b".repeat(64),
                target: "x86_64-linux".into(),
            },
            client: ManifestAsset {
                url: "https://github.com/HelloAnner/nuntius/releases/client.tar.gz".into(),
                sha256: "c".repeat(64),
                target: "aarch64-macos".into(),
            },
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[tokio::test]
    async fn rejects_relay_archive_with_wrong_digest() {
        let config = UpdateConfig::production(
            UpdateRole::Server,
            "nuntius-server",
            "x86_64-unknown-linux-gnu",
            PathBuf::from("/tmp/nuntius-updater-test"),
            Duration::from_secs(60),
        );
        let result = prepare_relayed_update(
            &config,
            &"a".repeat(40),
            // Sequence zero is the legacy-relay compatibility path and deliberately bypasses
            // monotonic ordering, allowing this test to reach the digest validation even when
            // CI embeds a large NUNTIUS_BUILD_SEQUENCE in the test binary.
            0,
            &"b".repeat(64),
            b"not the expected archive",
        )
        .await;
        assert!(result.is_err());
    }
}
