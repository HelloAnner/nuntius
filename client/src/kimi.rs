use crate::{
    config::ClientConfig,
    protocol::{AgentProvider, AgentProviderStatus, ConversationAccessMode, new_id},
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
    process::Command,
    sync::{Mutex, Notify, broadcast},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use url::Url;

const API_PREFIX: &str = "/api/v1";

#[derive(Clone)]
pub struct KimiRuntime {
    config: Arc<ClientConfig>,
    http: reqwest::Client,
    startup: Arc<Mutex<()>>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    subscriptions_changed: Arc<Notify>,
    events: broadcast::Sender<Value>,
}

impl KimiRuntime {
    pub fn new(config: Arc<ClientConfig>) -> Self {
        let (events, _) = broadcast::channel(4096);
        Self {
            config,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(60))
                .build()
                .expect("static Kimi HTTP client configuration"),
            startup: Arc::new(Mutex::new(())),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            subscriptions_changed: Arc::new(Notify::new()),
            events,
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
            self.subscriptions_changed.notify_waiters();
        }
    }

    pub async fn provider_status(&self) -> AgentProviderStatus {
        if let Ok(meta) = self.request_without_start(Method::GET, "/meta", None).await {
            return AgentProviderStatus {
                provider: AgentProvider::Kimi,
                label: "Kimi".into(),
                available: true,
                status: "online".into(),
                version: meta
                    .get("server_version")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
        }
        let version = tokio::time::timeout(
            Duration::from_secs(3),
            Command::new(&self.config.kimi_command)
                .arg("--version")
                .output(),
        )
        .await
        .ok()
        .and_then(|result| result.ok())
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
        AgentProviderStatus {
            provider: AgentProvider::Kimi,
            label: "Kimi".into(),
            available: version.is_some(),
            status: if version.is_some() {
                "stopped"
            } else {
                "unavailable"
            }
            .into(),
            version,
        }
    }

    pub async fn ensure_ready(&self) -> Result<Value> {
        if let Ok(meta) = self.request_without_start(Method::GET, "/meta", None).await {
            return Ok(meta);
        }
        let _startup = self.startup.lock().await;
        if let Ok(meta) = self.request_without_start(Method::GET, "/meta", None).await {
            return Ok(meta);
        }
        let status = tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(&self.config.kimi_command)
                .args(&self.config.kimi_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await
        .context("starting Kimi timed out")?
        .with_context(|| format!("failed to start `{}`", self.config.kimi_command))?;
        if !status.success() {
            bail!("Kimi launcher exited with {status}")
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(meta) = self.request_without_start(Method::GET, "/meta", None).await {
                return Ok(meta);
            }
            if tokio::time::Instant::now() >= deadline {
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
    ) -> Result<Value> {
        self.request(
            Method::POST,
            "/sessions",
            Some(json!({
                "title": title,
                "metadata": {"cwd": cwd},
                "agent_config": {"permission_mode": permission_mode(access_mode)},
            })),
        )
        .await
    }

    pub async fn session(&self, session_id: &str) -> Result<Value> {
        self.request(Method::GET, &format!("/sessions/{session_id}"), None)
            .await
    }

    pub async fn snapshot(&self, session_id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/sessions/{session_id}/snapshot"),
            None,
        )
        .await
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
        copy_option(options, &mut body, "model");
        copy_option(options, &mut body, "thinking");
        copy_option(options, &mut body, "plan_mode");
        copy_option(options, &mut body, "swarm_mode");
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
                            } else if value.get("session_id").is_some() {
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
}

fn kimi_home() -> Option<PathBuf> {
    std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".kimi-code")))
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
