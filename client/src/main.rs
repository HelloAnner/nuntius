mod agent;
#[cfg(unix)]
mod agent_host;
mod api;
mod app_server;
mod assets;
mod attachments;
mod command_queue;
mod config;
mod directory;
mod error;
mod executor;
mod history_monitor;
mod kimi;
mod pairing;
mod pi;
mod protocol;
mod runtime_reconciler;
mod service;
mod store;
mod tunnel;

use agent::AgentRuntimes;
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::ClientConfig;
use executor::CommandExecutor;
use fs2::FileExt;
use nuntius_updater::{BuildInfo, UpdateConfig};
#[cfg(not(target_os = "macos"))]
use std::process::{Command as ProcessCommand, Stdio};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use store::ClientStore;
use tower_http::{catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const RUNTIME_HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const UPDATE_STARTUP_PROBATION: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum RunStopReason {
    Server,
    External,
    Update,
    CriticalTask(&'static str),
}

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
    /// Initialize, pair, and start this device using a securely prompted one-time code.
    Setup {
        #[arg(long)]
        server_url: String,
        #[arg(long)]
        allow_insecure_http: bool,
        #[arg(long)]
        display_name: Option<String>,
    },
    Run,
    Start,
    Stop,
    Status,
    Backup,
    Paths,
    BuildInfo,
    #[cfg(unix)]
    #[command(hide = true)]
    AgentHost,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Run update recovery before clap parses the daemon command. A candidate
    // that accidentally breaks the `run` subcommand must still advance the
    // boot marker so launchd's next attempt can restore the previous binary.
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("run")) {
        nuntius_updater::handle_startup(&config::data_dir()?)?;
    }
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
        Command::Setup {
            server_url,
            allow_insecure_http,
            display_name,
        } => setup_device(server_url, allow_insecure_http, display_name).await,
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
        Command::BuildInfo => {
            println!(
                "{}",
                serde_json::to_string(&BuildInfo::current(
                    "nuntius-client",
                    env!("CARGO_PKG_VERSION")
                ))?
            );
            Ok(())
        }
        #[cfg(unix)]
        Command::AgentHost => run_agent_host().await,
    }
}

#[cfg(unix)]
async fn run_agent_host() -> Result<()> {
    let config = Arc::new(ClientConfig::load()?);
    init_tracing(&config);
    agent_host::run(config).await
}

#[derive(Debug, PartialEq, Eq)]
enum SetupDisposition {
    Pair,
    AlreadyPaired(String),
}

async fn setup_device(
    server_url: String,
    allow_insecure_http: bool,
    display_name: Option<String>,
) -> Result<()> {
    let config_path = config::config_path()?;
    let initialized = !config_path.exists();
    if initialized {
        let root = config::initialize(false)?;
        println!("initialized {}", root.display());
    }

    let root = config::data_dir()?;
    ClientStore::open(&root).await?;
    let mut cfg = ClientConfig::load()?;
    match configure_setup(
        &mut cfg,
        &server_url,
        allow_insecure_http,
        display_name.as_deref(),
    )? {
        SetupDisposition::AlreadyPaired(device_id) => {
            println!("device {device_id} is already paired with {server_url}");
            return start_if_stopped();
        }
        SetupDisposition::Pair => {}
    }

    if running_pid()?.is_some() {
        println!("stopping the unpaired client before setup");
        stop()?;
    }
    // Persist and fsync the target configuration before consuming the one-time
    // code. pairing::pair performs a second atomic save with the device id.
    cfg.save()?;

    println!("请在 Server 的“设置 → 设备配对”中生成一次性验证码。");
    let code = rpassword::prompt_password("请输入一次性验证码: ")?;
    if code.trim().is_empty() {
        bail!("the one-time pairing code cannot be empty")
    }
    let device_id = pairing::pair(&mut cfg, &code).await?;
    println!("paired device {device_id}");
    start_if_stopped()
}

fn configure_setup(
    config: &mut ClientConfig,
    server_url: &str,
    allow_insecure_http: bool,
    display_name: Option<&str>,
) -> Result<SetupDisposition> {
    let requested_url = url::Url::parse(server_url).context("server_url is invalid")?;
    if let Some(device_id) = &config.device_id {
        let existing_url =
            url::Url::parse(&config.server_url).context("configured server_url is invalid")?;
        if existing_url != requested_url {
            bail!(
                "this client is already paired with {}; refusing to replace it with {}",
                config.server_url,
                server_url
            )
        }
        return Ok(SetupDisposition::AlreadyPaired(device_id.clone()));
    }

    config.server_url = requested_url.to_string();
    config.allow_insecure_http = allow_insecure_http;
    if let Some(display_name) = display_name {
        let display_name = display_name.trim();
        if display_name.is_empty() || display_name.len() > 128 {
            bail!("display_name must contain 1 to 128 bytes")
        }
        config.display_name = display_name.into();
    }
    config.validate()?;
    Ok(SetupDisposition::Pair)
}

fn start_if_stopped() -> Result<()> {
    if let Some(pid) = running_pid()? {
        println!("nuntius-client is already running with pid {pid}");
        Ok(())
    } else {
        start()
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
        "nuntius-client-{}",
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    fs::create_dir(&destination)?;
    config::private_dir(&destination)?;
    let database = destination.join(config::DATABASE_FILE);
    store.backup(&database).await?;
    config::private_file(&database)?;
    copy_private_tree(&root.join("attachments"), &destination.join("attachments"))?;
    println!("backup created {}", destination.display());
    Ok(())
}

fn copy_private_tree(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir(destination)?;
    config::private_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_private_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
            config::private_file(&target)?;
        } else {
            anyhow::bail!(
                "refusing to back up symlink or special file {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

async fn run() -> Result<()> {
    let cfg = Arc::new(ClientConfig::load()?);
    init_tracing(&cfg);
    #[cfg(target_os = "macos")]
    service::ensure_agent_host()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    agent_host::ensure_started()?;
    let _pid = PidGuard::acquire()?;
    let root = config::data_dir()?;
    let mut update_probation_pending = nuntius_updater::startup_update_pending(&root)?;
    let store = ClientStore::open(&root).await?;
    let recovery_candidates = store.recover_process_state().await?;
    let (events, _) = tokio::sync::broadcast::channel(4096);
    let (command_acks, _) = tokio::sync::broadcast::channel(1024);
    let agents = AgentRuntimes::new(cfg.clone())?;
    let device_id = cfg.device_id.clone().unwrap_or_else(|| "unpaired".into());
    let executor = CommandExecutor {
        config: cfg.clone(),
        store,
        agents: agents.clone(),
        device_id,
        display_name: Arc::new(tokio::sync::RwLock::new(cfg.display_name.clone())),
        events,
        command_acks,
        command_notify: Arc::new(tokio::sync::Notify::new()),
        history_import_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    // Subscribe before `thread/resume`: resumed threads may begin streaming
    // notifications as soon as the App Server accepts the request.
    let app_event_receiver = agents.codex.subscribe();
    let codex_event_stream_task = tokio::spawn(agents.codex.clone().run_event_stream());
    let app_events_task = tokio::spawn(executor::process_app_events(
        executor.clone(),
        app_event_receiver,
    ));
    let kimi_event_stream_task = tokio::spawn(agents.kimi.clone().run_event_stream());
    let kimi_events_task = tokio::spawn(executor::process_kimi_events(executor.clone()));
    let pi_events_task = tokio::spawn(executor::process_pi_events(executor.clone()));
    if !recovery_candidates.is_empty() {
        tracing::info!(
            count = recovery_candidates.len(),
            "recovering running threads after restart"
        );
    }
    // This is the startup synchronization barrier. The public tunnel must not
    // project the old process state until every candidate received one resume
    // attempt and was either resolved or explicitly left as `recovering`.
    let pending_recovery = executor.recover_threads_once(&recovery_candidates).await;
    if !pending_recovery.is_empty() {
        // The Agent Host keeps provider work alive across the Client process
        // replacement. Finish reattaching every durable runtime projection
        // before commands, HTTP and the public device tunnel can expose the new
        // process as ready.
        executor.retry_thread_recovery(pending_recovery).await;
    }
    let command_queue_task = tokio::spawn(command_queue::run(executor.clone()));
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
    let history_monitor_task = tokio::spawn(history_monitor::run(executor.clone()));
    let runtime_reconciler_task = tokio::spawn(runtime_reconciler::run(executor.clone()));
    let discovery = executor.clone();
    let discovery_task = tokio::spawn(async move {
        match discovery.discover_all().await {
            Ok(count) => tracing::info!(count, "agent history discovery completed"),
            Err(error) => {
                tracing::warn!(error=?error, "agent history discovery unavailable; local API remains online")
            }
        }
    });
    let (desired_release_tx, desired_release_rx) = tokio::sync::watch::channel(None);
    let tunnel_task = cfg
        .device_id
        .as_ref()
        .map(|_| tokio::spawn(tunnel::run_forever(executor.clone(), desired_release_tx)));
    let runtime_store = executor.store.clone();
    let runtime_agents = agents.clone();
    let router = api::router(executor)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(cfg.local_bind)
        .await
        .with_context(|| format!("cannot bind local console {}", cfg.local_bind))?;
    let probation_started = tokio::time::Instant::now();
    tracing::info!(bind=%cfg.local_bind,paired=cfg.device_id.is_some(),"Nuntius client running");
    let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(1);
    let update_task = cfg.auto_update.then(|| {
        let update = UpdateConfig::client(
            "nuntius-client",
            "aarch64-apple-darwin",
            root.clone(),
            Duration::from_secs(cfg.update_interval_seconds),
            cfg.server_url.clone(),
        );
        nuntius_updater::spawn_client_update_worker(update, desired_release_rx, update_tx)
    });
    let (graceful_tx, graceful_rx) = tokio::sync::oneshot::channel();
    let mut graceful_tx = Some(graceful_tx);
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = graceful_rx.await;
        }),
    );
    tokio::pin!(server);
    let external_shutdown = shutdown_signal();
    tokio::pin!(external_shutdown);
    let mut prepared_update = None;
    let mut update_rx_open = update_task.is_some();
    let mut runtime_health = tokio::time::interval(RUNTIME_HEALTH_INTERVAL);
    runtime_health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next_host_upgrade_check = tokio::time::Instant::now();
    let stop_reason = loop {
        tokio::select! {
            result = &mut server => {
                result?;
                break RunStopReason::Server;
            }
            _ = &mut external_shutdown => break RunStopReason::External,
            update = update_rx.recv(), if update_rx_open => {
                update_rx_open = false;
                prepared_update = update;
                if let Some(update) = prepared_update.as_ref() {
                    tracing::info!(target=%update.target_sha(), "client update staged; activating immediately while provider work remains in the Agent Host");
                    if client_update_ready(true, update_probation_pending) {
                        break RunStopReason::Update;
                    }
                }
            }
            _ = runtime_health.tick() => {
                if let Some(name) = finished_critical_task(
                    &codex_event_stream_task,
                    &app_events_task,
                    &kimi_event_stream_task,
                    &kimi_events_task,
                    &pi_events_task,
                    &command_queue_task,
                    &maintenance_task,
                    &history_monitor_task,
                    &runtime_reconciler_task,
                    tunnel_task.as_ref(),
                ) {
                    tracing::error!(task=name, "critical client task exited unexpectedly");
                    break RunStopReason::CriticalTask(name);
                }
                if update_probation_pending
                    && probation_started.elapsed() >= UPDATE_STARTUP_PROBATION
                {
                    match nuntius_updater::mark_healthy(&root) {
                        Ok(()) => update_probation_pending = false,
                        Err(error) => {
                            tracing::error!(error=?error, "cannot commit client update health marker");
                            break RunStopReason::CriticalTask("self_update_health_marker");
                        }
                    }
                }
                if client_update_ready(prepared_update.is_some(), update_probation_pending) {
                    break RunStopReason::Update;
                }
                if !update_probation_pending
                    && tokio::time::Instant::now() >= next_host_upgrade_check
                {
                    next_host_upgrade_check = tokio::time::Instant::now() + Duration::from_secs(60);
                    match host_upgrade_blocker(&runtime_store).await {
                        Ok(None) => match runtime_agents.request_host_upgrade_if_idle().await {
                            Ok(true) => tracing::info!("rotating idle Agent Host to the current Client release"),
                            Ok(false) => {}
                            Err(error) => tracing::warn!(error=?error, "Agent Host release check failed"),
                        },
                        Ok(Some(_)) => {}
                        Err(error) => tracing::warn!(error=?error, "cannot inspect Agent Host upgrade window"),
                    }
                }
            }
        }
    };
    if !matches!(stop_reason, RunStopReason::Server) {
        if let Some(tx) = graceful_tx.take() {
            let _ = tx.send(());
        }
        match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, &mut server).await {
            Ok(result) => result?,
            Err(_) if matches!(stop_reason, RunStopReason::Update) => {
                tracing::warn!("forcing update after live connections exceeded drain timeout")
            }
            Err(_) => {
                tracing::warn!("forcing shutdown after live connections exceeded drain timeout")
            }
        }
    }
    if let Some(task) = update_task {
        task.abort();
    }
    if let Some(task) = tunnel_task {
        task.abort()
    }
    discovery_task.abort();
    history_monitor_task.abort();
    runtime_reconciler_task.abort();
    codex_event_stream_task.abort();
    app_events_task.abort();
    kimi_event_stream_task.abort();
    kimi_events_task.abort();
    pi_events_task.abort();
    command_queue_task.abort();
    maintenance_task.abort();
    agents.shutdown().await?;
    if let RunStopReason::CriticalTask(name) = stop_reason {
        bail!("critical client task exited unexpectedly: {name}")
    }
    if let Some(update) = prepared_update {
        tracing::info!(target=%update.target_sha(), "activating self-update");
        update.activate()?;
    }
    Ok(())
}

fn finished_critical_task(
    codex_event_stream: &tokio::task::JoinHandle<()>,
    app_events: &tokio::task::JoinHandle<()>,
    kimi_event_stream: &tokio::task::JoinHandle<()>,
    kimi_events: &tokio::task::JoinHandle<()>,
    pi_events: &tokio::task::JoinHandle<()>,
    command_queue: &tokio::task::JoinHandle<()>,
    maintenance: &tokio::task::JoinHandle<()>,
    history_monitor: &tokio::task::JoinHandle<()>,
    runtime_reconciler: &tokio::task::JoinHandle<()>,
    tunnel: Option<&tokio::task::JoinHandle<()>>,
) -> Option<&'static str> {
    [
        ("codex_event_stream", codex_event_stream),
        ("app_events", app_events),
        ("kimi_event_stream", kimi_event_stream),
        ("kimi_events", kimi_events),
        ("pi_events", pi_events),
        ("command_queue", command_queue),
        ("maintenance", maintenance),
        ("history_monitor", history_monitor),
        ("runtime_reconciler", runtime_reconciler),
    ]
    .into_iter()
    .find_map(|(name, task)| task.is_finished().then_some(name))
    .or_else(|| {
        tunnel
            .is_some_and(|task| task.is_finished())
            .then_some("device_tunnel")
    })
}

async fn host_upgrade_blocker(store: &ClientStore) -> Result<Option<String>> {
    let (_, inbox, _, active) = store.counts().await?;
    let approvals = store.pending_approval_count().await?;
    Ok(host_upgrade_blocker_for_counts(inbox, active, approvals))
}

fn host_upgrade_blocker_for_counts(inbox: i64, active: i64, approvals: i64) -> Option<String> {
    if active > 0 {
        return Some(format!("{active} active or recovering thread(s)"));
    }
    if inbox > 0 {
        return Some(format!("{inbox} accepted or applying command(s)"));
    }
    if approvals > 0 {
        return Some(format!("{approvals} pending approval(s)"));
    }
    None
}

fn client_update_ready(prepared: bool, probation_pending: bool) -> bool {
    prepared && !probation_pending
}

fn start() -> Result<()> {
    if let Some(pid) = running_pid()? {
        bail!("nuntius-client is already running with pid {pid}")
    }
    #[cfg(target_os = "macos")]
    {
        return service::start();
    }
    #[cfg(not(target_os = "macos"))]
    {
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
}
fn stop() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let managed_pid = running_pid()?;
        if service::stop()? {
            if let Some(pid) = managed_pid {
                wait_until_stopped(pid)?;
            }
            println!("stopped nuntius-client launchd service");
            return Ok(());
        }
    }
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
    wait_until_stopped(pid)?;
    println!("stopped nuntius-client pid {pid}");
    Ok(())
}

fn wait_until_stopped(pid: u32) -> Result<()> {
    let deadline = std::time::Instant::now() + STOP_WAIT_TIMEOUT;
    loop {
        if !process_alive(pid) || !data_lock_is_held()? {
            let _ = running_pid()?;
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("nuntius-client pid {pid} did not stop within 15 seconds")
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
}
fn status() -> Result<()> {
    match running_pid()? {
        Some(pid) => {
            #[cfg(target_os = "macos")]
            if service::is_loaded()? {
                println!("running (pid {pid}, supervised by launchd)");
                return Ok(());
            }
            println!("running (pid {pid})");
        }
        None => {
            #[cfg(target_os = "macos")]
            if service::is_loaded()? {
                println!("recovering (supervised by launchd)");
                return Ok(());
            }
            println!("stopped");
        }
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
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("nuntius_client=info,nuntius_updater=info,tower_http=info")
    });
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

#[cfg(test)]
mod setup_tests {
    use super::*;

    #[test]
    fn configures_an_unpaired_client() {
        let mut config = ClientConfig::default();

        let disposition = configure_setup(
            &mut config,
            "http://47.97.154.221:8765/",
            true,
            Some("Work Mac"),
        )
        .unwrap();

        assert_eq!(disposition, SetupDisposition::Pair);
        assert_eq!(config.server_url, "http://47.97.154.221:8765/");
        assert!(config.allow_insecure_http);
        assert_eq!(config.display_name, "Work Mac");
    }

    #[test]
    fn preserves_a_pairing_with_the_same_server() {
        let mut config = ClientConfig {
            server_url: "http://47.97.154.221:8765".into(),
            allow_insecure_http: true,
            device_id: Some("dev_existing".into()),
            ..ClientConfig::default()
        };

        let disposition =
            configure_setup(&mut config, "http://47.97.154.221:8765/", true, None).unwrap();

        assert_eq!(
            disposition,
            SetupDisposition::AlreadyPaired("dev_existing".into())
        );
    }

    #[test]
    fn refuses_to_move_an_existing_pairing() {
        let mut config = ClientConfig {
            server_url: "https://old.example.com/".into(),
            device_id: Some("dev_existing".into()),
            ..ClientConfig::default()
        };

        let result = configure_setup(&mut config, "https://new.example.com/", false, None);

        assert!(result.is_err());
        assert_eq!(config.server_url, "https://old.example.com/");
        assert_eq!(config.device_id.as_deref(), Some("dev_existing"));
    }

    #[test]
    fn rejects_an_empty_display_name() {
        let mut config = ClientConfig::default();

        let result = configure_setup(
            &mut config,
            "https://nuntius.example.com/",
            false,
            Some("  "),
        );

        assert!(result.is_err());
    }

    #[test]
    fn requires_explicit_consent_for_public_http() {
        let mut config = ClientConfig::default();

        let result = configure_setup(&mut config, "http://47.97.154.221:8765/", false, None);

        assert!(result.is_err());
    }

    #[test]
    fn agent_host_upgrade_waits_for_active_work() {
        assert_eq!(host_upgrade_blocker_for_counts(0, 0, 0), None);
        assert_eq!(
            host_upgrade_blocker_for_counts(0, 2, 0).as_deref(),
            Some("2 active or recovering thread(s)")
        );
        assert_eq!(
            host_upgrade_blocker_for_counts(1, 0, 0).as_deref(),
            Some("1 accepted or applying command(s)")
        );
        assert_eq!(
            host_upgrade_blocker_for_counts(0, 0, 3).as_deref(),
            Some("3 pending approval(s)")
        );
    }

    #[test]
    fn client_update_activation_never_waits_for_conversations() {
        assert!(client_update_ready(true, false));
        assert!(!client_update_ready(false, false));
        // Startup probation protects the rollback chain; conversation counts
        // are deliberately absent from the Client activation decision.
        assert!(!client_update_ready(true, true));
    }
}
