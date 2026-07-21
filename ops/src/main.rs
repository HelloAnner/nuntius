use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use directories::BaseDirs;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, sync::watch};
use tracing_subscriber::EnvFilter;

const CLIENT_ARCHIVE: &str = "nuntius-client-macos-arm64.tar.gz";
const SERVER_ARCHIVE: &str = "nuntius-server-linux-x86_64.tar.gz";
const DESIRED_CLIENT_FILE: &str = "desired-client.json";
const MACOS_TARGET: &str = "aarch64-apple-darwin";
const LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";
const DEFAULT_MACOS_SIGNING_IDENTITY: &str = "Nuntius Local Release";
const MACOS_CLIENT_SIGNING_IDENTIFIER: &str = "com.helloanner.nuntius-client";
const MACOS_OPS_SIGNING_IDENTIFIER: &str = "com.helloanner.nuntius-ops";
const OPS_UPDATE_PROBATION: Duration = Duration::from_secs(60);
const OPS_UPDATE_INPUTS: &[&str] = &["ops", "updater", "Cargo.toml", "Cargo.lock"];
const SERVER_BUILDER_DOCKERFILE: &str = include_str!("../docker/server-builder.Dockerfile");

#[derive(Parser)]
#[command(
    name = "nuntius-ops",
    version,
    about = "Nuntius release and deployment controller"
)]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: OpsCommand,
}

#[derive(Subcommand)]
enum OpsCommand {
    Init {
        #[arg(long)]
        force: bool,
    },
    Run,
    Once {
        #[arg(long)]
        force: bool,
    },
    Status,
    BuildInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct OpsConfig {
    repository_url: String,
    branch: String,
    poll_interval_seconds: u64,
    retry_seconds: u64,
    rust_toolchain: String,
    linux_builder_image: String,
    macos_signing_identity: String,
    macos_signing_identity_sha1: Option<String>,
    state_dir: PathBuf,
    public_base_url: String,
    ssh_program: String,
    scp_program: String,
    remote_host: String,
    remote_root: String,
    remote_data_dir: String,
    remote_binary: String,
    remote_service: String,
    remote_user: String,
    remote_group: String,
}

impl Default for OpsConfig {
    fn default() -> Self {
        Self {
            repository_url: "https://github.com/HelloAnner/nuntius.git".into(),
            branch: "main".into(),
            poll_interval_seconds: 20,
            retry_seconds: 60,
            rust_toolchain: "1.94.0".into(),
            linux_builder_image: "nuntius-server-builder:rust-1.94.0".into(),
            macos_signing_identity: DEFAULT_MACOS_SIGNING_IDENTITY.into(),
            macos_signing_identity_sha1: None,
            state_dir: default_root().unwrap_or_else(|_| PathBuf::from(".nuntius-ops")),
            public_base_url: "http://47.97.154.221:8765/".into(),
            ssh_program: "ssh".into(),
            scp_program: "scp".into(),
            remote_host: "moss-dev".into(),
            remote_root: "/var/docker/mysql/nuntius".into(),
            remote_data_dir: "/var/docker/mysql/nuntius/data".into(),
            remote_binary: "/var/docker/mysql/nuntius/bin/nuntius-server".into(),
            remote_service: "nuntius-server".into(),
            remote_user: "nuntius".into(),
            remote_group: "nuntius".into(),
        }
    }
}

impl OpsConfig {
    fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read ops configuration {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("parse ops configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.repository_url.trim().is_empty()
            || !safe_git_ref(&self.branch)
            || self.poll_interval_seconds < 5
            || self.retry_seconds < 10
            || self.rust_toolchain.trim().is_empty()
            || self.linux_builder_image.trim().is_empty()
            || !valid_signing_identity(&self.macos_signing_identity)
            || self
                .macos_signing_identity_sha1
                .as_deref()
                .is_some_and(|value| !valid_certificate_sha1(value))
            || !self.state_dir.is_absolute()
            || self.ssh_program.trim().is_empty()
            || self.scp_program.trim().is_empty()
            || self.remote_host.trim().is_empty()
            || self.remote_service.trim().is_empty()
            || self.remote_user.trim().is_empty()
            || self.remote_group.trim().is_empty()
        {
            bail!("invalid ops configuration");
        }
        let base =
            reqwest::Url::parse(&self.public_base_url).context("public_base_url is invalid")?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            bail!("public_base_url must be an absolute HTTP(S) URL");
        }
        for (label, path) in [
            ("remote_root", &self.remote_root),
            ("remote_data_dir", &self.remote_data_dir),
            ("remote_binary", &self.remote_binary),
        ] {
            validate_remote_path(label, path)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct OpsState {
    observed_sha: Option<String>,
    building_sha: Option<String>,
    deployed_sha: Option<String>,
    ops_sha: Option<String>,
    last_sequence: u64,
    phase: String,
    last_error: Option<String>,
    updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientRelease {
    release_id: String,
    commit_sha: String,
    release_sequence: u64,
    target: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildInfo {
    name: String,
    build_sha: String,
    release_sequence: u64,
    target: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerInfo {
    build_sha: String,
    release_sequence: u64,
}

struct BuildOutput {
    source_dir: PathBuf,
    package_dir: PathBuf,
    release: ClientRelease,
}

#[derive(Debug, Clone)]
struct SigningIdentity {
    sha1: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("nuntius_ops=info")),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or(default_config_path()?);
    if matches!(&cli.command, OpsCommand::Run | OpsCommand::Once { .. }) {
        // Advance or roll back an in-flight update before the new binary parses
        // and validates the complete configuration. This keeps a config-schema
        // regression from trapping launchd in a crash loop on the bad binary.
        let update_dir = startup_update_dir(&config_path);
        nuntius_updater::handle_startup(&update_dir)?;
    }
    match cli.command {
        OpsCommand::Init { force } => initialize(&config_path, force),
        OpsCommand::Run => {
            let config = OpsConfig::load(&config_path)?;
            run(config).await
        }
        OpsCommand::Once { force } => {
            let config = OpsConfig::load(&config_path)?;
            reconcile_once(config, force).await
        }
        OpsCommand::Status => print_status(&OpsConfig::load(&config_path)?),
        OpsCommand::BuildInfo => {
            println!(
                "{}",
                serde_json::to_string(&nuntius_updater::BuildInfo::current(
                    "nuntius-ops",
                    env!("CARGO_PKG_VERSION"),
                ))?
            );
            Ok(())
        }
    }
}

fn initialize(config_path: &Path, force: bool) -> Result<()> {
    if config_path.exists() && !force {
        bail!("{} already exists", config_path.display());
    }
    let mut config = OpsConfig::default();
    if let Some(parent) = config_path.parent() {
        config.state_dir = parent.to_path_buf();
        fs::create_dir_all(parent)?;
    }
    config.validate()?;
    atomic_write(config_path, toml::to_string_pretty(&config)?.as_bytes())?;
    prepare_state_dirs(&config)?;
    println!("initialized {}", config_path.display());
    Ok(())
}

fn print_status(config: &OpsConfig) -> Result<()> {
    let state = load_state(config)?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

async fn run(config: OpsConfig) -> Result<()> {
    prepare_state_dirs(&config)?;
    let _lock = acquire_lock(&config)?;
    ensure_environment(&config).await?;
    complete_ops_update_probation(&config).await?;
    let (tx, mut rx) = watch::channel::<Option<String>>(None);
    let detector_config = config.clone();
    tokio::spawn(async move {
        let mut last = None;
        loop {
            match remote_head(&detector_config).await {
                Ok(sha) => {
                    if last.as_deref() != Some(sha.as_str()) {
                        tracing::info!(%sha, "repository change detected");
                        last = Some(sha.clone());
                        tx.send_replace(Some(sha));
                    }
                }
                Err(error) => tracing::warn!(error=?error, "repository detection failed"),
            }
            tokio::time::sleep(Duration::from_secs(detector_config.poll_interval_seconds)).await;
        }
    });

    loop {
        let desired = rx.borrow().clone();
        let Some(mut sha) = desired else {
            if rx.changed().await.is_err() {
                return Ok(());
            }
            continue;
        };
        if load_state(&config)?.deployed_sha.as_deref() == Some(sha.as_str()) {
            if rx.changed().await.is_err() {
                return Ok(());
            }
            continue;
        }
        loop {
            if let Err(error) = ensure_ops_current(&config, &sha).await {
                record_failure(&config, &sha, &error)?;
                tracing::error!(%sha,error=?error,"ops self-update failed");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(config.retry_seconds)) => {}
                    changed = rx.changed() => {
                        if changed.is_err() { return Ok(()); }
                        if let Some(latest) = rx.borrow().clone() { sha = latest; }
                    }
                }
                continue;
            }
            match build_release(&config, &sha).await {
                Ok(output) => {
                    if let Some(latest) = rx.borrow().clone()
                        && latest != sha
                    {
                        tracing::info!(built=%sha,queued=%latest,"discarding superseded build");
                        sha = latest;
                        continue;
                    }
                    match deploy_release(&config, &output).await {
                        Ok(()) => {
                            update_state(&config, |state| {
                                state.deployed_sha = Some(sha.clone());
                                state.building_sha = None;
                                state.phase = "idle".into();
                                state.last_error = None;
                            })?;
                            cleanup_local_builds(&config, &output.source_dir)?;
                            tracing::info!(%sha,release_id=%output.release.release_id,"release deployed");
                            break;
                        }
                        Err(error) => {
                            record_failure(&config, &sha, &error)?;
                            tracing::error!(%sha,error=?error,"release deployment failed");
                        }
                    }
                }
                Err(error) => {
                    record_failure(&config, &sha, &error)?;
                    tracing::error!(%sha,error=?error,"release build failed");
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(config.retry_seconds)) => {}
                changed = rx.changed() => {
                    if changed.is_err() { return Ok(()); }
                    if let Some(latest) = rx.borrow().clone() { sha = latest; }
                }
            }
        }
    }
}

async fn reconcile_once(config: OpsConfig, force: bool) -> Result<()> {
    prepare_state_dirs(&config)?;
    let _lock = acquire_lock(&config)?;
    ensure_environment(&config).await?;
    complete_ops_update_probation(&config).await?;
    let sha = remote_head(&config).await?;
    ensure_ops_current(&config, &sha).await?;
    if !force && load_state(&config)?.deployed_sha.as_deref() == Some(sha.as_str()) {
        tracing::info!(%sha, "latest repository commit is already deployed");
        return Ok(());
    }
    let output = build_release(&config, &sha).await?;
    deploy_release(&config, &output).await?;
    update_state(&config, |state| {
        state.deployed_sha = Some(sha);
        state.building_sha = None;
        state.phase = "idle".into();
        state.last_error = None;
    })?;
    cleanup_local_builds(&config, &output.source_dir)?;
    Ok(())
}

async fn complete_ops_update_probation(config: &OpsConfig) -> Result<()> {
    if !nuntius_updater::startup_update_pending(&config.state_dir)? {
        update_state(config, |state| {
            state.ops_sha = Some(nuntius_updater::build_sha().into());
        })?;
        return Ok(());
    }
    tracing::info!(
        seconds = OPS_UPDATE_PROBATION.as_secs(),
        "ops self-update is in startup probation"
    );
    tokio::time::sleep(OPS_UPDATE_PROBATION).await;
    remote_head(config)
        .await
        .context("ops self-update probation repository check failed")?;
    nuntius_updater::mark_healthy(&config.state_dir)?;
    update_state(config, |state| {
        state.ops_sha = Some(nuntius_updater::build_sha().into());
        state.phase = "idle".into();
        state.last_error = None;
    })?;
    Ok(())
}

async fn ensure_ops_current(config: &OpsConfig, sha: &str) -> Result<()> {
    validate_sha(sha)?;
    let current = nuntius_updater::build_sha();
    if current == sha {
        return Ok(());
    }
    validate_sha(current).context(
        "running Ops has no release build identity; install one signed cloud artifact before enabling self-update",
    )?;
    if nuntius_updater::update_build_is_rejected(&config.state_dir, sha)? {
        bail!("Ops release {sha} previously failed startup probation; waiting for a newer commit");
    }

    update_state(config, |state| {
        state.observed_sha = Some(sha.into());
        state.phase = "ops_checkout".into();
        state.last_error = None;
    })?;
    let source_dir = checkout_ops_source(config, current, sha).await?;
    if !ops_inputs_changed(&source_dir, current, sha).await? {
        if let Some(build_root) = source_dir.parent() {
            fs::remove_dir_all(build_root).with_context(|| {
                format!(
                    "remove unchanged Ops update checkout {}",
                    build_root.display()
                )
            })?;
        }
        return Ok(());
    }

    update_state(config, |state| state.phase = "ops_build".into())?;
    let candidate = build_ops_candidate(config, &source_dir, sha).await?;
    verify_ops_binary(&candidate, sha).await?;
    let identity = resolve_signing_identity(config).await?;
    update_state(config, |state| state.phase = "ops_signing".into())?;
    sign_macos_binary(&identity, MACOS_OPS_SIGNING_IDENTIFIER, &candidate).await?;
    verify_macos_signature(&candidate, MACOS_OPS_SIGNING_IDENTIFIER, &identity.sha1).await?;
    verify_ops_binary(&candidate, sha).await?;

    let update = nuntius_updater::prepare_local_update(
        &config.state_dir,
        "nuntius-ops",
        MACOS_TARGET,
        MACOS_OPS_SIGNING_IDENTIFIER,
        sha,
        &candidate,
    )
    .await?;
    update_state(config, |state| state.phase = "ops_activating".into())?;
    tracing::info!(from=%current,to=%sha,"activating signed Ops self-update");
    update.activate()?;
    bail!("Ops self-update activation returned unexpectedly")
}

async fn checkout_ops_source(config: &OpsConfig, current: &str, target: &str) -> Result<PathBuf> {
    let build_root = config.state_dir.join("ops-updates").join(target);
    let source_dir = build_root.join("source");
    if build_root.exists() {
        fs::remove_dir_all(&build_root)
            .with_context(|| format!("clear previous Ops update {}", build_root.display()))?;
    }
    fs::create_dir_all(&source_dir)?;

    let mut init = Command::new("git");
    init.args(["init", "--quiet"]).arg(&source_dir);
    checked(
        init,
        "initialize Ops update source repository",
        Duration::from_secs(30),
    )
    .await?;
    let mut remote = Command::new("git");
    remote
        .current_dir(&source_dir)
        .args(["remote", "add", "origin", &config.repository_url]);
    checked(
        remote,
        "configure Ops update source repository",
        Duration::from_secs(30),
    )
    .await?;
    for revision in [target, current] {
        let mut fetch = Command::new("git");
        fetch
            .current_dir(&source_dir)
            .args(["fetch", "--depth", "1", "origin", revision]);
        checked(
            fetch,
            "fetch Ops update source commit",
            Duration::from_secs(300),
        )
        .await?;
    }
    let mut checkout = Command::new("git");
    checkout
        .current_dir(&source_dir)
        .args(["checkout", "--quiet", "--detach", target]);
    checked(
        checkout,
        "checkout Ops update source commit",
        Duration::from_secs(30),
    )
    .await?;
    Ok(source_dir)
}

async fn ops_inputs_changed(source_dir: &Path, current: &str, target: &str) -> Result<bool> {
    let mut diff = Command::new("git");
    diff.current_dir(source_dir)
        .args(["diff", "--quiet", current, target, "--"])
        .args(OPS_UPDATE_INPUTS);
    let output = output(diff, "compare Ops update inputs", Duration::from_secs(30)).await?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => bail!(
            "compare Ops update inputs failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

async fn build_ops_candidate(config: &OpsConfig, source_dir: &Path, sha: &str) -> Result<PathBuf> {
    let mac_target = config.state_dir.join("cache/macos-target");
    fs::create_dir_all(&mac_target)?;
    let mut command = Command::new("rustup");
    command
        .current_dir(source_dir)
        .args([
            "run",
            &config.rust_toolchain,
            "cargo",
            "build",
            "--locked",
            "--release",
            "--package",
            "nuntius-ops",
        ])
        .env("NUNTIUS_BUILD_SHA", sha)
        .env("NUNTIUS_BUILD_TARGET", MACOS_TARGET)
        .env("CARGO_TARGET_DIR", &mac_target);
    checked(
        command,
        "build macOS ARM Ops update",
        Duration::from_secs(1800),
    )
    .await?;
    Ok(mac_target.join("release/nuntius-ops"))
}

async fn verify_ops_binary(path: &Path, sha: &str) -> Result<()> {
    let mut command = Command::new(path);
    command.arg("build-info");
    let output = checked(command, "probe macOS Ops", Duration::from_secs(30)).await?;
    validate_build_info(&output.stdout, "nuntius-ops", sha, 0, MACOS_TARGET)
}

async fn ensure_environment(config: &OpsConfig) -> Result<()> {
    verify_macos_signing_identity(config).await?;

    let mut rustup = Command::new("rustup");
    rustup
        .args([
            "toolchain",
            "install",
            &config.rust_toolchain,
            "--profile",
            "minimal",
        ])
        .stdin(Stdio::null());
    checked(rustup, "install Rust toolchain", Duration::from_secs(900)).await?;

    let mut inspect = Command::new("docker");
    inspect.args(["image", "inspect", &config.linux_builder_image]);
    if output(
        inspect,
        "inspect Linux builder image",
        Duration::from_secs(30),
    )
    .await?
    .status
    .success()
    {
        return Ok(());
    }
    let dockerfile = config.state_dir.join("bootstrap/server-builder.Dockerfile");
    if !dockerfile.is_file() {
        bail!(
            "{} is missing; install it with the nuntius-ops binary",
            dockerfile.display()
        );
    }
    let mut docker = Command::new("docker");
    docker
        .args(["build", "--platform", "linux/amd64", "--build-arg"])
        .arg(format!("RUST_VERSION={}", config.rust_toolchain))
        .args(["-t", &config.linux_builder_image, "-f"])
        .arg(&dockerfile)
        .arg(dockerfile.parent().expect("bootstrap directory"));
    checked(
        docker,
        "build Linux builder image",
        Duration::from_secs(1800),
    )
    .await?;
    Ok(())
}

async fn remote_head(config: &OpsConfig) -> Result<String> {
    let reference = format!("refs/heads/{}", config.branch);
    let mut command = Command::new("git");
    command.args(["ls-remote", &config.repository_url, &reference]);
    let output = checked(command, "query repository HEAD", Duration::from_secs(30)).await?;
    let stdout = String::from_utf8(output.stdout).context("repository HEAD is not UTF-8")?;
    let sha = stdout
        .split_whitespace()
        .next()
        .context("repository HEAD response is empty")?;
    validate_sha(sha)?;
    Ok(sha.into())
}

async fn build_release(config: &OpsConfig, sha: &str) -> Result<BuildOutput> {
    validate_sha(sha)?;
    let remote_sequence = current_server_info(config)
        .await
        .ok()
        .map(|info| info.release_sequence);
    let sequence = allocate_sequence(config, remote_sequence)?;
    let release_id = format!("{}-{}", sequence, &sha[..12]);
    let build_root = config.state_dir.join("builds").join(&release_id);
    let source_dir = build_root.join("source");
    let package_dir = build_root.join("package");
    fs::create_dir_all(&package_dir)?;
    update_state(config, |state| {
        state.observed_sha = Some(sha.into());
        state.building_sha = Some(sha.into());
        state.phase = "checkout".into();
        state.last_error = None;
    })?;

    let mut init = Command::new("git");
    init.args(["init", "--quiet"]).arg(&source_dir);
    checked(
        init,
        "initialize clean source repository",
        Duration::from_secs(30),
    )
    .await?;
    let mut remote = Command::new("git");
    remote
        .current_dir(&source_dir)
        .args(["remote", "add", "origin", &config.repository_url]);
    checked(
        remote,
        "configure source repository",
        Duration::from_secs(30),
    )
    .await?;
    let mut fetch = Command::new("git");
    fetch
        .current_dir(&source_dir)
        .args(["fetch", "--depth", "1", "origin", sha]);
    checked(fetch, "fetch source commit", Duration::from_secs(300)).await?;
    let mut checkout = Command::new("git");
    checkout
        .current_dir(&source_dir)
        .args(["checkout", "--quiet", "--detach", "FETCH_HEAD"]);
    checked(checkout, "checkout source commit", Duration::from_secs(30)).await?;

    update_state(config, |state| state.phase = "frontend".into())?;
    let mut bun_install = Command::new("bun");
    bun_install
        .current_dir(&source_dir)
        .args(["install", "--frozen-lockfile"]);
    checked(
        bun_install,
        "install frontend dependencies",
        Duration::from_secs(600),
    )
    .await?;
    let mut typecheck = Command::new("bun");
    typecheck
        .current_dir(&source_dir)
        .args(["run", "typecheck"]);
    let mut frontend = Command::new("bun");
    frontend.current_dir(&source_dir).args(["run", "build"]);
    tokio::try_join!(
        checked(typecheck, "typecheck frontends", Duration::from_secs(900)),
        checked(frontend, "build frontends", Duration::from_secs(900)),
    )?;

    update_state(config, |state| state.phase = "binaries".into())?;
    let mac_target = config.state_dir.join("cache/macos-target");
    let linux_target = config.state_dir.join("cache/linux-target");
    let linux_cargo = config.state_dir.join("cache/linux-cargo");
    fs::create_dir_all(&mac_target)?;
    fs::create_dir_all(&linux_target)?;
    fs::create_dir_all(&linux_cargo)?;

    let mut client = Command::new("rustup");
    client
        .current_dir(&source_dir)
        .args([
            "run",
            &config.rust_toolchain,
            "cargo",
            "build",
            "--locked",
            "--release",
            "--package",
            "nuntius-client",
        ])
        .env("NUNTIUS_BUILD_SHA", sha)
        .env("NUNTIUS_BUILD_SEQUENCE", sequence.to_string())
        .env("NUNTIUS_BUILD_TARGET", MACOS_TARGET)
        .env("CARGO_TARGET_DIR", &mac_target);

    let mut server = Command::new("docker");
    server
        .args(["run", "--rm", "--platform", "linux/amd64"])
        .args(["-e", &format!("NUNTIUS_BUILD_SHA={sha}")])
        .args(["-e", &format!("NUNTIUS_BUILD_SEQUENCE={sequence}")])
        .args(["-e", &format!("NUNTIUS_BUILD_TARGET={LINUX_TARGET}")])
        .args(["-e", "CARGO_HOME=/cargo", "-e", "CARGO_TARGET_DIR=/target"])
        .args(["-v", &format!("{}:/workspace", source_dir.display())])
        .args(["-v", &format!("{}:/cargo", linux_cargo.display())])
        .args(["-v", &format!("{}:/target", linux_target.display())])
        .args(["-w", "/workspace", &config.linux_builder_image])
        .args([
            "cargo",
            "build",
            "--locked",
            "--release",
            "--package",
            "nuntius-server",
        ]);

    tokio::try_join!(
        checked(client, "build macOS ARM client", Duration::from_secs(3600)),
        checked(
            server,
            "build Linux AMD64 server",
            Duration::from_secs(3600)
        ),
    )?;

    let client_binary = mac_target.join("release/nuntius-client");
    let server_binary = linux_target.join("release/nuntius-server");
    verify_client_binary(&client_binary, sha, sequence).await?;
    verify_server_binary(config, &server_binary, sha, sequence).await?;

    update_state(config, |state| state.phase = "signing".into())?;
    sign_client_binary(config, &client_binary).await?;
    verify_client_signature(config, &client_binary).await?;
    verify_client_binary(&client_binary, sha, sequence).await?;

    update_state(config, |state| state.phase = "package".into())?;
    let client_archive = package_dir.join(CLIENT_ARCHIVE);
    let server_archive = package_dir.join(SERVER_ARCHIVE);
    create_archive(&client_binary, &client_archive).await?;
    create_archive(&server_binary, &server_archive).await?;
    let client_sha = sha256_file(&client_archive)?;
    let client_size = fs::metadata(&client_archive)?.len();
    let base = config.public_base_url.trim_end_matches('/');
    let release = ClientRelease {
        release_id: release_id.clone(),
        commit_sha: sha.into(),
        release_sequence: sequence,
        target: MACOS_TARGET.into(),
        url: format!("{base}/api/v1/client-releases/{release_id}/{CLIENT_ARCHIVE}"),
        sha256: client_sha,
        size: client_size,
    };
    atomic_write(
        &package_dir.join(DESIRED_CLIENT_FILE),
        &serde_json::to_vec_pretty(&release)?,
    )?;
    Ok(BuildOutput {
        source_dir,
        package_dir,
        release,
    })
}

async fn verify_client_binary(path: &Path, sha: &str, sequence: u64) -> Result<()> {
    let mut command = Command::new(path);
    command.arg("build-info");
    let output = checked(command, "probe macOS client", Duration::from_secs(30)).await?;
    validate_build_info(
        &output.stdout,
        "nuntius-client",
        sha,
        sequence,
        MACOS_TARGET,
    )
}

async fn verify_macos_signing_identity(config: &OpsConfig) -> Result<()> {
    let identity = resolve_signing_identity(config).await?;
    let probe = config
        .state_dir
        .join(format!(".signing-probe-{}", std::process::id()));
    fs::copy("/usr/bin/true", &probe).context("prepare macOS signing preflight")?;
    let result = async {
        sign_macos_binary(&identity, MACOS_CLIENT_SIGNING_IDENTIFIER, &probe).await?;
        verify_macos_signature(&probe, MACOS_CLIENT_SIGNING_IDENTIFIER, &identity.sha1).await
    }
    .await;
    let cleanup = fs::remove_file(&probe).context("remove macOS signing preflight binary");
    if let Err(error) = result {
        let _ = cleanup;
        return Err(error).context(
            "macOS signing preflight failed; run `nuntius-ops once --force` interactively and allow codesign to use the private key",
        );
    }
    cleanup?;
    Ok(())
}

async fn resolve_signing_identity(config: &OpsConfig) -> Result<SigningIdentity> {
    let mut command = Command::new("/usr/bin/security");
    command.args(["find-identity", "-v", "-p", "codesigning"]);
    let output = checked(
        command,
        "inspect macOS code-signing identities",
        Duration::from_secs(30),
    )
    .await?;
    let identities = String::from_utf8(output.stdout)
        .context("macOS code-signing identity list is not UTF-8")?;
    let matches = signing_identity_matches(&identities, &config.macos_signing_identity);
    let matched = if let Some(expected_sha1) = config.macos_signing_identity_sha1.as_deref() {
        matches
            .into_iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(expected_sha1))
            .with_context(|| {
                format!(
                    "macOS code-signing identity {:?} does not match pinned certificate {}",
                    config.macos_signing_identity, expected_sha1
                )
            })?
    } else if matches.len() == 1 {
        matches[0].clone()
    } else if matches.is_empty() {
        bail!(
            "macOS code-signing identity {:?} is unavailable; create or import it into the OPS login Keychain before running a release",
            config.macos_signing_identity
        );
    } else {
        bail!(
            "multiple macOS code-signing identities are named {:?}; configure macos_signing_identity_sha1",
            config.macos_signing_identity
        );
    };
    Ok(SigningIdentity {
        sha1: matched.to_ascii_uppercase(),
    })
}

async fn sign_client_binary(config: &OpsConfig, path: &Path) -> Result<()> {
    let identity = resolve_signing_identity(config).await?;
    sign_macos_binary(&identity, MACOS_CLIENT_SIGNING_IDENTIFIER, path).await
}

async fn sign_macos_binary(
    identity: &SigningIdentity,
    identifier: &str,
    path: &Path,
) -> Result<()> {
    let requirement = explicit_designated_requirement(identifier, &identity.sha1);
    let mut command = Command::new("/usr/bin/codesign");
    command
        .args(["--force", "--sign"])
        .arg(&identity.sha1)
        .args(["--identifier", identifier, "--requirements"])
        .arg(format!("={requirement}"))
        .args(["--options", "runtime", "--timestamp=none"])
        .arg(path)
        .stdin(Stdio::null());
    checked(command, "sign macOS binary", Duration::from_secs(60)).await?;
    Ok(())
}

async fn verify_client_signature(config: &OpsConfig, path: &Path) -> Result<()> {
    let identity = resolve_signing_identity(config).await?;
    verify_macos_signature(path, MACOS_CLIENT_SIGNING_IDENTIFIER, &identity.sha1).await
}

async fn verify_macos_signature(path: &Path, identifier: &str, sha1: &str) -> Result<()> {
    let mut verify = Command::new("/usr/bin/codesign");
    verify
        .args(["--verify", "--strict", "--verbose=2"])
        .arg(path)
        .stdin(Stdio::null());
    checked(
        verify,
        "verify macOS code signature",
        Duration::from_secs(30),
    )
    .await?;

    let expected = explicit_designated_requirement(identifier, sha1);
    let mut requirement_check = Command::new("/usr/bin/codesign");
    requirement_check
        .args(["--verify", "--strict", "--verbose=2"])
        .arg(format!("-R={expected}"))
        .arg(path)
        .stdin(Stdio::null());
    checked(
        requirement_check,
        "verify pinned macOS signing requirement",
        Duration::from_secs(30),
    )
    .await?;

    let requirement = inspect_designated_requirement(path).await?;
    let embedded_sha1 = certificate_leaf_sha1(&requirement)?;
    if !embedded_sha1.eq_ignore_ascii_case(sha1) {
        bail!("macOS binary is signed by an unpinned certificate");
    }
    if requirement != expected {
        bail!("macOS binary has an unexpected designated requirement: {requirement}");
    }
    tracing::info!(%identifier,certificate_sha1=%sha1,%requirement,"macOS signing identity verified");
    Ok(())
}

async fn inspect_designated_requirement(path: &Path) -> Result<String> {
    let mut inspect = Command::new("/usr/bin/codesign");
    inspect
        .args(["--display", "--requirements", "-"])
        .arg(path)
        .stdin(Stdio::null());
    let output = checked(
        inspect,
        "inspect macOS designated requirement",
        Duration::from_secs(30),
    )
    .await?;
    parse_designated_requirement(&output.stdout)
}

async fn verify_server_binary(
    config: &OpsConfig,
    path: &Path,
    sha: &str,
    sequence: u64,
) -> Result<()> {
    let parent = path.parent().context("server binary has no parent")?;
    let mut command = Command::new("docker");
    command
        .args(["run", "--rm", "--platform", "linux/amd64"])
        .args(["-v", &format!("{}:/probe", parent.display())])
        .arg(&config.linux_builder_image)
        .args(["/probe/nuntius-server", "build-info"]);
    let output = checked(command, "probe Linux server", Duration::from_secs(60)).await?;
    validate_build_info(
        &output.stdout,
        "nuntius-server",
        sha,
        sequence,
        LINUX_TARGET,
    )
}

fn validate_build_info(
    bytes: &[u8],
    expected_name: &str,
    sha: &str,
    sequence: u64,
    target: &str,
) -> Result<()> {
    let info: BuildInfo = serde_json::from_slice(bytes).context("decode binary build info")?;
    if info.name != expected_name
        || info.build_sha != sha
        || info.release_sequence != sequence
        || info.target != target
    {
        bail!("binary build identity mismatch: {info:?}");
    }
    Ok(())
}

async fn create_archive(binary: &Path, destination: &Path) -> Result<()> {
    let parent = binary.parent().context("binary has no parent directory")?;
    let name = binary.file_name().context("binary has no file name")?;
    let mut command = Command::new("tar");
    command
        .arg("--no-xattrs")
        .args(["-czf"])
        .arg(destination)
        .arg("-C")
        .arg(parent)
        .arg(name)
        .env("COPYFILE_DISABLE", "1");
    checked(command, "package release archive", Duration::from_secs(120)).await?;
    Ok(())
}

async fn deploy_release(config: &OpsConfig, output: &BuildOutput) -> Result<()> {
    update_state(config, |state| state.phase = "upload".into())?;
    let release_id = &output.release.release_id;
    let remote_server_dir = format!("{}/releases/{release_id}", config.remote_root);
    let remote_client_dir = format!("{}/releases/{release_id}", config.remote_data_dir);
    let remote_desired = format!("{}/releases/{DESIRED_CLIENT_FILE}", config.remote_data_dir);
    let remote_previous_desired = format!("{remote_desired}.previous");
    let remote_previous_binary = format!("{}.previous.ops", config.remote_binary);
    for path in [&remote_server_dir, &remote_client_dir] {
        remote_checked(
            config,
            [
                "install",
                "-d",
                "-o",
                &config.remote_user,
                "-g",
                &config.remote_group,
                "-m",
                "0700",
                path,
            ],
            "create remote release directory",
        )
        .await?;
    }

    let remote_server_archive = format!("{remote_server_dir}/{SERVER_ARCHIVE}");
    let remote_client_archive = format!("{remote_client_dir}/{CLIENT_ARCHIVE}");
    let remote_client_upload = format!("{remote_client_archive}.upload");
    let remote_desired_upload = format!("{remote_desired}.upload");
    scp(
        config,
        &output.package_dir.join(SERVER_ARCHIVE),
        &remote_server_archive,
    )
    .await?;
    scp(
        config,
        &output.package_dir.join(CLIENT_ARCHIVE),
        &remote_client_upload,
    )
    .await?;
    scp(
        config,
        &output.package_dir.join(DESIRED_CLIENT_FILE),
        &remote_desired_upload,
    )
    .await?;
    remote_checked(
        config,
        [
            "tar",
            "-xzf",
            &remote_server_archive,
            "--no-same-owner",
            "-C",
            &remote_server_dir,
        ],
        "extract remote server archive",
    )
    .await?;
    let candidate = format!("{remote_server_dir}/nuntius-server");
    let probe = remote_checked(
        config,
        [&candidate, "build-info"],
        "probe server on deployment target",
    )
    .await?;
    validate_build_info(
        &probe.stdout,
        "nuntius-server",
        &output.release.commit_sha,
        output.release.release_sequence,
        LINUX_TARGET,
    )?;

    remote_checked(
        config,
        [
            "install",
            "-o",
            &config.remote_user,
            "-g",
            &config.remote_group,
            "-m",
            "0600",
            &remote_client_upload,
            &remote_client_archive,
        ],
        "install client archive",
    )
    .await?;
    let previous_desired_exists = remote_status(config, ["test", "-f", &remote_desired]).await?;
    if previous_desired_exists {
        remote_checked(
            config,
            ["cp", "-p", &remote_desired, &remote_previous_desired],
            "backup desired client release",
        )
        .await?;
    } else {
        let _ = remote_checked(
            config,
            ["rm", "-f", &remote_previous_desired],
            "clear stale desired release backup",
        )
        .await?;
    }
    remote_checked(
        config,
        [
            "install",
            "-o",
            &config.remote_user,
            "-g",
            &config.remote_group,
            "-m",
            "0600",
            &remote_desired_upload,
            &remote_desired,
        ],
        "install desired client release",
    )
    .await?;
    remote_checked(
        config,
        ["rm", "-f", &remote_client_upload, &remote_desired_upload],
        "remove uploaded staging files",
    )
    .await?;
    remote_checked(
        config,
        ["cp", "-p", &config.remote_binary, &remote_previous_binary],
        "backup server binary",
    )
    .await?;
    let next_binary = format!("{}.next", config.remote_binary);
    remote_checked(
        config,
        [
            "install",
            "-o",
            &config.remote_user,
            "-g",
            &config.remote_group,
            "-m",
            "0755",
            &candidate,
            &next_binary,
        ],
        "stage server binary",
    )
    .await?;
    remote_checked(
        config,
        ["mv", &next_binary, &config.remote_binary],
        "activate server binary",
    )
    .await?;
    remote_checked(
        config,
        ["systemctl", "restart", &config.remote_service],
        "restart server service",
    )
    .await?;

    update_state(config, |state| state.phase = "verify".into())?;
    if let Err(error) = verify_deployment(config, &output.release).await {
        tracing::error!(error=?error,"deployment verification failed; rolling back");
        remote_checked(
            config,
            [
                "install",
                "-o",
                &config.remote_user,
                "-g",
                &config.remote_group,
                "-m",
                "0755",
                &remote_previous_binary,
                &config.remote_binary,
            ],
            "restore previous server binary",
        )
        .await?;
        if previous_desired_exists {
            remote_checked(
                config,
                [
                    "install",
                    "-o",
                    &config.remote_user,
                    "-g",
                    &config.remote_group,
                    "-m",
                    "0600",
                    &remote_previous_desired,
                    &remote_desired,
                ],
                "restore previous desired client release",
            )
            .await?;
        } else {
            remote_checked(
                config,
                ["rm", "-f", &remote_desired],
                "remove failed desired client release",
            )
            .await?;
        }
        remote_checked(
            config,
            ["systemctl", "restart", &config.remote_service],
            "restart rolled back server",
        )
        .await?;
        return Err(error).context("new release rolled back");
    }
    Ok(())
}

async fn verify_deployment(config: &OpsConfig, release: &ClientRelease) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match current_server_info(config).await {
            Ok(info)
                if info.build_sha == release.commit_sha
                    && info.release_sequence == release.release_sequence =>
            {
                break;
            }
            _ if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            _ => bail!("server did not become ready with the expected build"),
        }
    }
    let archive = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?
        .get(&release.url)
        .send()
        .await
        .context("download deployed client archive")?
        .error_for_status()
        .context("deployed client archive returned an error")?
        .bytes()
        .await?;
    if archive.len() as u64 != release.size
        || hex::encode(Sha256::digest(&archive)) != release.sha256
    {
        bail!("deployed client archive verification failed");
    }
    Ok(())
}

async fn current_server_info(config: &OpsConfig) -> Result<ServerInfo> {
    let url = format!(
        "{}/api/v1/info",
        config.public_base_url.trim_end_matches('/')
    );
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn scp(config: &OpsConfig, local: &Path, remote: &str) -> Result<()> {
    validate_remote_path("scp destination", remote)?;
    let mut command = Command::new(&config.scp_program);
    command
        .arg(local)
        .arg(format!("{}:{remote}", config.remote_host));
    checked(command, "upload release file", Duration::from_secs(600)).await?;
    Ok(())
}

async fn remote_checked<I, S>(
    config: &OpsConfig,
    args: I,
    label: &str,
) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(&config.ssh_program);
    command.arg(&config.remote_host).args(args);
    checked(command, label, Duration::from_secs(600)).await
}

async fn remote_status<I, S>(config: &OpsConfig, args: I) -> Result<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(&config.ssh_program);
    command.arg(&config.remote_host).args(args);
    Ok(
        output(command, "check remote state", Duration::from_secs(30))
            .await?
            .status
            .success(),
    )
}

async fn checked(command: Command, label: &str, timeout: Duration) -> Result<std::process::Output> {
    let result = output(command, label, timeout).await?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let detail: String = stderr
            .chars()
            .rev()
            .take(6000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        bail!("{label} exited with {}: {}", result.status, detail.trim());
    }
    Ok(result)
}

async fn output(
    mut command: Command,
    label: &str,
    timeout: Duration,
) -> Result<std::process::Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("{label} timed out"))?
        .with_context(|| format!("start {label}"))
}

fn allocate_sequence(config: &OpsConfig, remote_sequence: Option<u64>) -> Result<u64> {
    let mut allocated = 0;
    update_state(config, |state| {
        let now = now_millis();
        allocated = now
            .max(state.last_sequence.saturating_add(1))
            .max(remote_sequence.unwrap_or(0).saturating_add(1));
        state.last_sequence = allocated;
    })?;
    Ok(allocated)
}

fn prepare_state_dirs(config: &OpsConfig) -> Result<()> {
    for path in [
        config.state_dir.clone(),
        config.state_dir.join("builds"),
        config.state_dir.join("cache"),
        config.state_dir.join("bootstrap"),
        config.state_dir.join("ops-updates"),
        config.state_dir.join("run"),
    ] {
        fs::create_dir_all(&path)?;
    }
    let dockerfile = config.state_dir.join("bootstrap/server-builder.Dockerfile");
    let synchronized = fs::read(&dockerfile)
        .map(|contents| contents == SERVER_BUILDER_DOCKERFILE.as_bytes())
        .unwrap_or(false);
    if !synchronized {
        atomic_write(&dockerfile, SERVER_BUILDER_DOCKERFILE.as_bytes())?;
    }
    Ok(())
}

fn acquire_lock(config: &OpsConfig) -> Result<File> {
    let path = config.state_dir.join("ops.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    file.try_lock_exclusive()
        .context("another nuntius-ops process is already running")?;
    Ok(file)
}

fn load_state(config: &OpsConfig) -> Result<OpsState> {
    let path = config.state_dir.join("state.json");
    match fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("decode ops state")?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OpsState::default()),
        Err(error) => Err(error.into()),
    }
}

fn update_state(config: &OpsConfig, update: impl FnOnce(&mut OpsState)) -> Result<()> {
    let mut state = load_state(config)?;
    update(&mut state);
    state.updated_at = now_millis();
    atomic_write(
        &config.state_dir.join("state.json"),
        &serde_json::to_vec_pretty(&state)?,
    )
}

fn record_failure(config: &OpsConfig, sha: &str, error: &anyhow::Error) -> Result<()> {
    update_state(config, |state| {
        state.building_sha = Some(sha.into());
        state.phase = "failed".into();
        state.last_error = Some(format!("{error:#}"));
    })
}

fn cleanup_local_builds(config: &OpsConfig, keep_source: &Path) -> Result<()> {
    let keep = keep_source.parent().context("source build has no parent")?;
    let builds = config.state_dir.join("builds");
    let mut entries: Vec<_> = fs::read_dir(&builds)?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok());
    let removable = entries.len().saturating_sub(3);
    for entry in entries.into_iter().take(removable) {
        if entry.path() != keep {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn valid_signing_identity(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value != "-"
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && !value.contains('"')
}

fn valid_certificate_sha1(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn signing_identity_matches(output: &str, expected: &str) -> Vec<String> {
    let quoted = format!("\"{expected}\"");
    output
        .lines()
        .filter(|line| line.contains(&quoted))
        .filter_map(|line| {
            line.split_whitespace()
                .find(|token| valid_certificate_sha1(token))
                .map(str::to_owned)
        })
        .collect()
}

fn explicit_designated_requirement(identifier: &str, certificate_sha1: &str) -> String {
    format!(
        "identifier \"{identifier}\" and certificate leaf = H\"{}\"",
        certificate_sha1.to_ascii_uppercase()
    )
}

fn certificate_leaf_sha1(requirement: &str) -> Result<String> {
    let prefix = "certificate leaf = H\"";
    let start = requirement
        .find(prefix)
        .map(|index| index + prefix.len())
        .context("designated requirement does not pin the leaf certificate")?;
    let remainder = &requirement[start..];
    let end = remainder
        .find('"')
        .context("designated requirement has an unterminated certificate hash")?;
    let sha1 = &remainder[..end];
    if !valid_certificate_sha1(sha1) {
        bail!("designated requirement has an invalid certificate SHA-1")
    }
    Ok(sha1.to_ascii_uppercase())
}

fn parse_designated_requirement(output: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(output).context("codesign requirement output is not UTF-8")?;
    text.lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("# designated => ")
                .or_else(|| line.strip_prefix("designated => "))
                .map(str::to_owned)
        })
        .filter(|requirement| !requirement.is_empty())
        .context("codesign did not report a designated requirement")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn validate_remote_path(label: &str, path: &str) -> Result<()> {
    if !path.starts_with('/')
        || path.len() > 2048
        || path.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        })
        || path.split('/').any(|component| component == "..")
    {
        bail!("{label} must be a shell-safe absolute path");
    }
    Ok(())
}

fn validate_sha(sha: &str) -> Result<()> {
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid Git commit SHA");
    }
    Ok(())
}

fn safe_git_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        && !value.contains("..")
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn default_root() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .context("cannot resolve home directory")?
        .home_dir()
        .join(".nuntius-ops"))
}

fn startup_update_dir(config_path: &Path) -> PathBuf {
    let fallback = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let Ok(source) = fs::read_to_string(config_path) else {
        return fallback;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&source) else {
        return fallback;
    };
    value
        .get("state_dir")
        .and_then(toml::Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or(fallback)
}

fn default_config_path() -> Result<PathBuf> {
    Ok(default_root()?.join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_are_strict() {
        assert!(validate_remote_path("path", "/var/lib/nuntius/releases/r-1").is_ok());
        assert!(validate_remote_path("path", "/var/lib/../root").is_err());
        assert!(validate_remote_path("path", "relative/path").is_err());
    }

    #[test]
    fn release_sequence_advances_past_remote_and_local_state() {
        let temp = tempfile::tempdir().unwrap();
        let config = OpsConfig {
            state_dir: temp.path().to_path_buf(),
            ..OpsConfig::default()
        };
        prepare_state_dirs(&config).unwrap();
        update_state(&config, |state| state.last_sequence = 2_000_000_000_000).unwrap();
        let sequence = allocate_sequence(&config, Some(2_000_000_000_100)).unwrap();
        assert_eq!(sequence, 2_000_000_000_101);
    }

    #[test]
    fn code_signing_identity_must_not_be_ad_hoc() {
        assert!(valid_signing_identity("Nuntius Local Release"));
        assert!(valid_signing_identity(
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!valid_signing_identity("-"));
        assert!(!valid_signing_identity(""));
    }

    #[test]
    fn finds_and_pins_named_signing_identity() {
        let identities = "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Nuntius Local Release\"\n     1 valid identities found\n";
        assert_eq!(
            signing_identity_matches(identities, "Nuntius Local Release"),
            vec!["0123456789ABCDEF0123456789ABCDEF01234567"]
        );
        assert!(signing_identity_matches(identities, "Other Identity").is_empty());
        assert!(valid_certificate_sha1(
            "0123456789abcdef0123456789abcdef01234567"
        ));
    }

    #[test]
    fn parses_stable_designated_requirement() {
        let sha1 = "0123456789ABCDEF0123456789ABCDEF01234567";
        let expected = explicit_designated_requirement(MACOS_CLIENT_SIGNING_IDENTIFIER, sha1);
        let output = format!("Executable=/tmp/nuntius-client\ndesignated => {expected}\n");
        assert_eq!(
            parse_designated_requirement(output.as_bytes()).unwrap(),
            expected
        );
        assert_eq!(certificate_leaf_sha1(&expected).unwrap(), sha1);
    }

    #[test]
    fn startup_update_directory_is_read_without_full_config_validation() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let state_dir = temp.path().join("runtime");
        fs::write(
            &config_path,
            format!(
                "state_dir = {:?}\npoll_interval_seconds = 0\n",
                state_dir.display().to_string()
            ),
        )
        .unwrap();

        assert_eq!(startup_update_dir(&config_path), state_dir);
    }
}
