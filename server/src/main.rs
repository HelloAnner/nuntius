mod api;
mod assets;
mod auth;
mod config;
mod error;
mod event_hub;
mod protocol;
mod store;
mod tunnel;
mod update_relay;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{ServerConfig, initialize_data_dir};
use event_hub::EventHub;
use nuntius_updater::{BuildInfo, UpdateConfig, UpdateRole};
use protocol::TransportSecurity;
use std::{path::PathBuf, sync::Arc, time::Duration};
use store::ServerStore;
use tower_http::{catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

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
    ReceiveUpdate {
        #[arg(long)]
        commit_sha: String,
        #[arg(long)]
        archive_sha256: String,
        #[arg(long)]
        source_device_id: String,
    },
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServerConfig>,
    pub data_dir: Arc<PathBuf>,
    pub store: ServerStore,
    pub events: EventHub,
    pub tunnels: Arc<tunnel::TunnelRegistry>,
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
            nuntius_updater::handle_startup(&data_dir)?;
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
        Command::ReceiveUpdate {
            commit_sha,
            archive_sha256,
            source_device_id,
        } => {
            let data_dir = required_data_dir(cli.data_dir)?;
            update_relay::receive(&data_dir, commit_sha, archive_sha256, source_device_id).await
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
    let file_name = format!(
        "nuntius-server-{}.db",
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let destination = data_dir.join("backups").join(file_name);
    store.backup(&destination).await?;
    config::set_private_file_permissions(&destination)?;
    println!("backup created {}", destination.display());
    Ok(())
}

async fn serve(data_dir: PathBuf) -> Result<()> {
    let _data_dir_lock = config::DataDirLock::acquire(&data_dir)?;
    let config = ServerConfig::load(&data_dir)?;
    let _log_guard = init_tracing(&config, &data_dir);
    let store = ServerStore::open(&data_dir)
        .await
        .context("open server SQLite")?;
    let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(1);
    let state = AppState {
        config: Arc::new(config.clone()),
        data_dir: Arc::new(data_dir.clone()),
        store,
        events: EventHub::new(4096),
        tunnels: tunnel::TunnelRegistry::new(),
    };
    let maintenance_store = state.store.clone();
    let retention_hours = config.event_retention_hours;
    let maintenance_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = maintenance_store.maintenance(retention_hours).await {
                tracing::warn!(error=?error, "server maintenance failed");
            }
        }
    });
    let app = api::router(state)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let health_data_dir = data_dir.clone();
    let health_marker_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        if let Err(error) = nuntius_updater::mark_healthy(&health_data_dir) {
            tracing::warn!(error=?error, "cannot mark self-update healthy");
        }
    });
    tracing::info!(bind=%config.bind,public_base_url=%config.public_base_url,secure=config.is_secure(),"nuntius server listening");
    let update_task = config.auto_update.then(|| {
        nuntius_updater::spawn_update_loop(
            UpdateConfig::production(
                UpdateRole::Server,
                "nuntius-server",
                "x86_64-unknown-linux-gnu",
                data_dir.clone(),
                Duration::from_secs(config.update_interval_seconds),
            ),
            update_tx.clone(),
        )
    });
    let update_relay_task = config
        .auto_update
        .then(|| update_relay::spawn(data_dir.clone(), Duration::from_secs(5), update_tx));
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
    let mut prepared_update = None;
    tokio::select! {
        result = &mut server => result?,
        _ = &mut external_shutdown => {
            if let Some(tx) = graceful_tx.take() { let _ = tx.send(()); }
            (&mut server).await?;
        }
        update = update_rx.recv(), if config.auto_update => {
            prepared_update = update;
            if let Some(tx) = graceful_tx.take() { let _ = tx.send(()); }
            (&mut server).await?;
        }
    }
    if let Some(task) = update_task {
        task.abort();
    }
    if let Some(task) = update_relay_task {
        task.abort();
    }
    health_marker_task.abort();
    maintenance_task.abort();
    if let Some(update) = prepared_update {
        tracing::info!(target=%update.target_sha(), "activating self-update");
        update.activate()?;
    }
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
