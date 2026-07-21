use crate::{
    app_server::AppServerRuntime,
    config::ClientConfig,
    kimi::KimiRuntime,
    pi::PiRuntime,
    protocol::{AgentModelOption, AgentProvider, AgentProviderStatus, ConversationAccessMode},
};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::{path::Path, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct AgentRuntimes {
    pub codex: AppServerRuntime,
    pub kimi: KimiRuntime,
    pub pi: PiRuntime,
    config: Arc<ClientConfig>,
}

#[derive(Debug, Clone)]
pub struct AgentThreadState {
    pub status: String,
    pub active_turn_id: Option<String>,
}

impl AgentRuntimes {
    pub fn new(config: Arc<ClientConfig>) -> Result<Self> {
        #[cfg(unix)]
        let kimi = KimiRuntime::new(config.clone());
        #[cfg(not(unix))]
        let kimi = KimiRuntime::new_host(config.clone());
        Ok(Self {
            codex: AppServerRuntime::new(config.clone())?,
            kimi,
            pi: PiRuntime::new(config.clone()),
            config,
        })
    }

    #[cfg(test)]
    pub fn new_local(config: Arc<ClientConfig>) -> Self {
        Self {
            codex: AppServerRuntime::new_local(config.clone()),
            kimi: KimiRuntime::new(config.clone()),
            pi: PiRuntime::new(config.clone()),
            config,
        }
    }

    pub async fn statuses(&self) -> Vec<AgentProviderStatus> {
        let codex_was_running = self.codex.is_running().await;
        let codex_installed = crate::probe::command_available(&self.config.codex_command);
        let codex_version = if codex_was_running || !codex_installed {
            None
        } else {
            crate::probe::command_version(&self.config.codex_command, &["--version"]).await
        };
        let codex_available = codex_was_running || codex_installed;
        let codex_models = if codex_was_running {
            match self.codex_model_catalog().await {
                Ok(models) if !models.is_empty() => models,
                Ok(_) => fallback_codex_models(),
                Err(error) => {
                    tracing::warn!(error=?error, "Codex model catalog unavailable; using fallback");
                    fallback_codex_models()
                }
            }
        } else if codex_available {
            fallback_codex_models()
        } else {
            Vec::new()
        };
        let codex_running = self.codex.is_running().await;
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
            models: codex_models,
        };
        vec![codex, self.kimi.provider_status().await, self.pi.provider_status().await]
    }

    async fn codex_model_catalog(&self) -> Result<Vec<AgentModelOption>> {
        let catalog = self
            .codex
            .call_with_timeout(
                "model/list",
                json!({"limit":100,"includeHidden":false}),
                Duration::from_secs(10),
            )
            .await?;
        let config = self
            .codex
            .call_with_timeout(
                "config/read",
                json!({"includeLayers":false}),
                Duration::from_secs(5),
            )
            .await
            .unwrap_or(Value::Null);
        Ok(parse_codex_models(&catalog, &config))
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
                let result = self
                    .kimi
                    .create_session(cwd, title, access_mode, &options)
                    .await?;
                let id = extract_id(&result, &["id"])
                    .context("Kimi create-session response has no session id")?;
                self.kimi.subscribe_session(&id).await;
                Ok(id)
            }
            AgentProvider::Pi => {
                let result = self
                    .pi
                    .create_session(cwd, title, access_mode, &options)
                    .await?;
                extract_id(&result, &["id"]).context("Pi create-session response has no session id")
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
            // Pi has no provider-side archive; Nuntius keeps the projection.
            AgentProvider::Pi => Ok(json!({"archived":archived,"scope":"nuntius"})),
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
                let busy = session.get("busy").and_then(Value::as_bool) == Some(true);
                let main_turn_active = session
                    .get("main_turn_active")
                    .and_then(Value::as_bool)
                    .unwrap_or(busy);
                Ok(AgentThreadState {
                    status: if main_turn_active {
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
            AgentProvider::Pi => self.pi.thread_state(session_id).await,
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
            AgentProvider::Pi => self.pi.submit_prompt(session_id, input, options).await,
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
            AgentProvider::Pi => self.pi.steer_prompt(session_id, input, options).await,
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
            AgentProvider::Pi => self.pi.interrupt(session_id).await,
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
            AgentProvider::Pi => self.pi.read_thread(session_id).await,
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
            AgentProvider::Pi => self.pi.list_sessions(cwd, archived).await,
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
                let session_id = session_id
                    .or_else(|| provider_request_id.get("sessionId").and_then(Value::as_str))
                    .context("Kimi approval has no session")?;
                let approval_id = provider_request_id
                    .as_str()
                    .or_else(|| {
                        provider_request_id
                            .get("approvalId")
                            .and_then(Value::as_str)
                    })
                    .context("Kimi approval id is not a string")?;
                self.kimi
                    .resolve_approval(session_id, approval_id, decision)
                    .await?;
                Ok(())
            }
            AgentProvider::Pi => {
                let session_id = provider_request_id
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .context("Pi approval has no session")?;
                let request_id = provider_request_id
                    .get("requestId")
                    .and_then(Value::as_str)
                    .context("Pi approval has no request id")?;
                let ui_method = provider_request_id
                    .get("uiMethod")
                    .and_then(Value::as_str)
                    .unwrap_or("confirm");
                self.pi
                    .resolve_ui(
                        session_id,
                        request_id,
                        ui_method,
                        decision,
                        response.as_ref(),
                    )
                    .await
            }
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.pi.shutdown().await;
        self.kimi.shutdown().await;
        self.codex.shutdown().await
    }

    pub async fn request_host_upgrade_if_idle(&self) -> Result<bool> {
        self.codex.request_host_upgrade_if_idle().await
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
    let main_turn_active = session
        .get("main_turn_active")
        .and_then(Value::as_bool)
        .unwrap_or(busy);
    let active_group = main_turn_active.then(|| {
        active_prompt
            .map(str::to_owned)
            .or_else(|| groups.last().map(|group| group.0.clone()))
            .unwrap_or_else(|| "kimi-in-flight".into())
    });
    let turns = groups
        .into_iter()
        .map(|(id, items, started)| {
            let active = active_group.as_deref() == Some(id.as_str());
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
        } else if content.get("type").and_then(Value::as_str) == Some("tool_use") {
            let name = content
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let input = content
                .get("input")
                .map(Value::to_string)
                .unwrap_or_default();
            text.push(format!("{name} {input}"));
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

fn parse_codex_models(catalog: &Value, config: &Value) -> Vec<AgentModelOption> {
    let items = catalog
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let configured_model = config.pointer("/config/model").and_then(Value::as_str);
    let configured_effort = config
        .pointer("/config/model_reasoning_effort")
        .and_then(Value::as_str);
    let configured_model_is_listed = configured_model.is_some_and(|configured| {
        items.iter().any(|item| {
            item.get("model").and_then(Value::as_str) == Some(configured)
                || item.get("id").and_then(Value::as_str) == Some(configured)
        })
    });

    items
        .iter()
        .filter(|item| item.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|item| {
            let id = item
                .get("model")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))?
                .to_owned();
            let is_default = if configured_model_is_listed {
                configured_model == Some(id.as_str())
            } else {
                item.get("isDefault").and_then(Value::as_bool) == Some(true)
            };
            let default_reasoning_effort = if is_default {
                configured_effort
                    .or_else(|| item.get("defaultReasoningEffort").and_then(Value::as_str))
                    .map(str::to_owned)
            } else {
                item.get("defaultReasoningEffort")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let reasoning_efforts = item
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|effort| {
                    effort
                        .get("reasoningEffort")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect();
            Some(AgentModelOption {
                id,
                label: item
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex")
                    .to_owned(),
                description: item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                is_default,
                default_reasoning_effort,
                reasoning_efforts,
            })
        })
        .collect()
}

fn fallback_codex_models() -> Vec<AgentModelOption> {
    vec![AgentModelOption {
        id: "gpt-5.6-sol".into(),
        label: "GPT-5.6 Sol".into(),
        description: Some("OpenAI 当前旗舰编码与复杂推理模型".into()),
        is_default: true,
        default_reasoning_effort: Some("xhigh".into()),
        reasoning_efforts: ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_catalog_prefers_effective_config() {
        let catalog = json!({"data":[
            {
                "id":"gpt-5.6-sol",
                "model":"gpt-5.6-sol",
                "displayName":"GPT-5.6-Sol",
                "description":"Frontier",
                "hidden":false,
                "isDefault":true,
                "defaultReasoningEffort":"low",
                "supportedReasoningEfforts":[
                    {"reasoningEffort":"low"},
                    {"reasoningEffort":"xhigh"}
                ]
            }
        ]});
        let models = parse_codex_models(
            &catalog,
            &json!({"config":{"model":"gpt-5.6-sol","model_reasoning_effort":"xhigh"}}),
        );
        assert_eq!(models.len(), 1);
        assert!(models[0].is_default);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(
            models[0].reasoning_efforts,
            vec!["low".to_owned(), "xhigh".to_owned()]
        );
    }
}
