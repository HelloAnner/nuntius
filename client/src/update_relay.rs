use crate::{config::ClientConfig, pairing};
use anyhow::{Context, Result, bail};
use nuntius_updater::{DEFAULT_MANIFEST_URL, RelayPackage};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

pub fn spawn(config: Arc<ClientConfig>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let manifest_url = std::env::var("NUNTIUS_UPDATE_MANIFEST_URL")
            .unwrap_or_else(|_| DEFAULT_MANIFEST_URL.into());
        let server_info_url = match pairing::endpoint(&config, "api/v1/info") {
            Ok(url) => url.to_string(),
            Err(error) => {
                tracing::error!(error=?error,"cannot resolve server update relay endpoint");
                return;
            }
        };
        let mut interval =
            tokio::time::interval(Duration::from_secs(config.update_interval_seconds));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match nuntius_updater::fetch_server_relay_package(
                &manifest_url,
                &server_info_url,
                "x86_64-unknown-linux-gnu",
            )
            .await
            {
                Ok(Some(package)) => {
                    if let Err(error) = upload(&config, package).await {
                        tracing::warn!(error=?error,"server update relay failed");
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(error=?error,"server update relay check failed"),
            }
        }
    })
}

async fn upload(config: &ClientConfig, package: RelayPackage) -> Result<()> {
    let command = &config.server_update_ssh_command;
    let remote_binary = config
        .server_update_remote_binary
        .as_ref()
        .context("server_update_remote_binary is not configured")?;
    let remote_data_dir = config
        .server_update_remote_data_dir
        .as_ref()
        .context("server_update_remote_data_dir is not configured")?;
    let device_id = config
        .device_id
        .as_deref()
        .context("server update relay client is not paired")?;
    let target = package.commit_sha.clone();
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .arg(remote_binary)
        .arg("--data-dir")
        .arg(remote_data_dir)
        .arg("receive-update")
        .arg("--commit-sha")
        .arg(&package.commit_sha)
        .arg("--archive-sha256")
        .arg(&package.archive_sha256)
        .arg("--source-device-id")
        .arg(device_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("start configured SSH executable {}", command[0]))?;
    let mut stdin = child.stdin.take().context("open SSH update input")?;
    let output = tokio::time::timeout(Duration::from_secs(180), async move {
        stdin
            .write_all(&package.archive)
            .await
            .context("stream server update archive over SSH")?;
        stdin.shutdown().await.context("finish SSH update input")?;
        child
            .wait_with_output()
            .await
            .context("wait for remote server update receiver")
    })
    .await
    .context("SSH server update relay timed out")??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr.trim().chars().take(2048).collect();
        bail!(
            "remote server update receiver exited with {}: {}",
            output.status,
            detail
        );
    }
    tracing::info!(target=%target,device_id=%device_id,"server update package relayed over configured SSH connection");
    Ok(())
}
