use crate::{
    config::ClientConfig,
    protocol::{
        AgentModelOption, AgentProvider, AgentProviderStatus, ConversationAccessMode, new_id,
    },
};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use directories::BaseDirs;
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Child,
    sync::{Mutex, Notify, broadcast},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use url::Url;

const API_PREFIX: &str = "/api/v1";
const DEFAULT_MODEL: &str = "kimi-code/k3";
const DEFAULT_THINKING: &str = "max";

#[derive(Clone)]
pub struct KimiRuntime {
    config: Arc<ClientConfig>,
    http: reqwest::Client,
    startup: Arc<Mutex<()>>,
    process: Arc<Mutex<Option<Child>>>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    approvals_seen: Arc<Mutex<HashSet<String>>>,
    subscriptions_changed: Arc<Notify>,
    events: broadcast::Sender<Value>,
    owns_process: bool,
}

impl KimiRuntime {
    pub fn new(config: Arc<ClientConfig>) -> Self {
        Self::with_process_ownership(config, false)
    }

    pub fn new_host(config: Arc<ClientConfig>) -> Self {
        Self::with_process_ownership(config, true)
    }

    fn with_process_ownership(config: Arc<ClientConfig>, owns_process: bool) -> Self {
        let (events, _) = broadcast::channel(4096);
        Self {
            config,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(60))
                .build()
                .expect("static Kimi HTTP client configuration"),
            startup: Arc::new(Mutex::new(())),
            process: Arc::new(Mutex::new(None)),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            approvals_seen: Arc::new(Mutex::new(HashSet::new())),
            subscriptions_changed: Arc::new(Notify::new()),
            events,
            owns_process,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    pub async fn subscribe_session(&self, session_id: &str) {
        if self
            .subscriptions
            .lock()
            .await
            .insert(session_id.to_owned())
        {
            self.subscriptions_changed.notify_one();
        }
    }

    pub async fn provider_status(&self) -> AgentProviderStatus {
        if let Ok(meta) = self.probe_meta().await {
            let models = match tokio::time::timeout(Duration::from_secs(3), self.model_catalog())
                .await
            {
                Ok(Ok(models)) if !models.is_empty() => models,
                Ok(Ok(_)) => fallback_kimi_models(),
                Ok(Err(error)) => {
                    tracing::warn!(error=?error, "Kimi model catalog unavailable; using fallback");
                    fallback_kimi_models()
                }
                Err(_) => fallback_kimi_models(),
            };
            return AgentProviderStatus {
                provider: AgentProvider::Kimi,
                label: "Kimi".into(),
                available: true,
                status: "online".into(),
                version: meta
                    .get("server_version")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                models,
            };
        }
        let available = crate::probe::command_available(&self.config.kimi_command);
        let version = if available {
            crate::probe::command_version(&self.config.kimi_command, &["--version"]).await
        } else {
            None
        };
        AgentProviderStatus {
            provider: AgentProvider::Kimi,
            label: "Kimi".into(),
            available,
            status: if available { "stopped" } else { "unavailable" }.into(),
            version,
            models: if available {
                fallback_kimi_models()
            } else {
                Vec::new()
            },
        }
    }

    async fn model_catalog(&self) -> Result<Vec<AgentModelOption>> {
        let catalog = self
            .request_without_start(Method::GET, "/models", None)
            .await?;
        Ok(parse_kimi_models(&catalog))
    }

    pub async fn ensure_ready(&self) -> Result<Value> {
        if let Ok(meta) = self.probe_meta().await {
            return Ok(meta);
        }
        let _startup = self.startup.lock().await;
        if let Ok(meta) = self.probe_meta().await {
            return Ok(meta);
        }
        if !self.owns_process {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            loop {
                if let Ok(meta) = self.probe_meta().await {
                    return Ok(meta);
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!(
                        "Kimi service managed by the Agent Host did not become ready at {}",
                        self.config.kimi_server_url
                    )
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        {
            let mut process = self.process.lock().await;
            let must_start = match process.as_mut() {
                Some(child) => child.try_wait()?.is_some(),
                None => true,
            };
            if must_start {
                let _ = process.take();
                let mut command = crate::probe::provider_command(&self.config.kimi_command);
                command
                    .args(&self.config.kimi_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    // The Agent Host can rotate independently of provider work. An
                    // explicitly owned process is stopped through `shutdown`; an
                    // abrupt Host replacement must not terminate active Kimi turns.
                    .kill_on_drop(false);
                *process =
                    Some(command.spawn().with_context(|| {
                        format!("failed to start `{}`", self.config.kimi_command)
                    })?);
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(meta) = self.probe_meta().await {
                return Ok(meta);
            }
            if let Some(status) = self.reap_owned_process().await?
                && !status.success()
            {
                bail!("Kimi launcher exited before becoming ready ({status})")
            }
            if tokio::time::Instant::now() >= deadline {
                self.stop_owned_process().await;
                bail!(
                    "Kimi server did not become ready at {}",
                    self.config.kimi_server_url
                )
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn create_session(
        &self,
        cwd: &Path,
        title: &str,
        access_mode: ConversationAccessMode,
        options: &Value,
    ) -> Result<Value> {
        let mut agent_config = serde_json::Map::new();
        agent_config.insert(
            "permission_mode".into(),
            json!(permission_mode(access_mode)),
        );
        agent_config.insert(
            "model".into(),
            options
                .get("model")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!(DEFAULT_MODEL)),
        );
        agent_config.insert(
            "thinking".into(),
            options
                .get("thinking")
                .and_then(kimi_thinking_value)
                .unwrap_or_else(|| json!(DEFAULT_THINKING)),
        );
        for key in ["plan_mode", "swarm_mode"] {
            copy_option(options, &mut agent_config, key);
        }
        self.request(
            Method::POST,
            "/sessions",
            Some(json!({
                "title": title,
                "metadata": {"cwd": cwd},
                "agent_config": agent_config,
            })),
        )
        .await
    }

    pub async fn session(&self, session_id: &str) -> Result<Value> {
        self.request(Method::GET, &format!("/sessions/{session_id}"), None)
            .await
    }

    pub async fn snapshot(&self, session_id: &str) -> Result<Value> {
        let mut snapshot = self
            .request(
                Method::GET,
                &format!("/sessions/{session_id}/snapshot"),
                None,
            )
            .await?;
        for _ in 0..100 {
            let has_more = snapshot
                .pointer("/messages/has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_more {
                self.publish_pending_approvals(&snapshot).await;
                return Ok(snapshot);
            }
            let before_id = snapshot
                .pointer("/messages/items/0/id")
                .and_then(Value::as_str)
                .context("Kimi snapshot reports older messages without a cursor")?;
            let encoded =
                url::form_urlencoded::byte_serialize(before_id.as_bytes()).collect::<String>();
            let page = self
                .request(
                    Method::GET,
                    &format!("/sessions/{session_id}/messages?page_size=100&before_id={encoded}"),
                    None,
                )
                .await?;
            let mut older = page
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            older.reverse();
            let current = snapshot
                .pointer_mut("/messages/items")
                .and_then(Value::as_array_mut)
                .context("Kimi snapshot messages are missing")?;
            older.append(current);
            *current = older;
            snapshot["messages"]["has_more"] =
                page.get("has_more").cloned().unwrap_or(Value::Bool(false));
        }
        bail!("Kimi message pagination exceeded the 10,000-message safety limit")
    }

    async fn publish_pending_approvals(&self, snapshot: &Value) {
        let Some(session_id) = snapshot.pointer("/session/id").and_then(Value::as_str) else {
            return;
        };
        for approval in snapshot
            .get("pending_approvals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(approval_id) = approval.get("approval_id").and_then(Value::as_str) else {
                continue;
            };
            let key = format!("{session_id}:{approval_id}");
            if self.approvals_seen.lock().await.insert(key) {
                let _ = self.events.send(json!({
                    "type":"event.approval.requested",
                    "session_id":session_id,
                    "payload":approval,
                }));
            }
        }
    }

    pub async fn list_sessions(&self, archived: bool) -> Result<Vec<Value>> {
        let mut before: Option<String> = None;
        let mut sessions = Vec::new();
        for _ in 0..100 {
            let mut path = if archived {
                "/sessions?page_size=100&archived_only=true".to_owned()
            } else {
                "/sessions?page_size=100".to_owned()
            };
            if let Some(cursor) = &before {
                path.push_str("&before_id=");
                path.push_str(
                    &url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect::<String>(),
                );
            }
            let page = self.request(Method::GET, &path, None).await?;
            let items = page
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let has_more = page
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            before = items
                .last()
                .and_then(|session| session.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            sessions.extend(items);
            if !has_more || before.is_none() {
                return Ok(sessions);
            }
        }
        bail!("Kimi session pagination exceeded the 10,000-session safety limit")
    }

    pub async fn archive_session(&self, session_id: &str, archived: bool) -> Result<Value> {
        let action = if archived { "archive" } else { "restore" };
        self.request(
            Method::POST,
            &format!("/sessions/{session_id}:{action}"),
            Some(json!({})),
        )
        .await
    }

    pub async fn submit_prompt(
        &self,
        session_id: &str,
        input: &[Value],
        access_mode: ConversationAccessMode,
        options: &Value,
    ) -> Result<Value> {
        let content = kimi_content(input).await?;
        let mut body = serde_json::Map::new();
        body.insert("content".into(), Value::Array(content));
        body.insert(
            "permission_mode".into(),
            json!(permission_mode(access_mode)),
        );
        apply_prompt_options(options, &mut body);
        self.request(
            Method::POST,
            &format!("/sessions/{session_id}/prompts"),
            Some(Value::Object(body)),
        )
        .await
    }

    pub async fn steer_prompt(
        &self,
        session_id: &str,
        input: &[Value],
        access_mode: ConversationAccessMode,
        options: &Value,
    ) -> Result<Value> {
        let submitted = self
            .submit_prompt(session_id, input, access_mode, options)
            .await?;
        let prompt_id = submitted
            .get("prompt_id")
            .and_then(Value::as_str)
            .context("Kimi prompt response has no prompt_id")?;
        if submitted.get("status").and_then(Value::as_str) == Some("queued") {
            let steered = self
                .request(
                    Method::POST,
                    &format!("/sessions/{session_id}/prompts:steer"),
                    Some(json!({"prompt_ids":[prompt_id]})),
                )
                .await?;
            return Ok(json!({"prompt":submitted,"steer":steered}));
        }
        Ok(json!({"prompt":submitted,"alreadyActive":true}))
    }

    pub async fn interrupt(&self, session_id: &str) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("/sessions/{session_id}:abort"),
            Some(json!({})),
        )
        .await
    }

    pub async fn resolve_approval(
        &self,
        session_id: &str,
        approval_id: &str,
        decision: &str,
    ) -> Result<Value> {
        let (decision, scope) = match decision {
            "accept" => ("approved", None),
            "accept_for_session" => ("approved", Some("session")),
            "decline" => ("rejected", None),
            "cancel" => ("cancelled", None),
            other => bail!("unsupported Kimi approval decision {other}"),
        };
        self.request(
            Method::POST,
            &format!("/sessions/{session_id}/approvals/{approval_id}"),
            Some(json!({"decision":decision,"scope":scope})),
        )
        .await
    }

    pub async fn shutdown(&self) {
        self.stop_owned_process().await;
    }

    pub async fn run_event_stream(self) {
        loop {
            let sessions = self.subscriptions.lock().await.clone();
            if sessions.is_empty() {
                self.subscriptions_changed.notified().await;
                continue;
            }
            if let Err(error) = self.run_event_connection(sessions).await {
                tracing::warn!(error=?error, "Kimi event stream disconnected");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    async fn run_event_connection(&self, sessions: HashSet<String>) -> Result<()> {
        self.ensure_ready().await?;
        let token = self.token().await?;
        let client_id = new_id("nuntius");
        let mut ws_url = Url::parse(&self.config.kimi_server_url)?;
        ws_url
            .set_scheme("ws")
            .map_err(|_| anyhow!("invalid Kimi WebSocket URL"))?;
        ws_url.set_path(&format!("{API_PREFIX}/ws"));
        ws_url.set_query(Some(&format!("client_id={client_id}")));
        let mut request = ws_url.as_str().into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .context("invalid Kimi token header")?,
        );
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(request))
            .await
            .context("Kimi WebSocket connect timed out")??;
        let (mut sink, mut stream) = socket.split();
        let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .context("Kimi WebSocket hello timed out")?
            .context("Kimi WebSocket closed before server hello")??;
        let Message::Text(first) = first else {
            bail!("Kimi WebSocket did not begin with a text server hello")
        };
        let first: Value = serde_json::from_str(&first)?;
        if first.get("type").and_then(Value::as_str) != Some("server_hello") {
            bail!("Kimi WebSocket did not begin with server_hello")
        }
        let hello_id = new_id("hello");
        sink.send(Message::Text(
            json!({
                "type":"client_hello",
                "id":hello_id,
                "payload":{
                    "client_id":client_id,
                    "subscriptions":sessions,
                    "cursors":{},
                }
            })
            .to_string()
            .into(),
        ))
        .await?;
        loop {
            tokio::select! {
                _ = self.subscriptions_changed.notified() => return Ok(()),
                message = stream.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let value: Value = serde_json::from_str(&text)?;
                            if value.get("type").and_then(Value::as_str) == Some("ping") {
                                let nonce = value.pointer("/payload/nonce").cloned().unwrap_or(Value::Null);
                                sink.send(Message::Text(json!({"type":"pong","payload":{"nonce":nonce}}).to_string().into())).await?;
                            } else if value.get("type").and_then(Value::as_str) == Some("resync_required") {
                                if let Some(session_id) = value.pointer("/payload/session_id").and_then(Value::as_str) {
                                    let _ = self.events.send(json!({
                                        "type":"nuntius.resync_required",
                                        "session_id":session_id,
                                        "payload":value.get("payload"),
                                    }));
                                }
                            } else if value.get("type").and_then(Value::as_str) == Some("ack") {
                                for session_id in value.pointer("/payload/resync_required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                                    let _ = self.events.send(json!({
                                        "type":"nuntius.resync_required",
                                        "session_id":session_id,
                                        "payload":{"reason":"handshake"},
                                    }));
                                }
                            } else if value.get("session_id").is_some() {
                                if value.get("type").and_then(Value::as_str) == Some("event.approval.requested")
                                    && let (Some(session_id), Some(approval_id)) = (
                                        value.get("session_id").and_then(Value::as_str),
                                        value.pointer("/payload/approval_id").and_then(Value::as_str),
                                    )
                                {
                                    self.approvals_seen.lock().await.insert(format!("{session_id}:{approval_id}"));
                                }
                                let _ = self.events.send(value);
                            }
                        }
                        Some(Ok(Message::Ping(bytes))) => sink.send(Message::Pong(bytes)).await?,
                        Some(Ok(Message::Close(_))) | None => bail!("Kimi WebSocket closed"),
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error.into()),
                    }
                }
            }
        }
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        self.ensure_ready().await?;
        self.request_without_start(method, path, body).await
    }

    async fn probe_meta(&self) -> Result<Value> {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.request_without_start(Method::GET, "/meta", None),
        )
        .await
        .context("Kimi metadata probe timed out")?
    }

    async fn request_without_start(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let token = self.token().await?;
        let url = Url::parse(&self.config.kimi_server_url)?.join(&format!("{API_PREFIX}{path}"))?;
        let mut request = self.http.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let envelope: Value = response
            .json()
            .await
            .context("invalid Kimi JSON response")?;
        let code = envelope.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if !status.is_success() || code != 0 {
            let message = envelope
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown Kimi server error")
                .replace(['\r', '\n'], " ");
            bail!("Kimi API {path} failed ({status}, code {code}): {message}")
        }
        Ok(envelope.get("data").cloned().unwrap_or(Value::Null))
    }

    async fn token(&self) -> Result<String> {
        let path = kimi_home()
            .context("cannot resolve Kimi home directory")?
            .join("server.token");
        let token = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("cannot read Kimi server token {}", path.display()))?;
        let token = token.trim();
        if token.is_empty() {
            bail!("Kimi server token is empty")
        }
        Ok(token.to_owned())
    }

    async fn reap_owned_process(&self) -> Result<Option<std::process::ExitStatus>> {
        let mut process = self.process.lock().await;
        let status = match process.as_mut() {
            Some(child) => child.try_wait()?,
            None => None,
        };
        if status.is_some() {
            let _ = process.take();
        }
        Ok(status)
    }

    async fn stop_owned_process(&self) {
        if let Some(mut child) = self.process.lock().await.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

fn kimi_home() -> Option<PathBuf> {
    std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".kimi-code")))
}

fn parse_kimi_models(catalog: &Value) -> Vec<AgentModelOption> {
    catalog
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("model").and_then(Value::as_str)?.to_owned();
            let is_k3 = id.ends_with("/k3");
            let mut reasoning_efforts = item
                .get("support_efforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if reasoning_efforts.is_empty() {
                reasoning_efforts.push("on".into());
            }
            let default_reasoning_effort = if is_k3 {
                Some("max".into())
            } else {
                Some("on".into())
            };
            Some(AgentModelOption {
                id,
                label: item
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or("Kimi")
                    .to_owned(),
                description: Some(if is_k3 {
                    "Kimi 旗舰编程模型 · 最高 1M 上下文".into()
                } else if item
                    .get("model")
                    .and_then(Value::as_str)
                    .is_some_and(|model| model.ends_with("-highspeed"))
                {
                    "K2.7 Code · 高速输出".into()
                } else {
                    "K2.7 Code · 稳定编程模型".into()
                }),
                is_default: is_k3,
                default_reasoning_effort,
                reasoning_efforts,
            })
        })
        .collect()
}

fn fallback_kimi_models() -> Vec<AgentModelOption> {
    vec![
        AgentModelOption {
            id: "kimi-code/k3".into(),
            label: "K3".into(),
            description: Some("Kimi 旗舰编程模型 · 最高 1M 上下文".into()),
            is_default: true,
            default_reasoning_effort: Some("max".into()),
            reasoning_efforts: ["low", "high", "max"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        },
        AgentModelOption {
            id: "kimi-code/kimi-for-coding".into(),
            label: "K2.7 Coding".into(),
            description: Some("K2.7 Code · 稳定编程模型".into()),
            is_default: false,
            default_reasoning_effort: Some("on".into()),
            reasoning_efforts: vec!["on".into()],
        },
        AgentModelOption {
            id: "kimi-code/kimi-for-coding-highspeed".into(),
            label: "K2.7 Coding Highspeed".into(),
            description: Some("K2.7 Code · 高速输出".into()),
            is_default: false,
            default_reasoning_effort: Some("on".into()),
            reasoning_efforts: vec!["on".into()],
        },
    ]
}

fn permission_mode(mode: ConversationAccessMode) -> &'static str {
    match mode {
        ConversationAccessMode::Full => "yolo",
        ConversationAccessMode::Ask => "manual",
    }
}

fn copy_option(source: &Value, target: &mut serde_json::Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key) {
        target.insert(key.into(), value.clone());
    }
}

fn apply_prompt_options(options: &Value, body: &mut serde_json::Map<String, Value>) {
    body.insert(
        "model".into(),
        options
            .get("model")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!(DEFAULT_MODEL)),
    );
    body.insert(
        "thinking".into(),
        options
            .get("thinking")
            .and_then(kimi_thinking_value)
            .unwrap_or_else(|| json!(DEFAULT_THINKING)),
    );
    copy_option(options, body, "plan_mode");
    copy_option(options, body, "swarm_mode");
}

fn kimi_thinking_value(value: &Value) -> Option<Value> {
    if let Some(effort) = value.as_str().filter(|effort| !effort.is_empty()) {
        return Some(json!(effort));
    }
    if value.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Some(json!("none"));
    }
    value
        .get("effort")
        .and_then(Value::as_str)
        .filter(|effort| !effort.is_empty())
        .map(|effort| json!(effort))
        .or_else(|| {
            (value.get("enabled").and_then(Value::as_bool) == Some(true)).then(|| json!("on"))
        })
}

async fn kimi_content(input: &[Value]) -> Result<Vec<Value>> {
    let mut content = Vec::with_capacity(input.len());
    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => content.push(json!({
                "type":"text",
                "text":item.get("text").and_then(Value::as_str).unwrap_or_default(),
            })),
            Some("localImage") => {
                let path = item
                    .get("path")
                    .and_then(Value::as_str)
                    .context("local image input has no path")?;
                let bytes = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("cannot read image {path}"))?;
                let media_type = mime_guess::from_path(path)
                    .first_raw()
                    .unwrap_or("application/octet-stream");
                content.push(json!({
                    "type":"image",
                    "source":{
                        "kind":"base64",
                        "media_type":media_type,
                        "data":base64::engine::general_purpose::STANDARD.encode(bytes),
                    }
                }));
            }
            Some(other) => bail!("unsupported Kimi prompt content type {other}"),
            None => bail!("prompt content is missing its type"),
        }
    }
    if content.is_empty() {
        bail!("Kimi prompt content is empty")
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_catalog_defaults_to_k3_and_max_effort() {
        let models = parse_kimi_models(&json!({"items":[{
            "model":"kimi-code/k3",
            "display_name":"K3",
            "support_efforts":["low","high","max"],
            "default_effort":"high"
        }]}));
        assert_eq!(models.len(), 1);
        assert!(models[0].is_default);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn kimi_thinking_options_are_encoded_as_web_api_strings() {
        assert_eq!(kimi_thinking_value(&json!("max")), Some(json!("max")));
        assert_eq!(
            kimi_thinking_value(&json!({"enabled":true,"effort":"high"})),
            Some(json!("high"))
        );
        assert_eq!(
            kimi_thinking_value(&json!({"enabled":false})),
            Some(json!("none"))
        );
    }

    #[test]
    fn kimi_prompt_options_always_include_a_model_and_thinking_effort() {
        let mut defaults = serde_json::Map::new();
        apply_prompt_options(&json!({}), &mut defaults);
        assert_eq!(
            Value::Object(defaults),
            json!({"model":"kimi-code/k3","thinking":"max"})
        );

        let mut selected = serde_json::Map::new();
        apply_prompt_options(
            &json!({
                "model": "kimi-code/kimi-for-coding-highspeed",
                "thinking": "on",
                "plan_mode": true
            }),
            &mut selected,
        );
        assert_eq!(
            Value::Object(selected),
            json!({
                "model": "kimi-code/kimi-for-coding-highspeed",
                "thinking": "on",
                "plan_mode": true
            })
        );
    }
}
