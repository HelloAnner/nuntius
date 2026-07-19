use anyhow::{Context, Result, bail};
use nuntius_updater::{MAX_ARCHIVE_BYTES, PreparedUpdate, UpdateConfig, UpdateRole};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::mpsc};

const METADATA_FILE: &str = "relayed-server-update.json";
const ARCHIVE_FILE: &str = "relayed-server-update.tar.gz";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayMetadata {
    commit_sha: String,
    #[serde(default)]
    release_sequence: u64,
    archive_sha256: String,
    source_device_id: String,
}

pub async fn receive(
    data_dir: &Path,
    commit_sha: String,
    release_sequence: u64,
    archive_sha256: String,
    source_device_id: String,
) -> Result<()> {
    validate_hex(&commit_sha, 40, "commit SHA")?;
    validate_hex(&archive_sha256, 64, "archive SHA-256")?;
    validate_source_device_id(&source_device_id)?;

    let run_dir = data_dir.join("run");
    if !data_dir.join(crate::config::CONFIG_FILE).is_file() || !run_dir.is_dir() {
        bail!(
            "{} is not an initialized Nuntius server data directory",
            data_dir.display()
        );
    }

    let mut archive = Vec::new();
    tokio::io::stdin()
        .take((MAX_ARCHIVE_BYTES + 1) as u64)
        .read_to_end(&mut archive)
        .await
        .context("read relayed update archive from standard input")?;
    if archive.is_empty() || archive.len() > MAX_ARCHIVE_BYTES {
        bail!("relayed update archive has an invalid size");
    }
    let actual_digest = hex::encode(Sha256::digest(&archive));
    if actual_digest != archive_sha256 {
        bail!(
            "relayed update checksum mismatch: expected {}, got {}",
            archive_sha256,
            actual_digest
        );
    }

    let metadata = serde_json::to_vec(&RelayMetadata {
        commit_sha: commit_sha.clone(),
        release_sequence,
        archive_sha256,
        source_device_id: source_device_id.clone(),
    })?;
    let metadata_path = run_dir.join(METADATA_FILE);
    remove_if_exists(&metadata_path)?;
    atomic_shared_write(&run_dir.join(ARCHIVE_FILE), &archive)?;
    // Metadata is the ready marker and must be published after the archive.
    atomic_shared_write(&metadata_path, &metadata)?;
    println!("accepted server update {commit_sha} from {source_device_id}");
    Ok(())
}

pub fn spawn(
    data_dir: PathBuf,
    interval: Duration,
    ready: mpsc::Sender<PreparedUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(interval);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            match prepare_from_inbox(&data_dir, interval).await {
                Ok(Some((update, source_device_id))) => {
                    tracing::info!(
                        target = %update.target_sha(),
                        source_device_id = %source_device_id,
                        "validated server update received through SSH relay"
                    );
                    if let Err(error) = clear_inbox(&data_dir) {
                        tracing::warn!(error=?error, "cannot clear accepted server update inbox");
                    }
                    if ready.send(update).await.is_err() {
                        tracing::warn!("server update receiver closed");
                    }
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(error=?error, "rejected server update from SSH relay inbox");
                    if let Err(clear_error) = clear_inbox(&data_dir) {
                        tracing::warn!(error=?clear_error, "cannot clear rejected server update inbox");
                    }
                }
            }
        }
    })
}

async fn prepare_from_inbox(
    data_dir: &Path,
    interval: Duration,
) -> Result<Option<(PreparedUpdate, String)>> {
    let run_dir = data_dir.join("run");
    let metadata_bytes = match tokio::fs::read(run_dir.join(METADATA_FILE)).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read relayed update metadata"),
    };
    let metadata: RelayMetadata =
        serde_json::from_slice(&metadata_bytes).context("decode relayed update metadata")?;
    validate_hex(&metadata.commit_sha, 40, "commit SHA")?;
    validate_hex(&metadata.archive_sha256, 64, "archive SHA-256")?;
    validate_source_device_id(&metadata.source_device_id)?;

    let archive_path = run_dir.join(ARCHIVE_FILE);
    let archive_size = tokio::fs::metadata(&archive_path)
        .await
        .context("inspect relayed update archive")?
        .len();
    if archive_size == 0 || archive_size > MAX_ARCHIVE_BYTES as u64 {
        bail!("relayed update archive has an invalid size");
    }
    let archive = tokio::fs::read(&archive_path)
        .await
        .context("read relayed update archive")?;
    let config = UpdateConfig::production(
        UpdateRole::Server,
        "nuntius-server",
        "x86_64-unknown-linux-gnu",
        data_dir.to_path_buf(),
        interval,
    );
    let update = nuntius_updater::prepare_relayed_update(
        &config,
        &metadata.commit_sha,
        metadata.release_sequence,
        &metadata.archive_sha256,
        &archive,
    )
    .await?;
    let Some(update) = update else {
        clear_inbox(data_dir)?;
        return Ok(None);
    };
    Ok(Some((update, metadata.source_device_id)))
}

fn clear_inbox(data_dir: &Path) -> Result<()> {
    let run_dir = data_dir.join("run");
    remove_if_exists(&run_dir.join(METADATA_FILE))?;
    remove_if_exists(&run_dir.join(ARCHIVE_FILE))?;
    Ok(())
}

fn validate_hex(value: &str, expected_length: usize, label: &str) -> Result<()> {
    if value.len() != expected_length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid {label}");
    }
    Ok(())
}

fn validate_source_device_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("invalid source device ID");
    }
    Ok(())
}

fn atomic_shared_write(path: &Path, contents: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("relay");
    let temporary = path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(contents)?;
        file.sync_all()?;
        set_shared_permissions(&temporary)?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace {} with {}", path.display(), temporary.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
fn set_shared_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_shared_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_relay_files_atomically_for_the_server_user() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("relay.bin");
        atomic_shared_write(&destination, b"first").unwrap();
        atomic_shared_write(&destination, b"second").unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"second");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(destination).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
    }

    #[test]
    fn rejects_unsafe_relay_source_identity() {
        assert!(validate_source_device_id("dev_valid-1").is_ok());
        assert!(validate_source_device_id("dev_bad;command").is_err());
    }
}
