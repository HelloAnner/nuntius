mod api;
mod app_server;
mod assets;
mod config;
mod directory;
mod error;
mod executor;
mod pairing;
mod protocol;
mod store;
mod tunnel;

use anyhow::{Context, Result, bail};
use app_server::AppServerRuntime;
use clap::{Parser, Subcommand};
use config::ClientConfig;
use executor::CommandExecutor;
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::{Command as ProcessCommand, Stdio},
    sync::Arc,
};
use store::ClientStore;
use tower_http::{catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "nuntius-client",
    version,
    about = "Nuntius workstation agent and local console"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        force: bool,
    },
    Pair {
        #[arg(long)]
        code: String,
        #[arg(long)]
        server_url: Option<String>,
        #[arg(long)]
        allow_insecure_http: bool,
    },
    Run,
    Start,
    Stop,
    Status,
    Backup,
    Paths,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { force } => {
            let root = config::initialize(force)?;
            ClientStore::open(&root).await?;
            println!("initialized {}", root.display());
            println!("configuration: {}", config::config_path()?.display());
            Ok(())
        }
        Command::Pair {
            code,
            server_url,
            allow_insecure_http,
        } => {
            let mut cfg = ClientConfig::load()?;
            if let Some(url) = server_url {
                cfg.server_url = url
            }
            if allow_insecure_http {
                cfg.allow_insecure_http = true
            }
            let device_id = pairing::pair(&mut cfg, &code).await?;
            println!("paired device {device_id}");
            Ok(())
        }
        Command::Run => run().await,
        Command::Start => start(),
        Command::Stop => stop(),
        Command::Status => status(),
        Command::Backup => backup().await,
        Command::Paths => {
            println!("data: {}", config::data_dir()?.display());
            println!("config: {}", config::config_path()?.display());
            println!(
                "database: {}",
                config::data_dir()?.join(config::DATABASE_FILE).display()
            );
            println!("log: {}", config::log_path()?.display());
            Ok(())
        }
    }
}

async fn backup() -> Result<()> {
    if let Some(pid) = running_pid()? {
        bail!("stop nuntius-client before backup (running pid {pid})")
    }
    let _lock = acquire_data_lock()?;
    let root = config::data_dir()?;
    let store = ClientStore::open(&root).await?;
    let destination = root.join("backups").join(format!(
        "nuntius-client-{}.db",
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    store.backup(&destination).await?;
    config::private_file(&destination)?;
    println!("backup created {}", destination.display());
    Ok(())
}

async fn run() -> Result<()> {
    let cfg = Arc::new(ClientConfig::load()?);
    init_tracing(&cfg);
    let _pid = PidGuard::acquire()?;
    let root = config::data_dir()?;
    let store = ClientStore::open(&root).await?;
    store.recover_process_state().await?;
    let (events, _) = tokio::sync::broadcast::channel(4096);
    let app = AppServerRuntime::new(cfg.clone());
    let device_id = cfg.device_id.clone().unwrap_or_else(|| "unpaired".into());
    let executor = CommandExecutor {
        config: cfg.clone(),
        store,
        app: app.clone(),
        device_id,
        events,
    };
    let maintenance_store = executor.store.clone();
    let maintenance_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = maintenance_store.maintenance().await {
                tracing::warn!(error=?error, "client maintenance failed");
            }
        }
    });
    let app_events_task = tokio::spawn(executor::process_app_events(executor.clone()));
    let discovery = executor.clone();
    let discovery_task = tokio::spawn(async move {
        match discovery.discover_all().await {
            Ok(count) => tracing::info!(count, "Codex history discovery completed"),
            Err(error) => {
                tracing::warn!(error=?error, "Codex history discovery unavailable; local API remains online")
            }
        }
    });
    let tunnel_task = cfg
        .device_id
        .as_ref()
        .map(|_| tokio::spawn(tunnel::run_forever(executor.clone())));
    let router = api::router(executor)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(cfg.local_bind)
        .await
        .with_context(|| format!("cannot bind local console {}", cfg.local_bind))?;
    tracing::info!(bind=%cfg.local_bind,paired=cfg.device_id.is_some(),"Nuntius client running");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    if let Some(task) = tunnel_task {
        task.abort()
    }
    app.shutdown().await?;
    discovery_task.abort();
    app_events_task.abort();
    maintenance_task.abort();
    Ok(())
}

fn start() -> Result<()> {
    if let Some(pid) = running_pid()? {
        bail!("nuntius-client is already running with pid {pid}")
    }
    let executable = std::env::current_exe()?;
    let log_path = config::log_path()?;
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = ProcessCommand::new(executable);
    command
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    let child = command.spawn()?;
    println!("started nuntius-client with pid {}", child.id());
    println!("log: {}", log_path.display());
    Ok(())
}
fn stop() -> Result<()> {
    let Some(pid) = running_pid()? else {
        println!("nuntius-client is not running");
        return Ok(());
    };
    #[cfg(unix)]
    {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        )?;
    }
    #[cfg(windows)]
    {
        let status = ProcessCommand::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .status()?;
        if !status.success() {
            bail!("taskkill failed")
        }
    }
    println!("sent shutdown signal to pid {pid}");
    Ok(())
}
fn status() -> Result<()> {
    match running_pid()? {
        Some(pid) => println!("running (pid {pid})"),
        None => println!("stopped"),
    };
    Ok(())
}

struct PidGuard {
    path: PathBuf,
    _lock: std::fs::File,
}
impl PidGuard {
    fn acquire() -> Result<Self> {
        let path = config::pid_path()?;
        if let Some(pid) = running_pid()? {
            bail!("another nuntius-client is already running with pid {pid}")
        }
        let lock = acquire_data_lock()?;
        fs::write(&path, std::process::id().to_string())?;
        Ok(Self { path, _lock: lock })
    }
}
fn acquire_data_lock() -> Result<std::fs::File> {
    let lock_path = config::data_dir()?.join("run/client.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .context("another nuntius-client process owns the data directory")?;
    Ok(lock)
}
impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
fn running_pid() -> Result<Option<u32>> {
    let path = config::pid_path()?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    let pid = text.trim().parse::<u32>().context("invalid pid file")?;
    if process_alive(pid) && data_lock_is_held()? {
        Ok(Some(pid))
    } else {
        let _ = fs::remove_file(path);
        Ok(None)
    }
}
fn data_lock_is_held() -> Result<bool> {
    let path = config::data_dir()?.join("run/client.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}
#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

fn init_tracing(config: &ClientConfig) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("nuntius_client=info,tower_http=info"));
    if config.log_format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init()
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init()
    }
}
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let _ = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {_=ctrl_c=>{},_=terminate=>{}}
}
