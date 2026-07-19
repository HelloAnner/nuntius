use crate::{
    app_server::AppServerRuntime,
    config::ClientConfig,
    kimi::KimiRuntime,
    protocol::{AgentProvider, AgentProviderStatus, ConversationAccessMode},
};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::process::Command;

#[derive(Clone)]
pub struct AgentRuntimes {
    pub codex: AppServerRuntime,
    pub kimi: KimiRuntime,
    config: Arc<ClientConfig>,
}

#[derive(Debug, Clone)]
pub struct AgentThreadState {
    pub status: String,
    pub active_turn_id: Option<String>,
}

impl AgentRuntimes {
    pub fn new(config: Arc<ClientConfig>) -> Self {
        Self {
            codex: AppServerRuntime::new(config.clone()),
            kimi: KimiRuntime::new(config.clone()),
            config,
        }
    }

    pub async fn statuses(&self) -> Vec<AgentProviderStatus> {
        let codex_running = self.codex.is_running().await;
        let codex_version = if codex_running {
            None
        } else {
            tokio::time::timeout(
                Duration::from_secs(3),
                Command::new(&self.config.codex_command)
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
        let codex_available = codex_running || codex_version.is_some();
        let codex = AgentProviderStatus {
            provider: AgentProvider::Codex,
            label: "Codex".into(),
            available: codex_available,
            status: if codex_running {
                "online"
            } else if codex_available {
                "stopped"
            } else {
                "unavailable"
            }
            .into(),
            version: codex_version,
        };
        vec![codex, self.kimi.provider_status().await]
    }

    pub async fn create_session(
        &self,
        provider: AgentProvider,
        cwd: &Path,
        title: &str,
        access_mode: ConversationAccessMode,
        options: Value,
    ) -> Result<String> {
        match provider {
            AgentProvider::Codex => {
                let mut params = object(options);
                let defaults = codex_thread_access(access_mode);
                for (key, value) in defaults {
                    params.entry(key).or_insert(value);
                }
                params.insert("cwd".into(), json!(cwd));
                let result = self
                    .codex
                    .call("thread/start", Value::Object(params))
                    .await?;
                extract_id(&result, &["thread/id", "threadId", "id"])
                    .context("thread/start response has no thread id")
            }
            AgentProvider::Kimi => {
                let result = self.kimi.create_session(cwd, title, access_mode).await?;
                let id = extract_id(&result, &["id"])
                    .context("Kimi create-session response has no session id")?;
                self.kimi.subscribe_session(&id).await;
                Ok(id)
            }
        }
    }

    pub async fn archive_session(
        &self,
        provider: AgentProvider,
        session_id: &str,
        archived: bool,
    ) -> Result<Value> {
        match provider {
            AgentProvider::Codex => {
                let method = if archived {
                    "thread/archive"
                } else {
                    "thread/unarchive"
                };
                self.codex
                    .call(method, json!({"threadId":session_id}))
                    .await
            }
            AgentProvider::Kimi => self.kimi.archive_session(session_id, archived).await,
        }
    }

    pub async fn thread_state(
        &self,
        provider: AgentProvider,
        session_id: &str,
    ) -> Result<AgentThreadState> {
        match provider {
            AgentProvider::Codex => {
                let result = self
                    .codex
                    .call(
                        "thread/resume",
                        json!({
                            "threadId": session_id,
                            "initialTurnsPage": {
                                "limit": 1,
                                "sortDirection": "desc",
                                "itemsView": "notLoaded"
                            }
                        }),
                    )
                    .await?;
                let thread = result.get("thread").unwrap_or(&result);
                let status = app_thread_status(thread).to_owned();
                let active_turn_id = result
                    .pointer("/initialTurnsPage/data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .chain(
                        thread
                            .get("turns")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten(),
                    )
                    .find(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Ok(AgentThreadState {
                    status,
                    active_turn_id,
                })
            }
            AgentProvider::Kimi => {
                let session = self.kimi.session(session_id).await?;
                Ok(AgentThreadState {
                    status: if session.get("busy").and_then(Value::as_bool) == Some(true) {
                        "active".into()
                    } else {
                        "idle".into()
                    },
                    active_turn_id: session
                        .get("current_prompt_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            }
        }
    }

    pub async fn start_turn(
        &self,
        provider: AgentProvider,
        session_id: &str,
        input: &[Value],
        access_mode: ConversationAccessMode,
        options: &Value,
        client_message_id: Option<&str>,
    ) -> Result<Value> {
        match provider {
            AgentProvider::Codex => {
                let mut params = object(options.clone());
                for (key, value) in codex_turn_access(access_mode) {
                    params.entry(key).or_insert(value);
                }
                params.insert("threadId".into(), json!(session_id));
                params.insert("input".into(), json!(input));
                if let Some(message_id) = client_message_id {
                    params.insert("clientUserMessageId".into(), json!(message_id));
                }
                self.codex.call("turn/start", Value::Object(params)).await
            }
            AgentProvider::Kimi => {
                self.kimi
                    .submit_prompt(session_id, input, access_mode, options)
                    .await
            }
        }
    }

    pub async fn steer_turn(
        &self,
        provider: AgentProvider,
        session_id: &str,
        active_turn_id: Option<&str>,
        input: &[Value],
        access_mode: ConversationAccessMode,
        options: &Value,
        client_message_id: Option<&str>,
    ) -> Result<Value> {
        match provider {
            AgentProvider::Codex => {
                let active_turn_id = active_turn_id.context("no active turn to steer")?;
                let mut params = json!({
                    "threadId":session_id,
                    "expectedTurnId":active_turn_id,
                    "input":input,
                });
                if let Some(message_id) = client_message_id {
                    params["clientUserMessageId"] = json!(message_id);
                }
                self.codex.call("turn/steer", params).await
            }
            AgentProvider::Kimi => {
                self.kimi
                    .steer_prompt(session_id, input, access_mode, options)
                    .await
            }
        }
    }

    pub async fn interrupt(
        &self,
        provider: AgentProvider,
        session_id: &str,
        active_turn_id: Option<&str>,
    ) -> Result<Value> {
        match provider {
            AgentProvider::Codex => {
                let active_turn_id = active_turn_id.context("no active turn to interrupt")?;
                self.codex
                    .call(
                        "turn/interrupt",
                        json!({"threadId":session_id,"turnId":active_turn_id}),
                    )
                    .await
            }
            AgentProvider::Kimi => self.kimi.interrupt(session_id).await,
        }
    }

    pub async fn read_thread(&self, provider: AgentProvider, session_id: &str) -> Result<Value> {
        match provider {
            AgentProvider::Codex => {
                let response = self
                    .codex
                    .call_with_timeout(
                        "thread/read",
                        json!({"threadId":session_id,"includeTurns":true}),
                        Duration::from_secs(180),
                    )
                    .await?;
                Ok(response.get("thread").unwrap_or(&response).clone())
            }
            AgentProvider::Kimi => {
                self.kimi.subscribe_session(session_id).await;
                let snapshot = self.kimi.snapshot(session_id).await?;
                normalize_kimi_snapshot(&snapshot)
            }
        }
    }

    pub async fn list_threads(
        &self,
        provider: AgentProvider,
        cwd: Option<&Path>,
        archived: bool,
    ) -> Result<Vec<Value>> {
        match provider {
            AgentProvider::Codex => {
                let mut cursor: Option<String> = None;
                let mut threads = Vec::new();
                for _ in 0..100 {
                    let mut params = json!({"limit":100,"archived":archived,"cursor":cursor});
                    if let Some(path) = cwd {
                        params["cwd"] = json!(path);
                    }
                    let response = self.codex.call("thread/list", params).await?;
                    threads.extend(
                        response
                            .get("data")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    cursor = response
                        .get("nextCursor")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if cursor.is_none() {
                        return Ok(threads);
                    }
                }
                bail!("Codex thread pagination exceeded the 10,000-thread safety limit")
            }
            AgentProvider::Kimi => {
                let sessions = self.kimi.list_sessions(archived).await?;
                let mut threads = Vec::new();
                for session in sessions {
                    let session_cwd = session.pointer("/metadata/cwd").and_then(Value::as_str);
                    if cwd.is_some_and(|path| session_cwd != path.to_str()) {
                        continue;
                    }
                    if let Some(id) = session.get("id").and_then(Value::as_str) {
                        self.kimi.subscribe_session(id).await;
                    }
                    threads.push(normalize_kimi_session(&session, None)?);
                }
                Ok(threads)
            }
        }
    }

    pub async fn resolve_approval(
        &self,
        provider: AgentProvider,
        session_id: Option<&str>,
        provider_request_id: Value,
        decision: &str,
        response: Option<Value>,
    ) -> Result<()> {
        match provider {
            AgentProvider::Codex => {
                let app_decision = if decision == "accept_for_session" {
                    "acceptForSession"
                } else {
                    decision
                };
                let response = response.unwrap_or_else(|| json!({"decision":app_decision}));
                self.codex.respond(provider_request_id, response).await
            }
            AgentProvider::Kimi => {
                let session_id = session_id.context("Kimi approval has no session")?;
                let approval_id = provider_request_id
                    .as_str()
                    .context("Kimi approval id is not a string")?;
                self.kimi
                    .resolve_approval(session_id, approval_id, decision)
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.codex.shutdown().await
    }
}

fn codex_thread_access(mode: ConversationAccessMode) -> Map<String, Value> {
    match mode {
        ConversationAccessMode::Full => object(json!({
            "approvalPolicy":"never",
            "sandbox":"danger-full-access",
        })),
        ConversationAccessMode::Ask => object(json!({
            "approvalPolicy":"on-request",
            "sandbox":"workspace-write",
        })),
    }
}

fn codex_turn_access(mode: ConversationAccessMode) -> Map<String, Value> {
    match mode {
        ConversationAccessMode::Full => object(json!({
            "approvalPolicy":"never",
            "sandboxPolicy":{"type":"dangerFullAccess"},
        })),
        ConversationAccessMode::Ask => object(json!({
            "approvalPolicy":"on-request",
            "sandboxPolicy":{"type":"workspaceWrite","networkAccess":false},
        })),
    }
}

fn normalize_kimi_snapshot(snapshot: &Value) -> Result<Value> {
    let session = snapshot
        .get("session")
        .context("Kimi snapshot has no session")?;
    let mut groups: Vec<(String, Vec<Value>, Option<i64>)> = Vec::new();
    for message in snapshot
        .pointer("/messages/items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("system");
        if role == "system" {
            continue;
        }
        let prompt_id = message
            .get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                (role != "user")
                    .then(|| groups.last().map(|group| group.0.clone()))
                    .flatten()
            })
            .unwrap_or_else(|| {
                message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned()
            });
        let item = normalize_kimi_message(message);
        let timestamp = rfc3339_unix(message.get("created_at"));
        if let Some(group) = groups.iter_mut().find(|group| group.0 == prompt_id) {
            group.1.push(item);
        } else {
            groups.push((prompt_id, vec![item], timestamp));
        }
    }
    let active_prompt = session.get("current_prompt_id").and_then(Value::as_str);
    if let Some(in_flight) = snapshot
        .get("in_flight_turn")
        .filter(|value| !value.is_null())
    {
        let prompt_id = in_flight
            .get("current_prompt_id")
            .and_then(Value::as_str)
            .or(active_prompt)
            .unwrap_or("kimi-in-flight")
            .to_owned();
        let group = if let Some(index) = groups.iter().position(|group| group.0 == prompt_id) {
            &mut groups[index]
        } else {
            groups.push((prompt_id.clone(), Vec::new(), None));
            groups.last_mut().expect("group appended")
        };
        if let Some(text) = in_flight
            .get("thinking_text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            group.1.push(json!({
                "id":format!("{prompt_id}:thinking"),
                "type":"reasoning",
                "status":"inProgress",
                "text":text,
                "structuredDetail":in_flight,
            }));
        }
        if let Some(text) = in_flight
            .get("assistant_text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            group.1.push(json!({
                "id":format!("{prompt_id}:assistant"),
                "type":"agentMessage",
                "status":"inProgress",
                "text":text,
                "structuredDetail":in_flight,
            }));
        }
    }
    let busy = session.get("busy").and_then(Value::as_bool) == Some(true);
    let turns = groups
        .into_iter()
        .map(|(id, items, started)| {
            let active = busy && active_prompt.is_none_or(|prompt| prompt == id);
            json!({
                "id":id,
                "status":if active {"inProgress"} else {"completed"},
                "startedAt":started,
                "completedAt":if active {Value::Null} else {started.map(Value::from).unwrap_or(Value::Null)},
                "items":items,
            })
        })
        .collect::<Vec<_>>();
    normalize_kimi_session(session, Some(turns))
}

fn normalize_kimi_session(session: &Value, turns: Option<Vec<Value>>) -> Result<Value> {
    let id = session
        .get("id")
        .and_then(Value::as_str)
        .context("Kimi session has no id")?;
    Ok(json!({
        "id":id,
        "name":session.get("title").and_then(Value::as_str).unwrap_or("Kimi 对话"),
        "preview":session.get("last_prompt"),
        "cwd":session.pointer("/metadata/cwd"),
        "status":if session.get("busy").and_then(Value::as_bool) == Some(true) {"active"} else {"idle"},
        "archived":session.get("archived").and_then(Value::as_bool).unwrap_or(false),
        "updatedAt":rfc3339_unix(session.get("updated_at")),
        "createdAt":rfc3339_unix(session.get("created_at")),
        "turns":turns.unwrap_or_default(),
    }))
}

fn normalize_kimi_message(message: &Value) -> Value {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("system");
    let kind = match role {
        "user" => "userMessage",
        "assistant" => "agentMessage",
        "tool" => "commandExecution",
        _ => "reasoning",
    };
    let mut text = Vec::new();
    for content in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(value) = content.get("text").and_then(Value::as_str) {
            text.push(value.to_owned());
        } else if let Some(value) = content.get("thinking").and_then(Value::as_str) {
            text.push(value.to_owned());
        } else if content.get("type").and_then(Value::as_str) == Some("tool_result") {
            text.push(
                content
                    .get("output")
                    .map(Value::to_string)
                    .unwrap_or_default(),
            );
        }
    }
    json!({
        "id":message.get("id"),
        "type":kind,
        "status":"completed",
        "text":text.join("\n"),
        "structuredDetail":message,
    })
}

fn rfc3339_unix(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_str)
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|value| value.unix_timestamp())
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn extract_id(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        let mut current = value;
        for part in path.split('/') {
            current = current.get(part)?;
        }
        current.as_str().map(str::to_owned)
    })
}

fn app_thread_status(thread: &Value) -> &str {
    thread
        .get("status")
        .and_then(|status| {
            status
                .as_str()
                .or_else(|| status.get("type").and_then(Value::as_str))
        })
        .unwrap_or("idle")
}
