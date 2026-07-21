use crate::config::{self, ClientConfig};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Child,
    sync::{Mutex, broadcast, mpsc, oneshot},
};

#[cfg(unix)]
use tokio::net::UnixStream;

const HOST_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HOST_RECONNECT_DELAY: Duration = Duration::from_millis(250);
static NEXT_HOST_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error)]
#[error("App Server {method} failed with code {code}: {message}")]
pub struct AppServerCallError {
    pub method: String,
    pub code: String,
    pub message: String,
}

impl AppServerCallError {
    pub fn is_missing_thread(&self) -> bool {
        let message = self.message.to_ascii_lowercase();
        self.code == "-32600"
            && (message.contains("no rollout found for thread id")
                || message.contains("thread not found")
                || message.contains("unknown thread"))
    }
}

#[derive(Clone)]
pub struct AppServerRuntime {
    mode: Arc<AppServerMode>,
    notifications: broadcast::Sender<AppServerEvent>,
}

enum AppServerMode {
    Host {
        socket_path: PathBuf,
        cursor_path: PathBuf,
        cursor: Mutex<EventCursor>,
    },
    Local(AppServerProcess),
}

#[derive(Debug, Clone)]
pub struct AppServerEvent {
    pub generation: String,
    pub sequence: u64,
    pub payload: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventCursor {
    generation: Option<String>,
    sequence: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentHostRequest {
    Call {
        id: String,
        method: String,
        params: Value,
        timeout_millis: u64,
    },
    Respond {
        id: String,
        provider_request_id: Value,
        result: Value,
    },
    Status {
        id: String,
    },
    Subscribe {
        generation: Option<String>,
        after_sequence: u64,
    },
    ShutdownIfIdle {
        id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentHostStatus {
    pub codex_running: bool,
    pub build_sha: String,
    pub release_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentHostRpcError {
    pub message: String,
    pub method: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentHostMessage {
    Response {
        id: String,
        result: Option<Value>,
        error: Option<AgentHostRpcError>,
    },
    Status {
        id: String,
        status: AgentHostStatus,
    },
    Subscribed {
        generation: String,
        oldest_sequence: u64,
        latest_sequence: u64,
    },
    Event {
        generation: String,
        sequence: u64,
        payload: Value,
    },
    ResyncRequired {
        generation: String,
        oldest_sequence: u64,
        latest_sequence: u64,
    },
}

#[derive(Clone)]
pub(crate) struct AppServerProcess {
    config: Arc<ClientConfig>,
    session: Arc<Mutex<Option<AppSession>>>,
    notifications: broadcast::Sender<Value>,
}

struct AppSession {
    child: Child,
    writer: mpsc::Sender<Value>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
    reader_alive: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
struct AppSessionHandle {
    writer: mpsc::Sender<Value>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
}

impl AppServerRuntime {
    pub fn new(config: Arc<ClientConfig>) -> Result<Self> {
        let (notifications, _) = broadcast::channel(4096);
        #[cfg(unix)]
        let mode = {
            let root = config::data_dir()?;
            let cursor_path = root.join("run/agent-host-event-cursor.json");
            let cursor = load_event_cursor(&cursor_path);
            AppServerMode::Host {
                socket_path: root.join("run/agent-host.sock"),
                cursor_path,
                cursor: Mutex::new(cursor),
            }
        };
        #[cfg(not(unix))]
        let mode = AppServerMode::Local(AppServerProcess::new(config));
        Ok(Self {
            mode: Arc::new(mode),
            notifications,
        })
    }

    #[cfg(test)]
    pub fn new_local(config: Arc<ClientConfig>) -> Self {
        let (notifications, _) = broadcast::channel(4096);
        Self {
            mode: Arc::new(AppServerMode::Local(AppServerProcess::new(config))),
            notifications,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppServerEvent> {
        self.notifications.subscribe()
    }

    pub async fn is_running(&self) -> bool {
        match self.mode.as_ref() {
            AppServerMode::Host { socket_path, .. } => host_status(socket_path)
                .await
                .map(|status| status.codex_running)
                .unwrap_or(false),
            AppServerMode::Local(process) => process.is_running().await,
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(method, params, Duration::from_secs(60))
            .await
    }

    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        match self.mode.as_ref() {
            AppServerMode::Host { socket_path, .. } => {
                let id = next_host_request_id();
                let timeout_millis = timeout.as_millis().min(u64::MAX as u128) as u64;
                let message = request_host(
                    socket_path,
                    AgentHostRequest::Call {
                        id: id.clone(),
                        method: method.to_owned(),
                        params,
                        timeout_millis,
                    },
                    timeout + Duration::from_secs(5),
                )
                .await?;
                response_result(message, &id)
            }
            AppServerMode::Local(process) => {
                process.call_with_timeout(method, params, timeout).await
            }
        }
    }

    pub async fn respond(&self, provider_request_id: Value, result: Value) -> Result<()> {
        match self.mode.as_ref() {
            AppServerMode::Host { socket_path, .. } => {
                let id = next_host_request_id();
                let message = request_host(
                    socket_path,
                    AgentHostRequest::Respond {
                        id: id.clone(),
                        provider_request_id,
                        result,
                    },
                    Duration::from_secs(10),
                )
                .await?;
                response_result(message, &id).map(|_| ())
            }
            AppServerMode::Local(process) => process.respond(provider_request_id, result).await,
        }
    }

    /// Client shutdown intentionally leaves the Agent Host and provider processes alive.
    pub async fn shutdown(&self) -> Result<()> {
        match self.mode.as_ref() {
            AppServerMode::Host { .. } => Ok(()),
            AppServerMode::Local(process) => process.shutdown().await,
        }
    }

    pub async fn run_event_stream(self) {
        match self.mode.as_ref() {
            AppServerMode::Host { .. } => loop {
                if let Err(error) = self.run_host_event_connection().await {
                    tracing::warn!(error=?error, "Agent Host event stream disconnected");
                    tokio::time::sleep(HOST_RECONNECT_DELAY).await;
                }
            },
            AppServerMode::Local(process) => {
                let mut receiver = process.subscribe();
                let mut sequence = 0;
                while let Ok(payload) = receiver.recv().await {
                    sequence += 1;
                    let _ = self.notifications.send(AppServerEvent {
                        generation: "local".into(),
                        sequence,
                        payload,
                    });
                }
            }
        }
    }

    pub async fn acknowledge(&self, event: &AppServerEvent) -> Result<()> {
        let AppServerMode::Host {
            cursor_path,
            cursor,
            ..
        } = self.mode.as_ref()
        else {
            return Ok(());
        };
        if event.sequence == 0 {
            return Ok(());
        }
        let mut current = cursor.lock().await;
        if current.generation.as_deref() != Some(&event.generation) {
            current.generation = Some(event.generation.clone());
            current.sequence = 0;
        }
        if event.sequence <= current.sequence {
            return Ok(());
        }
        current.sequence = event.sequence;
        persist_event_cursor(cursor_path, &current)
    }

    pub async fn request_host_upgrade_if_idle(&self) -> Result<bool> {
        let AppServerMode::Host { socket_path, .. } = self.mode.as_ref() else {
            return Ok(false);
        };
        let status = host_status(socket_path).await?;
        if status.build_sha == nuntius_updater::build_sha()
            && status.release_sequence == nuntius_updater::build_sequence()
        {
            return Ok(false);
        }
        let id = next_host_request_id();
        let message = request_host(
            socket_path,
            AgentHostRequest::ShutdownIfIdle { id: id.clone() },
            Duration::from_secs(30),
        )
        .await?;
        let result = response_result(message, &id)?;
        Ok(result.get("accepted").and_then(Value::as_bool) == Some(true))
    }

    async fn run_host_event_connection(&self) -> Result<()> {
        let AppServerMode::Host {
            socket_path,
            cursor,
            ..
        } = self.mode.as_ref()
        else {
            return Ok(());
        };
        let current = cursor.lock().await.clone();
        let mut stream = connect_host(socket_path).await?;
        write_message(
            &mut stream,
            &AgentHostRequest::Subscribe {
                generation: current.generation.clone(),
                after_sequence: current.sequence,
            },
        )
        .await?;
        let mut lines = BufReader::new(stream).lines();
        while let Some(line) = lines.next_line().await? {
            let message: AgentHostMessage = serde_json::from_str(&line)?;
            match message {
                AgentHostMessage::Subscribed { generation, .. } => {
                    let mut current = cursor.lock().await;
                    if current.generation.as_deref() != Some(&generation) {
                        current.generation = Some(generation);
                        current.sequence = 0;
                    }
                }
                AgentHostMessage::Event {
                    generation,
                    sequence,
                    payload,
                } => {
                    let _ = self.notifications.send(AppServerEvent {
                        generation,
                        sequence,
                        payload,
                    });
                }
                AgentHostMessage::ResyncRequired {
                    generation,
                    oldest_sequence,
                    latest_sequence,
                } => {
                    let _ = self.notifications.send(AppServerEvent {
                        generation,
                        sequence: 0,
                        payload: json!({
                            "method":"nuntius/resync_required",
                            "params":{
                                "oldestSequence":oldest_sequence,
                                "latestSequence":latest_sequence,
                            }
                        }),
                    });
                }
                AgentHostMessage::Response { .. } | AgentHostMessage::Status { .. } => {
                    bail!("unexpected Agent Host message on subscription")
                }
            }
        }
        bail!("Agent Host event stream closed")
    }
}

fn load_event_cursor(path: &Path) -> EventCursor {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn persist_event_cursor(path: &Path, cursor: &EventCursor) -> Result<()> {
    config::atomic_private_write(path, &serde_json::to_vec(cursor)?)
}

fn next_host_request_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT_HOST_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn response_result(message: AgentHostMessage, expected_id: &str) -> Result<Value> {
    let AgentHostMessage::Response { id, result, error } = message else {
        bail!("unexpected Agent Host response")
    };
    if id != expected_id {
        bail!("Agent Host response id mismatch")
    }
    if let Some(error) = error {
        if let (Some(method), Some(code)) = (error.method, error.code) {
            return Err(AppServerCallError {
                method,
                code,
                message: error.message,
            }
            .into());
        }
        bail!("Agent Host request failed: {}", error.message)
    }
    Ok(result.unwrap_or(Value::Null))
}

async fn host_status(path: &Path) -> Result<AgentHostStatus> {
    let id = next_host_request_id();
    match request_host(
        path,
        AgentHostRequest::Status { id: id.clone() },
        Duration::from_secs(5),
    )
    .await?
    {
        AgentHostMessage::Status {
            id: response_id,
            status,
        } if response_id == id => Ok(status),
        _ => bail!("unexpected Agent Host status response"),
    }
}

#[cfg(unix)]
async fn request_host(
    path: &Path,
    request: AgentHostRequest,
    timeout: Duration,
) -> Result<AgentHostMessage> {
    tokio::time::timeout(timeout, async {
        let mut stream = connect_host(path).await?;
        write_message(&mut stream, &request).await?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        if line.is_empty() {
            bail!("Agent Host closed before responding")
        }
        Ok(serde_json::from_str(&line)?)
    })
    .await
    .context("Agent Host request timed out")?
}

#[cfg(not(unix))]
async fn request_host(
    _path: &Path,
    _request: AgentHostRequest,
    _timeout: Duration,
) -> Result<AgentHostMessage> {
    bail!("Agent Host sockets are unavailable on this platform")
}

#[cfg(unix)]
async fn connect_host(path: &Path) -> Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + HOST_CONNECT_TIMEOUT;
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if tokio::time::Instant::now() < deadline => {
                tracing::debug!(error=?error,path=%path.display(), "waiting for Agent Host");
                tokio::time::sleep(HOST_RECONNECT_DELAY).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("connect Agent Host {}", path.display()));
            }
        }
    }
}

#[cfg(unix)]
async fn write_message(stream: &mut UnixStream, message: &AgentHostRequest) -> Result<()> {
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    Ok(())
}

impl AppServerProcess {
    pub fn new(config: Arc<ClientConfig>) -> Self {
        let (notifications, _) = broadcast::channel(4096);
        Self {
            config,
            session: Arc::new(Mutex::new(None)),
            notifications,
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }
    pub async fn is_running(&self) -> bool {
        let mut guard = self.session.lock().await;
        guard
            .as_mut()
            .is_some_and(|s| matches!(s.child.try_wait(), Ok(None)))
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(method, params, std::time::Duration::from_secs(60))
            .await
    }
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let mut guard = self.session.lock().await;
        let must_start = match guard.as_mut() {
            Some(session) => {
                session.child.try_wait()?.is_some() || !session.reader_alive.load(Ordering::Relaxed)
            }
            None => true,
        };
        if must_start {
            if let Some(mut old) = guard.take() {
                let _ = old.child.kill().await;
            }
            *guard = Some(AppSession::spawn(&self.config, self.notifications.clone()).await?);
        }
        let handle = guard.as_mut().expect("session initialized").handle();
        // App Server can issue an approval request while this call is pending. Never
        // hold the lifecycle lock across the await, otherwise `respond` deadlocks.
        drop(guard);
        handle.request_with_timeout(method, params, timeout).await
    }
    pub async fn respond(&self, request_id: Value, result: Value) -> Result<()> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().context("Codex App Server is not running")?;
        let writer = session.writer.clone();
        drop(guard);
        writer
            .send(json!({"id":request_id,"result":result}))
            .await
            .map_err(|_| anyhow!("App Server writer stopped"))
    }
    pub async fn shutdown(&self) -> Result<()> {
        if let Some(mut session) = self.session.lock().await.take() {
            session.child.kill().await?;
        }
        Ok(())
    }
}

impl AppSession {
    async fn spawn(config: &ClientConfig, notifications: broadcast::Sender<Value>) -> Result<Self> {
        let mut command = crate::probe::provider_command(&config.codex_command);
        command
            .args(&config.codex_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start `{}`", config.codex_command))?;
        let stdin = child.stdin.take().context("App Server stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("App Server stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("App Server stderr unavailable")?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(bytes = line.len(), "App Server wrote stderr");
            }
        });
        let (writer_tx, mut writer_rx) = mpsc::channel::<Value>(256);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(message) = writer_rx.recv().await {
                let mut encoded = match serde_json::to_vec(&message) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(error=?e,"cannot encode App Server message");
                        continue;
                    }
                };
                encoded.push(b'\n');
                if stdin.write_all(&encoded).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let pending = Arc::new(Mutex::new(HashMap::<String, oneshot::Sender<Value>>::new()));
        let reader_pending = pending.clone();
        let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reader_alive_exit = reader_alive.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        // Oversized frames are dropped, never fatal: the reader must keep
                        // consuming stdout or every subsequent request silently wedges.
                        if line.len() > 128 * 1024 * 1024 {
                            tracing::error!(
                                bytes = line.len(),
                                "App Server JSONL message exceeds limit; dropping frame"
                            );
                            continue;
                        }
                        if line.len() > 2 * 1024 * 1024 {
                            tracing::warn!(bytes = line.len(), "large App Server JSONL message");
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(value) => {
                                let response_id = value
                                    .get("id")
                                    .filter(|_| {
                                        value.get("result").is_some()
                                            || value.get("error").is_some()
                                    })
                                    .map(id_key);
                                if let Some(id) = response_id {
                                    if let Some(sender) = reader_pending.lock().await.remove(&id) {
                                        let _ = sender.send(value);
                                    }
                                } else {
                                    let _ = notifications.send(value);
                                }
                            }
                            Err(error) => {
                                tracing::warn!(error=?error,bytes=line.len(),"invalid App Server JSONL")
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(error=?error,"App Server stdout failed");
                        break;
                    }
                }
            }
            reader_alive_exit.store(false, Ordering::Relaxed);
            let mut pending = reader_pending.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(json!({"error":{"code":-32000,"message":"App Server exited before responding"}}));
            }
        });
        let session = Self {
            child,
            writer: writer_tx,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            reader_alive,
        };
        let initialized = session
            .handle()
            .request_with_timeout(
                "initialize",
                json!({"clientInfo":{"name":"nuntius-client","title":"Nuntius","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}),
                std::time::Duration::from_secs(20),
            )
            .await?;
        if initialized.get("error").is_some() {
            bail!("App Server initialize failed: {initialized}")
        }
        session
            .writer
            .send(json!({"method":"initialized","params":{}}))
            .await
            .map_err(|_| anyhow!("App Server writer stopped"))?;
        Ok(session)
    }
    fn handle(&self) -> AppSessionHandle {
        AppSessionHandle {
            writer: self.writer.clone(),
            pending: self.pending.clone(),
            next_id: self.next_id.clone(),
        }
    }
}

impl AppSessionHandle {
    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key.clone(), tx);
        if self
            .writer
            .send(json!({"id":id,"method":method,"params":params}))
            .await
            .is_err()
        {
            self.pending.lock().await.remove(&key);
            bail!("App Server writer stopped")
        }
        let response = match tokio::time::timeout(timeout, rx).await {
            Ok(response) => response.context("App Server response channel closed")?,
            Err(_) => {
                self.pending.lock().await.remove(&key);
                bail!("App Server request timed out; outcome is unknown")
            }
        };
        if let Some(error) = response.get("error") {
            let code = error
                .get("code")
                .map(Value::to_string)
                .unwrap_or_else(|| "unknown".into());
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .replace(['\r', '\n'], " ");
            let message: String = message.chars().take(500).collect();
            return Err(AppServerCallError {
                method: method.into(),
                code,
                message,
            }
            .into());
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}
fn id_key(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}
