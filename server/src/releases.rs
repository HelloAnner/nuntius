use crate::{protocol::ClientRelease, tunnel::TunnelRegistry};
use anyhow::{Context, Result, bail};
use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use url::Url;

pub const RELEASES_DIR: &str = "releases";
pub const DESIRED_CLIENT_FILE: &str = "desired-client.json";
pub const CLIENT_ARCHIVE: &str = "nuntius-client-macos-arm64.tar.gz";
const MAX_CLIENT_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct ReleaseStore {
    current: Arc<RwLock<Option<ClientRelease>>>,
}

impl ReleaseStore {
    pub async fn load(data_dir: &Path, public_base_url: &str) -> Result<Self> {
        let store = Self::default();
        let release = load_desired(data_dir, public_base_url)?;
        *store.current.write().await = release;
        Ok(store)
    }

    pub async fn current(&self) -> Option<ClientRelease> {
        self.current.read().await.clone()
    }

    async fn refresh(
        &self,
        data_dir: &Path,
        public_base_url: &str,
    ) -> Result<Option<ClientRelease>> {
        let next = load_desired(data_dir, public_base_url)?;
        let mut current = self.current.write().await;
        if *current == next {
            return Ok(None);
        }
        *current = next.clone();
        Ok(next)
    }
}

pub fn spawn_watcher(
    data_dir: std::path::PathBuf,
    public_base_url: String,
    releases: ReleaseStore,
    tunnels: Arc<TunnelRegistry>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match releases.refresh(&data_dir, &public_base_url).await {
                Ok(Some(release)) => {
                    let delivered = tunnels.broadcast_client_release(release.clone()).await;
                    tracing::info!(
                        release_id = %release.release_id,
                        commit_sha = %release.commit_sha,
                        release_sequence = release.release_sequence,
                        delivered,
                        "desired client release updated"
                    );
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(error=?error, "cannot refresh desired client release"),
            }
        }
    })
}

pub async fn serve_client_archive(data_dir: &Path, release_id: &str, file_name: &str) -> Response {
    if !safe_component(release_id) || file_name != CLIENT_ARCHIVE {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = data_dir
        .join(RELEASES_DIR)
        .join(release_id)
        .join(CLIENT_ARCHIVE);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) if !bytes.is_empty() && bytes.len() as u64 <= MAX_CLIENT_ARCHIVE_BYTES => bytes,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(error) => {
            tracing::warn!(error=?error,path=%path.display(),"cannot read client release archive");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    (
        [
            (header::CONTENT_TYPE, "application/gzip"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response()
}

fn load_desired(data_dir: &Path, public_base_url: &str) -> Result<Option<ClientRelease>> {
    let path = data_dir.join(RELEASES_DIR).join(DESIRED_CLIENT_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let release: ClientRelease =
        serde_json::from_slice(&bytes).context("decode desired client release")?;
    validate_release(data_dir, public_base_url, &release)?;
    Ok(Some(release))
}

fn validate_release(data_dir: &Path, public_base_url: &str, release: &ClientRelease) -> Result<()> {
    if !safe_component(&release.release_id) {
        bail!("invalid client release ID");
    }
    validate_hex(&release.commit_sha, 40, "client release commit SHA")?;
    validate_hex(&release.sha256, 64, "client release SHA-256")?;
    if release.release_sequence == 0 || release.target != "aarch64-apple-darwin" {
        bail!("invalid client release identity");
    }
    if release.size == 0 || release.size > MAX_CLIENT_ARCHIVE_BYTES {
        bail!("invalid client release archive size");
    }
    let base = Url::parse(public_base_url).context("parse public base URL")?;
    let url = Url::parse(&release.url).context("parse client release URL")?;
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || url.path()
            != format!(
                "/api/v1/client-releases/{}/{}",
                release.release_id, CLIENT_ARCHIVE
            )
    {
        bail!("client release URL is outside the configured server origin");
    }
    let archive_path = data_dir
        .join(RELEASES_DIR)
        .join(&release.release_id)
        .join(CLIENT_ARCHIVE);
    let archive = fs::read(&archive_path)
        .with_context(|| format!("read client archive {}", archive_path.display()))?;
    if archive.len() as u64 != release.size {
        bail!("client release archive size mismatch");
    }
    let actual = hex::encode(Sha256::digest(&archive));
    if actual != release.sha256 {
        bail!("client release archive checksum mismatch");
    }
    Ok(())
}

fn validate_hex(value: &str, length: usize, label: &str) -> Result<()> {
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid {label}");
    }
    Ok(())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_components_are_bounded() {
        assert!(safe_component("1784512000000-aabbccddeeff"));
        assert!(!safe_component("../latest"));
        assert!(!safe_component(""));
    }
}
