use crate::{
    agent::AgentThreadState,
    config::ClientConfig,
    protocol::{AgentModelOption, AgentProvider, AgentProviderStatus, ConversationAccessMode},
};
use anyhow::{Context, Result, bail};
use base64::Engine;
use directories::BaseDirs;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, broadcast, mpsc, oneshot},
};

/// Pi runs one RPC process per session (`pi --mode rpc` speaks JSONL over
/// stdio and owns exactly one conversation). The runtime therefore keeps a
/// pool of session-keyed child processes instead of a single shared server.
///
/// Two intentional deviations from the Codex/Kimi runtimes:
/// - Pi processes are owned by the Client (kill_on_drop), not by the Agent
///   Host. A Client rotation interrupts in-flight Pi turns; the recovery loop
///   then closes them from the durable session file. Spawning a second writer
///   for a session JSONL that an orphaned process still appends to would be
///   worse than a clean interruption.
/// - History reads parse `~/.pi/agent/sessions/**/*.jsonl` directly instead
///   of hydrating through a live process, so discovery of sessions created by
///   an external `pi` CLI stays cheap and works while no process is running.
///
/// Pi has no sandbox/approval policy; both access modes execute tools
/// directly. Extension UI dialogs (`extension_ui_request`) are bridged into
/// Nuntius approvals.
#[derive(Clone)]
pub struct PiRuntime {
    config: Arc<ClientConfig>,
    sessions: Arc<Mutex<HashMap<String, PiProcess>>>,
    events: broadcast::Sender<Value>,
    model_catalog: Arc<Mutex<Option<CachedCatalog>>>,
}

struct CachedCatalog {
    fetched_at: std::time::Instant,
    models: Vec<AgentModelOption>,
}

/// How long a probed `/model` catalog is reused before another short-lived
/// `pi --mode rpc --no-session` probe refreshes it.
const CATALOG_TTL: Duration = Duration::from_secs(60);

struct PiProcess {
    child: Child,
    handle: PiHandle,
}

#[derive(Clone)]
struct PiHandle {
    writer: mpsc::Sender<Value>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
    reader_alive: Arc<AtomicBool>,
    /// Session id stamped onto outgoing events. Events only flow after the
    /// first prompt, which always happens after the id is registered.
    tag: Arc<RwLock<String>>,
}

impl PiHandle {
    fn is_alive(&self) -> bool {
        self.reader_alive.load(Ordering::Relaxed)
    }
}

const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_EVENT_TEXT: usize = 256 * 1024;

impl PiRuntime {
    pub fn new(config: Arc<ClientConfig>) -> Self {
        let (events, _) = broadcast::channel(4096);
        Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            events,
            model_catalog: Arc::new(Mutex::new(None)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    pub async fn provider_status(&self) -> AgentProviderStatus {
        let live = self.any_live_handle().await;
        let version = if live.is_some() {
            None
        } else {
            tokio::time::timeout(
                Duration::from_secs(3),
                Command::new(&self.config.pi_command)
                    .arg("--version")
                    .output(),
            )
            .await
            .ok()
            .and_then(|result| result.ok())
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        };
        let available = live.is_some() || version.is_some();
        let models = if let Some(handle) = &live {
            match tokio::time::timeout(Duration::from_secs(5), self.catalog_from(handle)).await {
                Ok(Some(models)) => {
                    *self.model_catalog.lock().await = Some(CachedCatalog {
                        fetched_at: std::time::Instant::now(),
                        models: models.clone(),
                    });
                    models
                }
                Ok(None) => self.cached_or_fallback_models().await,
                Err(_) => self.cached_or_fallback_models().await,
            }
        } else if available {
            match self.probe_model_catalog().await {
                Some(models) => models,
                None => self.cached_or_fallback_models().await,
            }
        } else {
            Vec::new()
        };
        AgentProviderStatus {
            provider: AgentProvider::Pi,
            label: "Pi".into(),
            available,
            status: if live.is_some() {
                "online"
            } else if available {
                "stopped"
            } else {
                "unavailable"
            }
            .into(),
            version,
            models,
        }
    }

    async fn cached_or_fallback_models(&self) -> Vec<AgentModelOption> {
        self.model_catalog
            .lock()
            .await
            .as_ref()
            .map(|cache| cache.models.clone())
            .filter(|models| !models.is_empty())
            .unwrap_or_else(fallback_pi_models)
    }

    /// Fetch the real `/model` catalog from a short-lived probe process.
    /// `--no-session` keeps the probe from persisting an empty conversation;
    /// Pi only writes session files on the first appended entry anyway.
    async fn probe_model_catalog(&self) -> Option<Vec<AgentModelOption>> {
        {
            let cache = self.model_catalog.lock().await;
            if let Some(cache) = cache.as_ref()
                && cache.fetched_at.elapsed() < CATALOG_TTL
                && !cache.models.is_empty()
            {
                return Some(cache.models.clone());
            }
        }
        let cwd = std::env::current_dir().ok()?;
        let mut process = self.spawn(&cwd, &["--no-session"]).await.ok()?;
        let handle = process.handle.clone();
        let models =
            tokio::time::timeout(Duration::from_secs(15), self.catalog_from(&handle)).await;
        let _ = process.child.kill().await;
        let _ = process.child.wait().await;
        let models = models.ok()??;
        *self.model_catalog.lock().await = Some(CachedCatalog {
            fetched_at: std::time::Instant::now(),
            models: models.clone(),
        });
        Some(models)
    }

    /// Read the available models plus the session's current model, which is
    /// Pi's effective default and drives the picker's initial selection.
    async fn catalog_from(&self, handle: &PiHandle) -> Option<Vec<AgentModelOption>> {
        let catalog = self
            .call(handle, json!({"type":"get_available_models"}))
            .await
            .ok()?;
        let current = self
            .call(handle, json!({"type":"get_state"}))
            .await
            .ok()
            .and_then(|state| {
                let provider = state
                    .pointer("/model/provider")
                    .and_then(Value::as_str)?;
                let id = state.pointer("/model/id").and_then(Value::as_str)?;
                Some((provider.to_owned(), id.to_owned()))
            });
        let models = parse_pi_models(
            &catalog,
            current
                .as_ref()
                .map(|(provider, id)| (provider.as_str(), id.as_str())),
        );
        (!models.is_empty()).then_some(models)
    }

    pub async fn create_session(
        &self,
        cwd: &Path,
        title: &str,
        _access_mode: ConversationAccessMode,
        options: &Value,
    ) -> Result<Value> {
        let process = self.spawn(cwd, &[]).await?;
        let handle = process.handle.clone();
        self.apply_turn_options(&handle, options).await?;
        if !title.trim().is_empty() {
            let _ = self
                .call(&handle, json!({"type":"set_session_name","name":title}))
                .await;
        }
        let state = self.call(&handle, json!({"type":"get_state"})).await?;
        let session_id = state
            .get("sessionId")
            .and_then(Value::as_str)
            .context("Pi did not report a session id")?
            .to_owned();
        *handle.tag.write().expect("Pi session tag") = session_id.clone();
        let session_file = state
            .get("sessionFile")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.sessions.lock().await.insert(session_id.clone(), process);
        Ok(json!({
            "id": session_id,
            "sessionFile": session_file,
            "cwd": cwd,
        }))
    }

    /// Reattach a live process to a durable Pi session. Sessions discovered
    /// from disk (or created before a Client restart) are spawned on demand
    /// and switched onto their JSONL file.
    async fn attach(&self, session_id: &str) -> Result<PiHandle> {
        if let Some(handle) = self.live_handle(session_id).await {
            return Ok(handle);
        }
        let session_file = find_session_file(session_id)
            .await?
            .with_context(|| format!("Pi session {session_id} not found"))?;
        let header = read_session_header(&session_file).await?;
        let cwd = header
            .get("cwd")
            .and_then(Value::as_str)
            .context("Pi session header has no cwd")?;
        let process = self.spawn(Path::new(cwd), &[]).await?;
        let handle = process.handle.clone();
        self.call(
            &handle,
            json!({"type":"switch_session","sessionPath":session_file}),
        )
        .await?;
        let state = self.call(&handle, json!({"type":"get_state"})).await?;
        if state.get("sessionId").and_then(Value::as_str) != Some(session_id) {
            bail!("Pi switched to an unexpected session")
        }
        *handle.tag.write().expect("Pi session tag") = session_id.to_owned();
        self.sessions
            .lock()
            .await
            .insert(session_id.to_owned(), process);
        Ok(handle)
    }

    async fn live_handle(&self, session_id: &str) -> Option<PiHandle> {
        let mut pool = self.sessions.lock().await;
        let alive = match pool.get_mut(session_id) {
            Some(process) => {
                process.handle.is_alive() && matches!(process.child.try_wait(), Ok(None))
            }
            None => false,
        };
        if alive {
            return pool.get(session_id).map(|process| process.handle.clone());
        }
        pool.remove(session_id);
        None
    }

    async fn any_live_handle(&self) -> Option<PiHandle> {
        let mut pool = self.sessions.lock().await;
        let mut dead = Vec::new();
        let mut live = None;
        for (session_id, process) in pool.iter_mut() {
            if process.handle.is_alive() && matches!(process.child.try_wait(), Ok(None)) {
                live = live.or_else(|| Some(process.handle.clone()));
            } else {
                dead.push(session_id.clone());
            }
        }
        for session_id in dead {
            pool.remove(&session_id);
        }
        live
    }

    pub async fn submit_prompt(
        &self,
        session_id: &str,
        input: &[Value],
        options: &Value,
    ) -> Result<Value> {
        let handle = self.attach(session_id).await?;
        self.apply_turn_options(&handle, options).await?;
        let (message, images) = pi_content(input).await?;
        let mut command = json!({"type":"prompt","message":message});
        if !images.is_empty() {
            command["images"] = json!(images);
        }
        self.call(&handle, command).await?;
        Ok(json!({"accepted":true}))
    }

    pub async fn steer_prompt(
        &self,
        session_id: &str,
        input: &[Value],
        options: &Value,
    ) -> Result<Value> {
        let handle = self.attach(session_id).await?;
        self.apply_turn_options(&handle, options).await?;
        let (message, images) = pi_content(input).await?;
        let mut command = json!({"type":"steer","message":message});
        if !images.is_empty() {
            command["images"] = json!(images);
        }
        self.call(&handle, command).await?;
        Ok(json!({"accepted":true,"queued":"steer"}))
    }

    pub async fn interrupt(&self, session_id: &str) -> Result<Value> {
        let Some(handle) = self.live_handle(session_id).await else {
            return Ok(json!({"alreadyTerminal":true}));
        };
        self.call(&handle, json!({"type":"abort"})).await?;
        Ok(json!({"aborted":true}))
    }

    pub async fn thread_state(&self, session_id: &str) -> Result<AgentThreadState> {
        if let Some(handle) = self.live_handle(session_id).await {
            let state = self.call(&handle, json!({"type":"get_state"})).await?;
            let streaming = state.get("isStreaming").and_then(Value::as_bool) == Some(true);
            return Ok(AgentThreadState {
                status: if streaming { "active" } else { "idle" }.into(),
                active_turn_id: None,
            });
        }
        // No live process: the turn cannot be running anywhere we control.
        // The durable session file is the remaining source of truth.
        if find_session_file(session_id).await?.is_some() {
            return Ok(AgentThreadState {
                status: "idle".into(),
                active_turn_id: None,
            });
        }
        bail!("Pi session {session_id} not found")
    }

    pub async fn read_thread(&self, session_id: &str) -> Result<Value> {
        let path = find_session_file(session_id)
            .await?
            .with_context(|| format!("Pi session {session_id} not found"))?;
        let streaming = match self.live_handle(session_id).await {
            Some(handle) => self
                .call(&handle, json!({"type":"get_state"}))
                .await
                .ok()
                .and_then(|state| state.get("isStreaming").and_then(Value::as_bool))
                .unwrap_or(false),
            None => false,
        };
        parse_pi_session(&path, streaming, true).await
    }

    pub async fn list_sessions(
        &self,
        cwd: Option<&Path>,
        archived: bool,
    ) -> Result<Vec<Value>> {
        // Pi has no provider-side archive; archiving is a Nuntius projection.
        if archived {
            return Ok(Vec::new());
        }
        let root = pi_sessions_root()?;
        let mut threads = Vec::new();
        let mut dirs = match tokio::fs::read_dir(&root).await {
            Ok(dirs) => dirs,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(threads),
            Err(error) => return Err(error).context("cannot scan Pi session directory"),
        };
        while let Some(dir) = dirs.next_entry().await? {
            let mut files = match tokio::fs::read_dir(dir.path()).await {
                Ok(files) => files,
                Err(_) => continue,
            };
            while let Some(file) = files.next_entry().await? {
                let path = file.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                    continue;
                }
                let summary = match parse_pi_session(&path, false, false).await {
                    Ok(summary) => summary,
                    Err(error) => {
                        tracing::debug!(path=%path.display(),error=?error,"skipping unreadable Pi session");
                        continue;
                    }
                };
                if cwd.is_some_and(|expected| {
                    summary.get("cwd").and_then(Value::as_str) != expected.to_str()
                }) {
                    continue;
                }
                // Sessions with neither a user prompt nor a display name are
                // empty husks left by external `pi` processes; they carry no
                // history worth importing.
                if summary.get("preview").is_none_or(Value::is_null)
                    && summary.get("name").and_then(Value::as_str) == Some("Pi 会话")
                {
                    continue;
                }
                threads.push(summary);
            }
        }
        Ok(threads)
    }

    pub async fn resolve_ui(
        &self,
        session_id: &str,
        request_id: &str,
        ui_method: &str,
        decision: &str,
        response: Option<&Value>,
    ) -> Result<()> {
        let handle = self
            .live_handle(session_id)
            .await
            .context("Pi session is not running")?;
        let mut reply = json!({"type":"extension_ui_response","id":request_id});
        match (ui_method, decision) {
            ("confirm", "accept") => reply["confirmed"] = json!(true),
            ("select" | "input" | "editor", "accept") => {
                if let Some(value) = response.and_then(|value| value.get("value")) {
                    reply["value"] = value.clone();
                } else {
                    reply["cancelled"] = json!(true);
                }
            }
            _ => reply["cancelled"] = json!(true),
        }
        handle
            .writer
            .send(reply)
            .await
            .map_err(|_| anyhow::anyhow!("Pi process writer stopped"))
    }

    async fn apply_turn_options(&self, handle: &PiHandle, options: &Value) -> Result<()> {
        if let Some((provider, model)) = options
            .get("model")
            .and_then(Value::as_str)
            .and_then(split_model)
        {
            self.call(
                handle,
                json!({"type":"set_model","provider":provider,"modelId":model}),
            )
            .await
            .context("Pi rejected the selected model")?;
        }
        if let Some(level) = options.get("thinking").and_then(pi_thinking_level) {
            self.call(handle, json!({"type":"set_thinking_level","level":level}))
                .await
                .context("Pi rejected the selected thinking level")?;
        }
        Ok(())
    }

    async fn call(&self, handle: &PiHandle, command: Value) -> Result<Value> {
        self.call_with_timeout(handle, command, RPC_TIMEOUT).await
    }

    async fn call_with_timeout(
        &self,
        handle: &PiHandle,
        mut command: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = format!(
            "nuntius-{}",
            handle.next_id.fetch_add(1, Ordering::Relaxed)
        );
        command["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        handle.pending.lock().await.insert(id.clone(), tx);
        if handle.writer.send(command).await.is_err() {
            handle.pending.lock().await.remove(&id);
            bail!("Pi process writer stopped")
        }
        let response = match tokio::time::timeout(timeout, rx).await {
            Ok(response) => response.context("Pi response channel closed")?,
            Err(_) => {
                handle.pending.lock().await.remove(&id);
                bail!("Pi request timed out; outcome is unknown")
            }
        };
        if response.get("success").and_then(Value::as_bool) == Some(true) {
            return Ok(response.get("data").cloned().unwrap_or(Value::Null));
        }
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown Pi error")
            .replace(['\r', '\n'], " ");
        let message: String = message.chars().take(500).collect();
        bail!("Pi command failed: {message}")
    }

    async fn spawn(&self, cwd: &Path, extra_args: &[&str]) -> Result<PiProcess> {
        let mut command = Command::new(&self.config.pi_command);
        command
            .args(&self.config.pi_args)
            .args(extra_args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The Client owns Pi sessions; see the type-level comment for why
            // turns must not outlive a Client rotation as orphaned writers.
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start `{}`", self.config.pi_command))?;
        let stdin = child.stdin.take().context("Pi stdin unavailable")?;
        let stdout = child.stdout.take().context("Pi stdout unavailable")?;
        let stderr = child.stderr.take().context("Pi stderr unavailable")?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(bytes = line.len(), "Pi wrote stderr");
            }
        });
        let (writer_tx, mut writer_rx) = mpsc::channel::<Value>(256);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(message) = writer_rx.recv().await {
                let mut encoded = match serde_json::to_vec(&message) {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        tracing::error!(error=?error, "cannot encode Pi message");
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
        let reader_alive = Arc::new(AtomicBool::new(true));
        let reader_alive_exit = reader_alive.clone();
        let tag = Arc::new(RwLock::new(String::new()));
        let reader_tag = tag.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.len() > 128 * 1024 * 1024 {
                            tracing::error!(bytes = line.len(), "Pi JSONL message exceeds limit; dropping frame");
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(value) => {
                                let is_response =
                                    value.get("type").and_then(Value::as_str) == Some("response");
                                let response_id = is_response
                                    .then(|| value.get("id").and_then(Value::as_str))
                                    .flatten()
                                    .map(str::to_owned);
                                if let Some(id) = response_id {
                                    if let Some(sender) = reader_pending.lock().await.remove(&id) {
                                        let _ = sender.send(value);
                                    }
                                } else {
                                    let session_id =
                                        reader_tag.read().expect("Pi session tag").clone();
                                    let _ = events.send(json!({
                                        "session_id": session_id,
                                        "event": value,
                                    }));
                                }
                            }
                            Err(error) => {
                                tracing::warn!(error=?error,bytes=line.len(),"invalid Pi JSONL")
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(error=?error, "Pi stdout failed");
                        break;
                    }
                }
            }
            reader_alive_exit.store(false, Ordering::Relaxed);
            let mut pending = reader_pending.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(json!({"success":false,"error":"Pi exited before responding"}));
            }
        });
        Ok(PiProcess {
            child,
            handle: PiHandle {
                writer: writer_tx,
                pending,
                next_id: Arc::new(AtomicU64::new(1)),
                reader_alive,
                tag,
            },
        })
    }

    pub async fn shutdown(&self) {
        let mut pool = self.sessions.lock().await;
        for (_, mut process) in pool.drain() {
            let _ = process.child.kill().await;
        }
    }
}

fn pi_sessions_root() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .context("cannot resolve user home directory")?
        .home_dir()
        .join(".pi/agent/sessions"))
}

/// Session files are named `<timestamp>_<session-id>.jsonl` inside a
/// per-working-directory folder.
async fn find_session_file(session_id: &str) -> Result<Option<PathBuf>> {
    let root = pi_sessions_root()?;
    let suffix = format!("_{session_id}.jsonl");
    let mut dirs = match tokio::fs::read_dir(&root).await {
        Ok(dirs) => dirs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot scan Pi session directory"),
    };
    while let Some(dir) = dirs.next_entry().await? {
        let mut files = match tokio::fs::read_dir(dir.path()).await {
            Ok(files) => files,
            Err(_) => continue,
        };
        while let Some(file) = files.next_entry().await? {
            let name = file.file_name();
            if name.to_string_lossy().ends_with(&suffix) {
                return Ok(Some(file.path()));
            }
        }
    }
    Ok(None)
}

async fn read_session_header(path: &Path) -> Result<Value> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("cannot read Pi session {}", path.display()))?;
    let line = text
        .lines()
        .next()
        .context("Pi session file is empty")?;
    let header: Value = serde_json::from_str(line).context("invalid Pi session header")?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        bail!("Pi session file does not start with a session header")
    }
    Ok(header)
}

/// Rebuild the active branch of a Pi session file. Entries form a tree via
/// `id`/`parentId`; the current leaf is the last appended entry.
async fn parse_pi_session(path: &Path, streaming: bool, include_turns: bool) -> Result<Value> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("cannot read Pi session {}", path.display()))?;
    let modified_unix = tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64);
    let mut header: Option<Value> = None;
    let mut entries: Vec<Value> = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session") => header = Some(value),
            _ if value.get("id").and_then(Value::as_str).is_some() => entries.push(value),
            _ => {}
        }
    }
    let header = header.context("Pi session file has no header")?;
    let id = header
        .get("id")
        .and_then(Value::as_str)
        .context("Pi session header has no id")?
        .to_owned();
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(|id| (id, entry)))
        .collect();
    let mut branch: Vec<&Value> = Vec::new();
    let mut current = entries.last();
    while let Some(entry) = current {
        branch.push(entry);
        current = entry
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|parent| by_id.get(parent).copied());
    }
    branch.reverse();

    let mut name: Option<String> = None;
    let mut first_user_text: Option<String> = None;
    let mut groups: Vec<(String, Vec<Value>, Option<i64>)> = Vec::new();
    for entry in &branch {
        match entry.get("type").and_then(Value::as_str) {
            Some("session_info") => {
                if let Some(value) = entry.get("name").and_then(Value::as_str) {
                    name = Some(value.to_owned());
                }
            }
            Some("message") => {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                let role = message.get("role").and_then(Value::as_str).unwrap_or("");
                if role == "user" && first_user_text.is_none() {
                    let text = user_text(&message);
                    if !text.is_empty() {
                        first_user_text = Some(text.chars().take(200).collect());
                    }
                }
                if !include_turns || role == "custom" {
                    continue;
                }
                let items = normalize_pi_message(entry, &message);
                if items.is_empty() {
                    continue;
                }
                let timestamp = rfc3339_unix(entry.get("timestamp"));
                if role == "user" {
                    let group_id = entry
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    groups.push((group_id, items, timestamp));
                } else if let Some(group) = groups.last_mut() {
                    group.1.extend(items);
                } else {
                    groups.push(("pi-preamble".into(), items, timestamp));
                }
            }
            _ => {}
        }
    }
    let last_index = groups.len().saturating_sub(1);
    let turns = groups
        .into_iter()
        .enumerate()
        .map(|(index, (group_id, items, started))| {
            let active = streaming && index == last_index;
            json!({
                "id": group_id,
                "status": if active {"inProgress"} else {"completed"},
                "startedAt": started,
                "completedAt": if active {Value::Null} else {started.map(Value::from).unwrap_or(Value::Null)},
                "items": items,
            })
        })
        .collect::<Vec<_>>();
    let preview = first_user_text;
    let title = name
        .clone()
        .or_else(|| preview.clone().map(|text| text.chars().take(40).collect()))
        .unwrap_or_else(|| "Pi 会话".into());
    Ok(json!({
        "id": id,
        "name": title,
        "preview": preview,
        "cwd": header.get("cwd"),
        "status": if streaming {"active"} else {"idle"},
        "archived": false,
        "updatedAt": modified_unix.or_else(|| rfc3339_unix(header.get("timestamp"))),
        "createdAt": rfc3339_unix(header.get("timestamp")),
        "turns": turns,
    }))
}

fn normalize_pi_message(entry: &Value, message: &Value) -> Vec<Value> {
    let entry_id = entry
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let failed = message.get("stopReason").and_then(Value::as_str) == Some("error");
    let status = if failed { "failed" } else { "completed" };
    match role {
        "user" => {
            let text = user_text(message);
            if text.is_empty() {
                return Vec::new();
            }
            vec![json!({
                "id": entry_id,
                "type": "userMessage",
                "status": "completed",
                "text": text,
                "structuredDetail": message,
            })]
        }
        "assistant" => {
            let mut items = Vec::new();
            for (index, block) in message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => items.push(json!({
                        "id": format!("{entry_id}:{index}"),
                        "type": "agentMessage",
                        "status": status,
                        "text": block.get("text").and_then(Value::as_str).unwrap_or_default(),
                        "structuredDetail": block,
                    })),
                    Some("thinking") => items.push(json!({
                        "id": format!("{entry_id}:{index}"),
                        "type": "reasoning",
                        "status": "completed",
                        "text": block.get("thinking").and_then(Value::as_str).unwrap_or_default(),
                        "structuredDetail": block,
                    })),
                    Some("toolCall") => items.push(json!({
                        "id": block.get("id").and_then(Value::as_str).unwrap_or(&format!("{entry_id}:{index}")),
                        "type": "commandExecution",
                        "status": status,
                        "text": format!(
                            "{} {}",
                            block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                            block.get("arguments").map(truncate_value).unwrap_or_default(),
                        ),
                        "structuredDetail": block,
                    })),
                    _ => {}
                }
            }
            if items.is_empty()
                && let Some(error) = message.get("errorMessage").and_then(Value::as_str)
            {
                items.push(json!({
                    "id": entry_id,
                    "type": "agentMessage",
                    "status": "failed",
                    "text": error,
                    "structuredDetail": message,
                }));
            }
            items
        }
        "toolResult" => {
            let text = message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            let is_error = message.get("isError").and_then(Value::as_bool) == Some(true);
            vec![json!({
                "id": message.get("toolCallId").and_then(Value::as_str).unwrap_or(entry_id),
                "type": "commandExecution",
                "status": if is_error {"failed"} else {"completed"},
                "text": text,
                "structuredDetail": message,
            })]
        }
        "bashExecution" => {
            let command = message.get("command").and_then(Value::as_str).unwrap_or("");
            let output = message.get("output").and_then(Value::as_str).unwrap_or("");
            vec![json!({
                "id": entry_id,
                "type": "commandExecution",
                "status": "completed",
                "text": format!("Ran `{command}`\n{output}"),
                "structuredDetail": message,
            })]
        }
        _ => Vec::new(),
    }
}

fn user_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn truncate_value(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > MAX_EVENT_TEXT {
        text.chars().take(MAX_EVENT_TEXT).collect()
    } else {
        text
    }
}

fn rfc3339_unix(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_str)
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|value| value.unix_timestamp())
}

/// Model options use the `provider/modelId` form accepted by `set_model`.
fn split_model(model: &str) -> Option<(&str, &str)> {
    let (provider, id) = model.split_once('/')?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some((provider, id))
}

/// Pi thinking levels: off, minimal, low, medium, high, xhigh. Other
/// providers' vocabulary is mapped onto the closest level.
fn pi_thinking_level(value: &Value) -> Option<String> {
    let map = |effort: &str| -> Option<String> {
        match effort {
            "off" | "minimal" | "low" | "medium" | "high" | "xhigh" => Some(effort.into()),
            "none" => Some("off".into()),
            "on" => Some("medium".into()),
            "max" | "ultra" => Some("high".into()),
            _ => None,
        }
    };
    if let Some(effort) = value.as_str().filter(|effort| !effort.is_empty()) {
        return map(effort);
    }
    if value.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Some("off".into());
    }
    value
        .get("effort")
        .and_then(Value::as_str)
        .and_then(map)
        .or_else(|| {
            (value.get("enabled").and_then(Value::as_bool) == Some(true)).then(|| "medium".into())
        })
}

fn parse_pi_models(catalog: &Value, current: Option<(&str, &str)>) -> Vec<AgentModelOption> {
    let items = catalog
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let current_is_listed = current.is_some_and(|(provider, id)| {
        items.iter().any(|model| {
            model.get("provider").and_then(Value::as_str) == Some(provider)
                && model.get("id").and_then(Value::as_str) == Some(id)
        })
    });
    items
        .iter()
        .enumerate()
        .filter_map(|(index, model)| {
            let provider = model.get("provider").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            let reasoning = model.get("reasoning").and_then(Value::as_bool) == Some(true);
            // thinkingLevelMap marks unsupported levels with null; only the
            // mapped levels are offered. Models without a map accept the
            // standard range.
            let mapped_efforts = model
                .get("thinkingLevelMap")
                .and_then(Value::as_object)
                .map(|levels| {
                    ["off", "minimal", "low", "medium", "high", "xhigh"]
                        .into_iter()
                        .filter(|level| {
                            levels.get(*level).is_some_and(|value| !value.is_null())
                        })
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                });
            let reasoning_efforts = if !reasoning {
                Vec::new()
            } else {
                match mapped_efforts {
                    Some(levels) if !levels.is_empty() => levels,
                    _ => ["off", "low", "medium", "high"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                }
            };
            let default_reasoning_effort = reasoning_efforts
                .iter()
                .find(|level| *level == "medium")
                .or_else(|| reasoning_efforts.iter().find(|level| *level != "off"))
                .or_else(|| reasoning_efforts.first())
                .cloned();
            let is_default = if current_is_listed {
                current == Some((provider, id))
            } else {
                index == 0
            };
            Some(AgentModelOption {
                id: format!("{provider}/{id}"),
                label: model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned(),
                description: None,
                is_default,
                default_reasoning_effort,
                reasoning_efforts,
            })
        })
        .collect()
}

fn fallback_pi_models() -> Vec<AgentModelOption> {
    vec![
        AgentModelOption {
            id: "anthropic/claude-opus-4-5".into(),
            label: "Claude Opus 4.5".into(),
            description: Some("Pi 默认 Anthropic 模型 · 深度推理".into()),
            is_default: true,
            default_reasoning_effort: Some("medium".into()),
            reasoning_efforts: ["off", "low", "medium", "high"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        },
        AgentModelOption {
            id: "anthropic/claude-sonnet-4-20250514".into(),
            label: "Claude Sonnet 4".into(),
            description: Some("均衡编码模型".into()),
            is_default: false,
            default_reasoning_effort: Some("medium".into()),
            reasoning_efforts: ["off", "low", "medium", "high"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        },
    ]
}

async fn pi_content(input: &[Value]) -> Result<(String, Vec<Value>)> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => texts.push(
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some("localImage") => {
                let path = item
                    .get("path")
                    .and_then(Value::as_str)
                    .context("local image input has no path")?;
                let bytes = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("cannot read image {path}"))?;
                let mime_type = mime_guess::from_path(path)
                    .first_raw()
                    .unwrap_or("application/octet-stream");
                images.push(json!({
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                    "mimeType": mime_type,
                }));
            }
            Some(other) => bail!("unsupported Pi prompt content type {other}"),
            None => bail!("prompt content is missing its type"),
        }
    }
    let message = texts.join("\n");
    if message.is_empty() && images.is_empty() {
        bail!("Pi prompt content is empty")
    }
    Ok((message, images))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_model_requires_provider_and_id() {
        assert_eq!(
            split_model("anthropic/claude-opus-4-5"),
            Some(("anthropic", "claude-opus-4-5"))
        );
        assert_eq!(split_model("claude-opus-4-5"), None);
        assert_eq!(split_model("anthropic/"), None);
    }

    #[test]
    fn thinking_levels_map_cross_provider_vocabulary() {
        assert_eq!(pi_thinking_level(&json!("high")), Some("high".into()));
        assert_eq!(pi_thinking_level(&json!("on")), Some("medium".into()));
        assert_eq!(pi_thinking_level(&json!("max")), Some("high".into()));
        assert_eq!(
            pi_thinking_level(&json!({"enabled":false})),
            Some("off".into())
        );
        assert_eq!(
            pi_thinking_level(&json!({"enabled":true,"effort":"minimal"})),
            Some("minimal".into())
        );
        assert_eq!(pi_thinking_level(&json!("unknown")), None);
    }

    #[test]
    fn pi_catalog_uses_provider_qualified_ids() {
        let models = parse_pi_models(
            &json!({"models":[{
                "id":"claude-opus-4-5",
                "name":"Claude Opus 4.5",
                "provider":"anthropic",
                "reasoning":true
            }]}),
            None,
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "anthropic/claude-opus-4-5");
        assert!(models[0].is_default);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn pi_catalog_marks_the_effective_model_and_honors_thinking_map() {
        let catalog = json!({"models":[
            {
                "id":"deepseek-v4-pro",
                "name":"DeepSeek V4 Pro",
                "provider":"deepseek",
                "reasoning":true,
                "thinkingLevelMap":{"minimal":null,"low":null,"medium":null,"high":"high","xhigh":"max"}
            },
            {
                "id":"k3",
                "name":"Kimi K3",
                "provider":"kimi-coding",
                "reasoning":true
            }
        ]});
        let models = parse_pi_models(&catalog, Some(("kimi-coding", "k3")));
        assert_eq!(models.len(), 2);
        assert!(!models[0].is_default);
        assert_eq!(models[0].reasoning_efforts, vec!["high".to_owned(), "xhigh".to_owned()]);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("high"));
        assert!(models[1].is_default);
        assert_eq!(models[1].default_reasoning_effort.as_deref(), Some("medium"));
    }

    #[tokio::test]
    async fn parses_session_active_branch_into_turns() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join("2026-07-20T23-24-27-064Z_019f81d8-5838-7252-a4f9-166844e7144e.jsonl");
        let body = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"019f81d8-5838-7252-a4f9-166844e7144e\",\"timestamp\":\"2026-07-20T23:24:27.064Z\",\"cwd\":\"/work/project\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"timestamp\":\"2026-07-20T23:24:56.994Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hello pi\"}],\"timestamp\":1}}\n",
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"u1\",\"timestamp\":\"2026-07-20T23:24:57.000Z\",\"name\":\"Named session\"}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"n1\",\"timestamp\":\"2026-07-20T23:24:58.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hmm\"},{\"type\":\"text\",\"text\":\"hi there\"}],\"stopReason\":\"stop\",\"timestamp\":2}}\n",
            "{\"type\":\"message\",\"id\":\"t1\",\"parentId\":\"a1\",\"timestamp\":\"2026-07-20T23:24:59.000Z\",\"message\":{\"role\":\"toolResult\",\"toolCallId\":\"call_1\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}],\"isError\":false,\"timestamp\":3}}\n"
        );
        std::fs::write(&path, body).unwrap();

        let thread = parse_pi_session(&path, false, true).await.unwrap();

        assert_eq!(thread["id"], "019f81d8-5838-7252-a4f9-166844e7144e");
        assert_eq!(thread["name"], "Named session");
        assert_eq!(thread["cwd"], "/work/project");
        assert_eq!(thread["status"], "idle");
        assert_eq!(thread["preview"], "hello pi");
        let turns = thread["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["status"], "completed");
        let kinds: Vec<&str> = turns[0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec!["userMessage", "reasoning", "agentMessage", "commandExecution"]
        );
    }

    #[tokio::test]
    async fn streaming_marks_only_the_last_turn_in_progress() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("2026-07-20T23-24-27-064Z_abc.jsonl");
        let body = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"abc\",\"timestamp\":\"2026-07-20T23:24:27.064Z\",\"cwd\":\"/work\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"timestamp\":\"2026-07-20T23:24:56.994Z\",\"message\":{\"role\":\"user\",\"content\":\"one\",\"timestamp\":1}}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"timestamp\":\"2026-07-20T23:24:57.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"1\"}],\"stopReason\":\"stop\",\"timestamp\":2}}\n",
            "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"timestamp\":\"2026-07-20T23:25:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"two\",\"timestamp\":3}}\n"
        );
        std::fs::write(&path, body).unwrap();

        let thread = parse_pi_session(&path, true, true).await.unwrap();
        let turns = thread["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["status"], "completed");
        assert_eq!(turns[1]["status"], "inProgress");
        assert_eq!(thread["status"], "active");
    }
}
