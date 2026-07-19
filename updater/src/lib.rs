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
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, sync::mpsc};

pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/HelloAnner/nuntius/releases/download/continuous/manifest.json";
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

pub fn build_sha() -> &'static str {
    option_env!("NUNTIUS_BUILD_SHA").unwrap_or("development")
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
    pub target: String,
}

impl BuildInfo {
    pub fn current(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            build_sha: build_sha().into(),
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
}

#[derive(Debug)]
pub struct PreparedUpdate {
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
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .user_agent(format!("nuntius-updater/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::limited(8))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(error=?error, "cannot initialize self-updater HTTP client");
                return;
            }
        };

        let mut interval = tokio::time::interval(config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
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

async fn prepare_update(
    client: &reqwest::Client,
    config: &UpdateConfig,
) -> Result<Option<PreparedUpdate>> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let separator = if config.manifest_url.contains('?') {
        '&'
    } else {
        '?'
    };
    let manifest_url = format!("{}{separator}nonce={nonce}", config.manifest_url);
    let manifest = client
        .get(&manifest_url)
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

    if manifest.commit_sha == build_sha() {
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
    let archive = response.bytes().await.context("read update archive")?;
    if archive.len() > MAX_ARCHIVE_BYTES {
        bail!("update archive exceeds size limit");
    }
    let actual_digest = hex::encode(Sha256::digest(&archive));
    if actual_digest != asset.sha256 {
        bail!(
            "update archive checksum mismatch: expected {}, got {}",
            asset.sha256,
            actual_digest
        );
    }
    let binary = extract_binary(&archive, &config.binary_name)?;
    let executable_path = std::env::current_exe().context("resolve current executable")?;
    let directory = executable_path
        .parent()
        .context("current executable has no parent directory")?;
    let staged_path = directory.join(format!(
        ".{}.update-{}",
        config.binary_name, manifest.commit_sha
    ));
    write_executable(&staged_path, &binary)?;
    probe_binary(
        &staged_path,
        &config.binary_name,
        &manifest.commit_sha,
        &config.expected_target,
    )
    .await
    .inspect_err(|_| {
        let _ = fs::remove_file(&staged_path);
    })?;

    let previous_path = directory.join(format!("{}.previous", config.binary_name));
    Ok(Some(PreparedUpdate {
        staged_path,
        executable_path,
        previous_path,
        marker_path: marker_path(&config.data_dir),
        from_sha: build_sha().into(),
        to_sha: manifest.commit_sha,
    }))
}

fn validate_manifest(manifest: &UpdateManifest) -> Result<()> {
    if manifest.schema_version != 1 {
        bail!(
            "unsupported update manifest schema {}",
            manifest.schema_version
        );
    }
    if manifest.commit_sha.len() != 40
        || !manifest
            .commit_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("invalid update commit SHA");
    }
    for asset in [&manifest.server, &manifest.client] {
        if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid update archive SHA-256");
        }
        if !asset
            .url
            .starts_with("https://github.com/HelloAnner/nuntius/")
        {
            bail!("update asset URL is outside the trusted repository");
        }
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
        || info.target != expected_target
    {
        bail!(
            "updated binary identity mismatch: name={}, sha={}, target={}",
            info.name,
            info.build_sha,
            info.target
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
}
