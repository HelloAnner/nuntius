mod api;
mod assets;
mod attachments;
mod auth;
mod config;
mod error;
mod event_hub;
mod protocol;
mod releases;
mod store;
mod tunnel;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{ServerConfig, initialize_data_dir};
use event_hub::EventHub;
use nuntius_updater::BuildInfo;
use protocol::TransportSecurity;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use store::ServerStore;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(
    name = "nuntius-server",
    version,
    about = "Nuntius public control server"
)]
struct Cli {
    #[arg(long, env = "NUNTIUS_SERVER_DATA_DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        force: bool,
    },
    Serve,
    Backup,
    BuildInfo,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServerConfig>,
    pub data_dir: Arc<PathBuf>,
    pub store: ServerStore,
    pub events: EventHub,
    pub tunnels: Arc<tunnel::TunnelRegistry>,
    pub releases: releases::ReleaseStore,
}

impl AppState {
    pub fn transport_security(&self) -> TransportSecurity {
        if self.config.is_secure() {
            TransportSecurity::Secure
        } else {
            TransportSecurity::Insecure
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { force } => {
            let data_dir = required_data_dir(cli.data_dir)?;
            let result = initialize_data_dir(&data_dir, force)?;
            ServerStore::open(&result.data_dir)
                .await
                .context("initialize server SQLite")?;
            println!("initialized {}", result.data_dir.display());
            println!("bootstrap token: {}", result.bootstrap_token);
            println!(
                "edit {} before serving",
                result.data_dir.join(config::CONFIG_FILE).display()
            );
            Ok(())
        }
        Command::Serve => {
            let data_dir = required_data_dir(cli.data_dir)?;
            serve(data_dir).await
        }
        Command::Backup => backup(required_data_dir(cli.data_dir)?).await,
        Command::BuildInfo => {
            println!(
                "{}",
                serde_json::to_string(&BuildInfo::current(
                    "nuntius-server",
                    env!("CARGO_PKG_VERSION")
                ))?
            );
            Ok(())
        }
    }
}

fn required_data_dir(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    data_dir.context("--data-dir or NUNTIUS_SERVER_DATA_DIR is required for this command")
}

async fn backup(data_dir: PathBuf) -> Result<()> {
    let _data_dir_lock = config::DataDirLock::acquire(&data_dir)?;
    let store = ServerStore::open(&data_dir)
        .await
        .context("open server SQLite")?;
    let backup_name = format!(
        "nuntius-server-{}",
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let destination = data_dir.join("backups").join(backup_name);
    fs::create_dir(&destination)?;
    config::set_private_dir_permissions(&destination)?;
    let database = destination.join(config::DATABASE_FILE);
    store.backup(&database).await?;
    config::set_private_file_permissions(&database)?;
    copy_private_tree(
        &data_dir.join("attachments"),
        &destination.join("attachments"),
    )?;
    println!("backup created {}", destination.display());
    Ok(())
}

fn copy_private_tree(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir(destination)?;
    config::set_private_dir_permissions(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_private_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
            config::set_private_file_permissions(&target)?;
        } else {
            anyhow::bail!(
                "refusing to back up symlink or special file {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

async fn serve(data_dir: PathBuf) -> Result<()> {
    let _data_dir_lock = config::DataDirLock::acquire(&data_dir)?;
    let config = ServerConfig::load(&data_dir)?;
    let _log_guard = init_tracing(&config, &data_dir);
    let store = ServerStore::open(&data_dir)
        .await
        .context("open server SQLite")?;
    let releases = releases::ReleaseStore::load(&data_dir, &config.public_base_url).await?;
    let state = AppState {
        config: Arc::new(config.clone()),
        data_dir: Arc::new(data_dir.clone()),
        store,
        events: EventHub::new(4096),
        tunnels: tunnel::TunnelRegistry::new(),
        releases,
    };
    let maintenance_store = state.store.clone();
    let retention_hours = config.event_retention_hours;
    let maintenance_task = tokio::spawn(async move {
        let cadence = std::time::Duration::from_secs(300);
        // Let reconnecting devices publish their current runtime inventory
        // before retention work competes for SQLite's single writer.
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + cadence, cadence);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = maintenance_store.maintenance(retention_hours).await {
                tracing::warn!(error=?error, "server maintenance failed");
            }
        }
    });
    let app = api::router(state.clone())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind=%config.bind,public_base_url=%config.public_base_url,secure=config.is_secure(),"nuntius server listening");
    let release_task = releases::spawn_watcher(
        data_dir.clone(),
        config.public_base_url.clone(),
        state.releases.clone(),
        state.tunnels.clone(),
    );
    let (graceful_tx, graceful_rx) = tokio::sync::oneshot::channel();
    let mut graceful_tx = Some(graceful_tx);
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = graceful_rx.await;
        }),
    );
    tokio::pin!(server);
    let external_shutdown = shutdown_signal();
    tokio::pin!(external_shutdown);
    tokio::select! {
        result = &mut server => result?,
        _ = &mut external_shutdown => {
            if let Some(tx) = graceful_tx.take() { let _ = tx.send(()); }
            match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, &mut server).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!("forcing shutdown after live connections exceeded drain timeout"),
            }
        }
    }
    release_task.abort();
    maintenance_task.abort();
    Ok(())
}

fn init_tracing(
    config: &ServerConfig,
    data_dir: &std::path::Path,
) -> tracing_appender::non_blocking::WorkerGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("nuntius_server=info,nuntius_updater=info,tower_http=info")
    });
    let appender = tracing_appender::rolling::never(data_dir.join("logs"), "nuntius-server.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    if config.log_format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .json()
            .init()
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .init()
    }
    guard
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
