use crate::{
    app_server::{
        AgentHostMessage, AgentHostRequest, AgentHostRpcError, AgentHostStatus, AppServerCallError,
        AppServerProcess,
    },
    config::{self, ClientConfig},
    kimi::KimiRuntime,
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex, Notify, RwLock, broadcast},
};

const EVENT_JOURNAL_CAPACITY: usize = 32_768;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
struct HostState {
    codex: AppServerProcess,
    kimi: KimiRuntime,
    journal: Arc<Mutex<EventJournal>>,
    live_events: broadcast::Sender<JournalEvent>,
    shutdown: Arc<Notify>,
    operation_gate: Arc<RwLock<()>>,
    stopping: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
struct JournalEvent {
    sequence: u64,
    payload: Value,
}

struct EventJournal {
    generation: String,
    next_sequence: u64,
    events: VecDeque<JournalEvent>,
}

struct HostGuard {
    socket_path: PathBuf,
    pid_path: PathBuf,
    _lock: std::fs::File,
}

pub async fn run(config: Arc<ClientConfig>) -> Result<()> {
    let root = config::data_dir()?;
    let guard = HostGuard::acquire(&root)?;
    let listener = UnixListener::bind(&guard.socket_path)
        .with_context(|| format!("bind Agent Host socket {}", guard.socket_path.display()))?;
    config::private_file(&guard.socket_path)?;

    let codex = AppServerProcess::new(config.clone());
    let kimi = KimiRuntime::new_host(config);
    let (live_events, _) = broadcast::channel(4096);
    let state = HostState {
        codex: codex.clone(),
        kimi: kimi.clone(),
        journal: Arc::new(Mutex::new(EventJournal {
            generation: uuid::Uuid::new_v4().to_string(),
            next_sequence: 1,
            events: VecDeque::with_capacity(EVENT_JOURNAL_CAPACITY),
        })),
        live_events,
        shutdown: Arc::new(Notify::new()),
        operation_gate: Arc::new(RwLock::new(())),
        stopping: Arc::new(AtomicBool::new(false)),
    };

    let collector_state = state.clone();
    let collector_task = tokio::spawn(async move {
        collect_codex_events(collector_state, codex.subscribe()).await;
    });
    let kimi_supervisor = tokio::spawn(supervise_kimi(kimi));

    tracing::info!(socket=%guard.socket_path.display(), "Nuntius Agent Host running");
    let external_shutdown = crate::shutdown_signal();
    tokio::pin!(external_shutdown);
    loop {
        tokio::select! {
            _ = state.shutdown.notified() => break,
            _ = &mut external_shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let connection_state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, connection_state).await {
                        tracing::warn!(error=?error, "Agent Host connection failed");
                    }
                });
            }
        }
    }

    kimi_supervisor.abort();
    collector_task.abort();
    state.codex.shutdown().await?;
    tracing::info!("Nuntius Agent Host stopped");
    Ok(())
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(config::data_dir()?.join("run/agent-host.sock"))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn ensure_started() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let socket = socket_path()?;
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return Ok(());
    }
    let executable = std::env::current_exe().context("resolve nuntius-client executable")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config::log_path()?)?;
    let stderr = log.try_clone()?;
    std::process::Command::new(executable)
        .arg("agent-host")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .process_group(0)
        .spawn()
        .context("start Nuntius Agent Host")?;
    Ok(())
}

async fn collect_codex_events(state: HostState, mut receiver: broadcast::Receiver<Value>) {
    loop {
        match receiver.recv().await {
            Ok(payload) => {
                let event = {
                    let mut journal = state.journal.lock().await;
                    let event = JournalEvent {
                        sequence: journal.next_sequence,
                        payload,
                    };
                    journal.next_sequence += 1;
                    journal.events.push_back(event.clone());
                    if journal.events.len() > EVENT_JOURNAL_CAPACITY {
                        journal.events.pop_front();
                    }
                    event
                };
                let _ = state.live_events.send(event);
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::error!(skipped, "Agent Host lost Codex source events");
                let event = {
                    let mut journal = state.journal.lock().await;
                    let event = JournalEvent {
                        sequence: journal.next_sequence,
                        payload: json!({
                            "method":"nuntius/resync_required",
                            "params":{"reason":"source_event_lag","skipped":skipped},
                        }),
                    };
                    journal.next_sequence += 1;
                    journal.events.push_back(event.clone());
                    if journal.events.len() > EVENT_JOURNAL_CAPACITY {
                        journal.events.pop_front();
                    }
                    event
                };
                let _ = state.live_events.send(event);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn supervise_kimi(kimi: KimiRuntime) {
    loop {
        if let Err(error) = kimi.ensure_ready().await {
            tracing::warn!(error=?error, "Agent Host could not start Kimi");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn handle_connection(stream: UnixStream, state: HostState) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).await?;
    if line.is_empty() {
        return Ok(());
    }
    if line.len() > MAX_REQUEST_BYTES {
        bail!("Agent Host request exceeds local frame limit")
    }
    let request: AgentHostRequest = serde_json::from_str(&line)?;
    match request {
        AgentHostRequest::Call {
            id,
            method,
            params,
            timeout_millis,
        } => {
            let _operation = state.operation_gate.read().await;
            if state.stopping.load(Ordering::Acquire) {
                return write_result(
                    &mut writer,
                    id,
                    Err(anyhow::anyhow!("Agent Host is rotating")),
                )
                .await;
            }
            let timeout = Duration::from_millis(timeout_millis.clamp(1, 300_000));
            let result = state
                .codex
                .call_with_timeout(&method, params, timeout)
                .await;
            write_result(&mut writer, id, result).await
        }
        AgentHostRequest::Respond {
            id,
            provider_request_id,
            result,
        } => {
            let _operation = state.operation_gate.read().await;
            if state.stopping.load(Ordering::Acquire) {
                return write_result(
                    &mut writer,
                    id,
                    Err(anyhow::anyhow!("Agent Host is rotating")),
                )
                .await;
            }
            let response = state
                .codex
                .respond(provider_request_id, result)
                .await
                .map(|_| Value::Null);
            write_result(&mut writer, id, response).await
        }
        AgentHostRequest::Status { id } => {
            write_host_message(
                &mut writer,
                &AgentHostMessage::Status {
                    id,
                    status: AgentHostStatus {
                        codex_running: state.codex.is_running().await,
                        build_sha: nuntius_updater::build_sha().into(),
                        release_sequence: nuntius_updater::build_sequence(),
                    },
                },
            )
            .await
        }
        AgentHostRequest::Subscribe {
            generation,
            after_sequence,
        } => subscribe_events(writer, state, generation, after_sequence).await,
        AgentHostRequest::ShutdownIfIdle { id } => {
            let _drain = state.operation_gate.write().await;
            let accepted = if state.stopping.load(Ordering::Acquire) {
                true
            } else {
                providers_are_idle(&state).await.unwrap_or_else(|error| {
                    tracing::warn!(error=?error, "refusing Agent Host rotation because idle state is unknown");
                    false
                })
            };
            if accepted {
                state.stopping.store(true, Ordering::Release);
            }
            write_host_message(
                &mut writer,
                &AgentHostMessage::Response {
                    id,
                    result: Some(json!({"accepted":accepted})),
                    error: None,
                },
            )
            .await?;
            if accepted {
                state.shutdown.notify_one();
            }
            Ok(())
        }
    }
}

async fn subscribe_events(
    mut writer: OwnedWriteHalf,
    state: HostState,
    requested_generation: Option<String>,
    requested_after: u64,
) -> Result<()> {
    let mut live = state.live_events.subscribe();
    let (generation, mut after_sequence, replay, oldest, latest, gap) = {
        let journal = state.journal.lock().await;
        let same_generation = requested_generation.as_deref() == Some(&journal.generation);
        let after = if same_generation { requested_after } else { 0 };
        let oldest = journal
            .events
            .front()
            .map(|event| event.sequence)
            .unwrap_or(journal.next_sequence);
        let latest = journal.next_sequence.saturating_sub(1);
        let gap = event_replay_requires_resync(
            requested_generation.as_deref(),
            &journal.generation,
            after,
            oldest,
        );
        let replay = journal
            .events
            .iter()
            .filter(|event| event.sequence > after)
            .cloned()
            .collect::<Vec<_>>();
        (
            journal.generation.clone(),
            after,
            replay,
            oldest,
            latest,
            gap,
        )
    };
    write_host_message(
        &mut writer,
        &AgentHostMessage::Subscribed {
            generation: generation.clone(),
            oldest_sequence: oldest,
            latest_sequence: latest,
        },
    )
    .await?;
    if gap {
        write_host_message(
            &mut writer,
            &AgentHostMessage::ResyncRequired {
                generation: generation.clone(),
                oldest_sequence: oldest,
                latest_sequence: latest,
            },
        )
        .await?;
    }
    for event in replay {
        send_event(&mut writer, &generation, &event).await?;
        after_sequence = event.sequence;
    }

    loop {
        match live.recv().await {
            Ok(event) if event.sequence > after_sequence => {
                send_event(&mut writer, &generation, &event).await?;
                after_sequence = event.sequence;
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let (replay, oldest, latest) = {
                    let journal = state.journal.lock().await;
                    let oldest = journal
                        .events
                        .front()
                        .map(|event| event.sequence)
                        .unwrap_or(journal.next_sequence);
                    let latest = journal.next_sequence.saturating_sub(1);
                    let replay = journal
                        .events
                        .iter()
                        .filter(|event| event.sequence > after_sequence)
                        .cloned()
                        .collect::<Vec<_>>();
                    (replay, oldest, latest)
                };
                if after_sequence > 0 && after_sequence.saturating_add(1) < oldest {
                    write_host_message(
                        &mut writer,
                        &AgentHostMessage::ResyncRequired {
                            generation: generation.clone(),
                            oldest_sequence: oldest,
                            latest_sequence: latest,
                        },
                    )
                    .await?;
                }
                for event in replay {
                    send_event(&mut writer, &generation, &event).await?;
                    after_sequence = event.sequence;
                }
            }
            Err(broadcast::error::RecvError::Closed) => bail!("Agent Host journal closed"),
        }
    }
}

async fn send_event(
    writer: &mut OwnedWriteHalf,
    generation: &str,
    event: &JournalEvent,
) -> Result<()> {
    write_host_message(
        writer,
        &AgentHostMessage::Event {
            generation: generation.to_owned(),
            sequence: event.sequence,
            payload: event.payload.clone(),
        },
    )
    .await
}

async fn write_result(
    writer: &mut OwnedWriteHalf,
    id: String,
    result: Result<Value>,
) -> Result<()> {
    let message = match result {
        Ok(result) => AgentHostMessage::Response {
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => AgentHostMessage::Response {
            id,
            result: None,
            error: Some(rpc_error(&error)),
        },
    };
    write_host_message(writer, &message).await
}

fn rpc_error(error: &anyhow::Error) -> AgentHostRpcError {
    if let Some(error) = error.downcast_ref::<AppServerCallError>() {
        AgentHostRpcError {
            message: error.message.clone(),
            method: Some(error.method.clone()),
            code: Some(error.code.clone()),
        }
    } else {
        AgentHostRpcError {
            message: format!("{error:#}"),
            method: None,
            code: None,
        }
    }
}

async fn write_host_message(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &impl Serialize,
) -> Result<()> {
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

async fn providers_are_idle(state: &HostState) -> Result<bool> {
    if state.codex.is_running().await && codex_has_active_threads(&state.codex).await? {
        return Ok(false);
    }
    let sessions = state.kimi.list_sessions(false).await?;
    Ok(!sessions.iter().any(kimi_session_is_active))
}

async fn codex_has_active_threads(codex: &AppServerProcess) -> Result<bool> {
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let response = codex
            .call(
                "thread/list",
                json!({"limit":100,"archived":false,"cursor":cursor}),
            )
            .await?;
        if response
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(codex_thread_is_active)
        {
            return Ok(true);
        }
        cursor = response
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            return Ok(false);
        }
    }
    bail!("Codex thread pagination exceeded safety limit while rotating Agent Host")
}

fn codex_thread_is_active(thread: &Value) -> bool {
    let status = thread
        .pointer("/status/type")
        .or_else(|| thread.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(status, "active" | "running" | "inProgress" | "recovering")
}

fn kimi_session_is_active(session: &Value) -> bool {
    session
        .get("main_turn_active")
        .and_then(Value::as_bool)
        .or_else(|| session.get("busy").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn event_replay_requires_resync(
    requested_generation: Option<&str>,
    current_generation: &str,
    requested_after: u64,
    oldest_available: u64,
) -> bool {
    requested_generation.is_some_and(|generation| generation != current_generation)
        || (requested_after > 0 && requested_after.saturating_add(1) < oldest_available)
}

impl HostGuard {
    fn acquire(root: &Path) -> Result<Self> {
        let run_dir = root.join("run");
        fs::create_dir_all(&run_dir)?;
        config::private_dir(&run_dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(run_dir.join("agent-host.lock"))?;
        lock.try_lock_exclusive()
            .context("another Nuntius Agent Host is already running")?;
        let socket_path = run_dir.join("agent-host.sock");
        match fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let pid_path = run_dir.join("agent-host.pid");
        fs::write(&pid_path, std::process::id().to_string())?;
        config::private_file(&pid_path)?;
        Ok(Self {
            socket_path,
            pid_path,
            _lock: lock,
        })
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_replay_requests_resync_for_host_rotation_or_journal_gap() {
        assert!(event_replay_requires_resync(
            Some("old-generation"),
            "new-generation",
            10,
            1,
        ));
        assert!(event_replay_requires_resync(
            Some("same-generation"),
            "same-generation",
            10,
            12,
        ));
        assert!(!event_replay_requires_resync(
            Some("same-generation"),
            "same-generation",
            11,
            12,
        ));
        assert!(!event_replay_requires_resync(
            None,
            "first-generation",
            0,
            1
        ));
    }

    #[test]
    fn provider_activity_detection_covers_codex_and_kimi() {
        assert!(codex_thread_is_active(&json!({"status":{"type":"active"}})));
        assert!(!codex_thread_is_active(&json!({"status":{"type":"idle"}})));
        assert!(kimi_session_is_active(&json!({"main_turn_active":true})));
        assert!(!kimi_session_is_active(&json!({"busy":false})));
    }
}
