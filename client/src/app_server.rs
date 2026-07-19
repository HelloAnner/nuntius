use crate::config::ClientConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, broadcast, mpsc, oneshot},
};

#[derive(Clone)]
pub struct AppServerRuntime {
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
        let mut guard = self.session.lock().await;
        let must_start = match guard.as_mut() {
            Some(session) => {
                session.child.try_wait()?.is_some()
                    || !session.reader_alive.load(Ordering::Relaxed)
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
        handle.request(method, params).await
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
        let mut command = Command::new(&config.codex_command);
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
                                        value.get("result").is_some() || value.get("error").is_some()
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
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, std::time::Duration::from_secs(60))
            .await
    }

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
            bail!("App Server {method} failed with code {code}")
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
