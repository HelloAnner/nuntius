use crate::{
    agent::{AgentRuntimes, AgentThreadState},
    app_server::AppServerCallError,
    config::ClientConfig,
    directory,
    protocol::*,
    store::ClientStore,
};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, broadcast};

#[derive(Clone)]
pub struct CommandExecutor {
    pub config: Arc<ClientConfig>,
    pub store: ClientStore,
    pub agents: AgentRuntimes,
    pub device_id: String,
    pub events: broadcast::Sender<NuntiusEvent>,
    pub command_acks: broadcast::Sender<TunnelFrame>,
    pub command_notify: Arc<Notify>,
    pub history_import_lock: Arc<Mutex<()>>,
}

impl CommandExecutor {
    pub async fn execute(&self, command: &DeviceCommand) -> Result<Value> {
        if command.device_id != self.device_id {
            bail!("command targets a different device")
        }
        validate_command(&command.command)?;
        match &command.command {
            DeviceCommandKind::Refresh => {
                self.discover_all().await?;
                self.emit_inventory().await?;
                Ok(json!({"refreshed":true}))
            }
            DeviceCommandKind::ProjectCreate(request) => self.create_project(request).await,
            DeviceCommandKind::ProjectDelete { project_id } => {
                self.delete_project(project_id).await
            }
            DeviceCommandKind::ThreadCreate {
                project_id,
                request,
            } => self.create_thread(project_id, request).await,
            DeviceCommandKind::ThreadArchive {
                thread_id,
                archived,
            } => {
                let thread = self.command_thread(thread_id).await?;
                let provider_result = if thread.archived == *archived {
                    json!({"alreadyInRequestedState":true})
                } else {
                    let result = self
                        .agents
                        .archive_session(
                            thread.provider,
                            thread
                                .app_server_thread_id
                                .as_deref()
                                .context("thread has no provider session id")?,
                            *archived,
                        )
                        .await?;
                    self.store.set_thread_archived(thread_id, *archived).await?;
                    result
                };
                let updated = self.thread(thread_id).await?;
                if let Err(error) = self.sync_thread(thread_id).await {
                    // The archive side effect and local SQLite state are already durable.
                    // A transient history-outbox failure must not report the idempotent
                    // archive operation itself as failed.
                    tracing::warn!(%thread_id,error=?error,"archived thread history sync deferred");
                }
                Ok(json!({
                    "threadId": thread_id,
                    "archived": archived,
                    "thread": updated,
                    "providerResult": provider_result,
                }))
            }
            DeviceCommandKind::TurnStart { thread_id, request } => {
                self.start_turn(thread_id, request).await
            }
            DeviceCommandKind::TurnSteer { thread_id, request } => {
                let thread = self.command_thread(thread_id).await?;
                let state = self.resume_provider_thread(&thread).await?;
                let input = text_input(&request.text)?;
                let result = self
                    .agents
                    .steer_turn(
                        thread.provider,
                        thread
                            .app_server_thread_id
                            .as_deref()
                            .context("thread has no provider session id")?,
                        state.active_turn_id.as_deref(),
                        &input,
                        self.store.thread_access_mode(thread_id).await?,
                        &json!({}),
                        None,
                    )
                    .await?;
                self.emit(
                    "turn.steered",
                    Some(&thread.project_id),
                    Some(thread_id),
                    None,
                    json!({"text":request.text,"providerResult":result}),
                    true,
                )
                .await?;
                Ok(result)
            }
            DeviceCommandKind::TurnInterrupt { thread_id } => {
                let thread = self.command_thread(thread_id).await?;
                let state = self.resume_provider_thread(&thread).await?;
                let provider_turn = state.active_turn_id;
                if provider_turn.is_none()
                    && !(thread.provider == AgentProvider::Kimi && state.status == "active")
                {
                    if state.status == "active" {
                        bail!("active provider turn identity is unavailable")
                    }
                    self.store.touch_thread(thread_id, "idle").await?;
                    self.sync_thread(thread_id).await?;
                    self.emit_thread_summary(thread_id).await?;
                    return Ok(json!({"alreadyTerminal":true}));
                }
                let result = self
                    .agents
                    .interrupt(
                        thread.provider,
                        thread
                            .app_server_thread_id
                            .as_deref()
                            .context("thread has no provider session id")?,
                        provider_turn.as_deref(),
                    )
                    .await?;
                self.store.touch_thread(thread_id, "interrupted").await?;
                self.sync_thread(thread_id).await?;
                self.emit_thread_summary(thread_id).await?;
                Ok(result)
            }
            DeviceCommandKind::ApprovalDecide {
                approval_id,
                request,
            } => {
                if request.response.is_none()
                    && !matches!(
                        request.decision.as_str(),
                        "accept" | "accept_for_session" | "decline" | "cancel"
                    )
                {
                    bail!("unsupported approval decision")
                };
                let (provider, request_id, _method) = self
                    .store
                    .claim_provider_request(approval_id)
                    .await?
                    .context("approval is missing or already decided")?;
                if let Err(error) = self
                    .agents
                    .resolve_approval(
                        provider,
                        None,
                        request_id,
                        &request.decision,
                        request.response.clone(),
                    )
                    .await
                {
                    self.store
                        .finish_app_request(approval_id, "unknown")
                        .await?;
                    return Err(error).context("approval outcome is unknown");
                }
                self.store
                    .finish_app_request(approval_id, "decided")
                    .await?;
                Ok(json!({"approvalId":approval_id,"decision":request.decision}))
            }
            DeviceCommandKind::HistorySync { thread_id } => {
                if let Some(id) = thread_id {
                    self.store
                        .state_set(&format!("history_hash:{id}"), "")
                        .await?;
                    self.refresh_thread_history(id).await?;
                } else {
                    for thread in self.store.list_threads(&self.device_id, None).await? {
                        self.store
                            .state_set(&format!("history_hash:{}", thread.id), "")
                            .await?;
                        self.refresh_thread_history(&thread.id).await?;
                    }
                }
                Ok(json!({"queued":true}))
            }
        }
    }

    async fn create_project(&self, request: &CreateProjectRequest) -> Result<Value> {
        let path = directory::resolve(&self.config, &self.store, &request.directory_ref).await?;
        if self.store.project_by_path(&path).await?.is_some() {
            bail!("directory already belongs to a project")
        };
        let id = new_id("prj");
        self.store
            .create_project(&id, request.display_name.trim(), &path, &request.defaults)
            .await?;
        let project = self
            .store
            .project(&id, &self.device_id)
            .await?
            .context("created project missing")?
            .summary;
        self.emit(
            "project.summary",
            Some(&id),
            None,
            None,
            serde_json::to_value(&project)?,
            true,
        )
        .await?;
        let executor = self.clone();
        let project_id = id.clone();
        tokio::spawn(async move {
            if let Err(error) = executor.discover_project(&project_id).await {
                tracing::warn!(%project_id,error=?error,"initial project history discovery failed");
            }
        });
        Ok(serde_json::to_value(project)?)
    }

    async fn delete_project(&self, project_id: &str) -> Result<Value> {
        let removal = self
            .store
            .remove_project(project_id)
            .await?
            .context("project not found")?;
        self.emit(
            "project.removed",
            Some(project_id),
            None,
            None,
            json!({
                "projectId": project_id,
                "threadCount": removal.thread_count,
                "alreadyRemoved": removal.already_removed,
            }),
            true,
        )
        .await?;
        Ok(json!({
            "projectId": project_id,
            "threadCount": removal.thread_count,
            "alreadyRemoved": removal.already_removed,
        }))
    }

    async fn create_thread(
        &self,
        project_id: &str,
        request: &CreateThreadRequest,
    ) -> Result<Value> {
        let title = request
            .title
            .clone()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| {
                request
                    .first_message
                    .as_deref()
                    .map(derive_title)
                    .unwrap_or_else(|| "新对话".into())
            });
        let provider_session_id = self
            .start_provider_thread(
                request.provider,
                project_id,
                &title,
                request.access_mode,
                request.options.clone(),
            )
            .await?;
        let id = new_id("thr");
        self.store
            .create_provider_thread(
                &id,
                project_id,
                request.provider,
                &provider_session_id,
                &title,
                request.access_mode,
                &request.options,
            )
            .await?;
        self.sync_thread(&id).await?;
        if let Some(text) = request
            .first_message
            .as_ref()
            .filter(|v| !v.trim().is_empty())
        {
            let start = StartTurnRequest {
                text: text.clone(),
                access_mode: request.access_mode,
                options: Value::Object(Map::new()),
            };
            let _ = self.start_turn(&id, &start).await?;
        }
        let thread = self.thread(&id).await?;
        Ok(json!({
            "threadId": id,
            "appServerThreadId": thread.app_server_thread_id,
            "provider": thread.provider,
            "thread": thread
        }))
    }

    async fn start_provider_thread(
        &self,
        provider: AgentProvider,
        project_id: &str,
        title: &str,
        access_mode: ConversationAccessMode,
        options: Value,
    ) -> Result<String> {
        let project = self
            .store
            .project(project_id, &self.device_id)
            .await?
            .context("project not found")?;
        let mut params = object(project.defaults.clone());
        params.extend(object(options));
        self.agents
            .create_session(
                provider,
                &project.canonical_path,
                title,
                access_mode,
                Value::Object(params),
            )
            .await
    }

    async fn start_turn(&self, thread_id: &str, request: &StartTurnRequest) -> Result<Value> {
        let input = text_input(&request.text)?;
        let mut thread = self.command_thread(thread_id).await?;
        // A new App Server thread has no rollout until its first turn starts.
        // Calling thread/resume here is therefore invalid; follow the protocol's
        // thread/start -> turn/start lifecycle directly.
        if !self.store.thread_has_turns(thread_id).await? {
            return match self.begin_turn(&thread, request, &input).await {
                Ok(result) => Ok(result),
                Err(error)
                    if thread.provider == AgentProvider::Codex && is_missing_app_thread(&error) =>
                {
                    thread = self.recreate_empty_app_thread(&thread).await?;
                    self.begin_turn(&thread, request, &input).await
                }
                Err(error) => Err(error),
            };
        }
        let state = self.resume_provider_thread(&thread).await?;
        if state.active_turn_id.is_some()
            || (thread.provider == AgentProvider::Kimi && state.status == "active")
        {
            let result = self
                .agents
                .steer_turn(
                    thread.provider,
                    thread
                        .app_server_thread_id
                        .as_deref()
                        .context("thread has no provider session id")?,
                    state.active_turn_id.as_deref(),
                    &input,
                    request.access_mode,
                    &request.options,
                    None,
                )
                .await?;
            self.emit(
                "turn.steered",
                Some(&thread.project_id),
                Some(thread_id),
                None,
                json!({"text":request.text,"providerResult":result}),
                true,
            )
            .await?;
            return Ok(
                json!({"operation":"steer","providerTurnId":state.active_turn_id,"providerResult":result}),
            );
        }
        if state.status == "active" {
            bail!("active provider turn identity is unavailable")
        }
        if state.status == "systemError" || state.status == "notLoaded" {
            bail!(
                "provider thread cannot accept input while status is {}",
                state.status
            )
        }
        self.begin_turn(&thread, request, &input).await
    }

    async fn begin_turn(
        &self,
        thread: &ThreadSummary,
        request: &StartTurnRequest,
        input: &[Value],
    ) -> Result<Value> {
        let result = self
            .agents
            .start_turn(
                thread.provider,
                thread
                    .app_server_thread_id
                    .as_deref()
                    .context("thread has no provider session id")?,
                input,
                request.access_mode,
                &request.options,
                None,
            )
            .await?;
        let provider_turn = extract_id(
            &result,
            &["turn/id", "turnId", "id", "prompt_id", "prompt/prompt_id"],
        );
        let local_turn = self
            .store
            .record_user_turn(&thread.id, provider_turn.as_deref(), &request.text)
            .await?;
        self.sync_thread(&thread.id).await?;
        self.emit_thread_summary(&thread.id).await?;
        self.emit(
            "turn.started",
            Some(&thread.project_id),
            Some(&thread.id),
            Some(&local_turn),
            json!({"text":request.text,"providerResult":result}),
            true,
        )
        .await?;
        Ok(json!({"operation":"start","turnId":local_turn,"providerResult":result}))
    }

    async fn recreate_empty_app_thread(&self, thread: &ThreadSummary) -> Result<ThreadSummary> {
        if self.store.thread_has_turns(&thread.id).await? {
            bail!("refusing to replace an App Server thread that already has local history")
        }
        if thread.provider != AgentProvider::Codex {
            bail!("only an empty Codex thread can be recreated")
        }
        let options = self.store.app_server_options(&thread.id).await?;
        let access_mode = self.store.thread_access_mode(&thread.id).await?;
        let app_id = self
            .start_provider_thread(
                AgentProvider::Codex,
                &thread.project_id,
                &thread.title,
                access_mode,
                options,
            )
            .await?;
        self.store
            .rebind_app_server_thread(&thread.id, &app_id)
            .await?;
        tracing::info!(
            thread_id = %thread.id,
            previous_app_thread_id = ?thread.app_server_thread_id,
            app_thread_id = %app_id,
            "recreated empty App Server thread after missing rollout"
        );
        self.command_thread(&thread.id).await
    }

    async fn resume_provider_thread(&self, thread: &ThreadSummary) -> Result<AgentThreadState> {
        let app_thread_id = thread
            .app_server_thread_id
            .as_deref()
            .context("thread has no provider session id")?;
        let mut state = self
            .agents
            .thread_state(thread.provider, app_thread_id)
            .await?;
        if thread.provider == AgentProvider::Codex
            && state.active_turn_id.is_none()
            && state.status == "active"
        {
            state.active_turn_id = self.store.active_app_turn_id(&thread.id).await?;
        }
        Ok(state)
    }

    async fn thread(&self, id: &str) -> Result<ThreadSummary> {
        self.store
            .thread(id, &self.device_id)
            .await?
            .context("thread not found")
    }
    async fn emit_thread_summary(&self, thread_id: &str) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        self.emit(
            "thread.summary",
            Some(&thread.project_id),
            Some(&thread.id),
            None,
            serde_json::to_value(&thread)?,
            true,
        )
        .await?;
        Ok(())
    }
    async fn command_thread(&self, id: &str) -> Result<ThreadSummary> {
        self.store
            .controllable_thread(id, &self.device_id)
            .await?
            .context("thread is not attached to an active workspace project")
    }
    pub async fn sync_thread(&self, thread_id: &str) -> Result<()> {
        let records = self
            .store
            .history_records(thread_id, &self.device_id)
            .await?;
        if records.is_empty() {
            return Ok(());
        };
        let encoded = serde_json::to_vec(&records)?;
        let payload_hash = hex::encode(Sha256::digest(&encoded));
        let hash_key = format!("history_hash:{thread_id}");
        if self.store.state_get(&hash_key).await?.as_deref() == Some(&payload_hash) {
            return Ok(());
        }
        let mut chunks: Vec<Vec<HistoryRecord>> = Vec::new();
        let mut current = Vec::new();
        for record in records {
            if !current.is_empty() {
                let mut candidate = current.clone();
                candidate.push(record.clone());
                if current.len() >= 200 || serde_json::to_vec(&candidate)?.len() > 512 * 1024 {
                    chunks.push(std::mem::take(&mut current));
                }
            }
            current.push(record);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        let revision = self.store.next_history_revision(thread_id).await?;
        let chunk_count = chunks.len();
        let mut previous_cursor = None;
        for (index, records) in chunks.into_iter().enumerate() {
            let cursor = new_id("hist");
            let chunk_hash = hex::encode(Sha256::digest(serde_json::to_vec(&records)?));
            let batch = HistoryBatch {
                batch_id: new_id("hbatch"),
                device_id: self.device_id.clone(),
                thread_id: thread_id.into(),
                from_cursor: previous_cursor,
                to_cursor: cursor.clone(),
                inventory_revision: revision,
                payload_hash: chunk_hash,
                complete: index + 1 == chunk_count,
                records,
            };
            self.store.enqueue_history(&batch).await?;
            previous_cursor = Some(cursor);
        }
        self.store.state_set(&hash_key, &payload_hash).await?;
        Ok(())
    }
    pub async fn refresh_thread_history(&self, thread_id: &str) -> Result<()> {
        let _guard = self.history_import_lock.lock().await;
        let thread = self.thread(thread_id).await?;
        let app_thread_id = thread
            .app_server_thread_id
            .as_deref()
            .context("thread has no provider session id")?;
        let provider_thread = self
            .agents
            .read_thread(thread.provider, app_thread_id)
            .await?;
        self.store
            .import_app_history(thread_id, &provider_thread)
            .await?;
        self.sync_thread(thread_id).await?;
        self.store
            .state_set(
                &thread_fingerprint_key(thread.provider, app_thread_id),
                &thread_fingerprint(&provider_thread)?,
            )
            .await
    }

    /// Reconcile the most recently changed Codex sessions, including sessions
    /// created by a different CLI/App Server process on this workstation.
    pub async fn reconcile_recent(&self, archived: bool) -> Result<usize> {
        let response = self
            .agents
            .codex
            .call(
                "thread/list",
                json!({
                    "limit": 100,
                    "archived": archived,
                    "cursor": null,
                    "sortKey": "updated_at",
                    "sortDirection": "desc",
                    "useStateDbOnly": true,
                    "sourceKinds": [
                        "cli", "vscode", "exec", "appServer", "subAgent",
                        "subAgentReview", "subAgentCompact", "subAgentThreadSpawn",
                        "subAgentOther", "unknown"
                    ]
                }),
            )
            .await?;
        let mut refreshed = 0;
        for app_thread in response
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(app_id) = app_thread.get("id").and_then(Value::as_str) else {
                continue;
            };
            if self
                .provider_thread_is_removed(AgentProvider::Codex, app_thread)
                .await?
            {
                continue;
            }
            let fingerprint = thread_fingerprint(app_thread)?;
            let key = thread_fingerprint_key(AgentProvider::Codex, app_id);
            let missing = self
                .store
                .local_provider_thread_id(AgentProvider::Codex, app_id)
                .await?
                .is_none();
            let active = app_thread_status(app_thread) == "active";
            if !missing
                && !active
                && self.store.state_get(&key).await?.as_deref() == Some(&fingerprint)
            {
                continue;
            }
            match self.reconcile_app_thread(app_id).await {
                Ok(()) => refreshed += 1,
                Err(error) => {
                    tracing::warn!(%app_id,error=?error,"recent Codex thread reconciliation failed")
                }
            }
        }
        Ok(refreshed)
    }

    /// Reconcile Kimi sessions created or changed outside Nuntius. Kimi's web
    /// service owns the durable session index, so there is no rollout-file
    /// watcher equivalent to Codex's.
    pub async fn reconcile_kimi_recent(&self, archived: bool) -> Result<usize> {
        let threads = self
            .agents
            .list_threads(AgentProvider::Kimi, None, archived)
            .await?;
        let mut refreshed = 0;
        for provider_thread in threads {
            let Some(session_id) = provider_thread.get("id").and_then(Value::as_str) else {
                continue;
            };
            if self
                .provider_thread_is_removed(AgentProvider::Kimi, &provider_thread)
                .await?
            {
                continue;
            }
            let fingerprint = thread_fingerprint(&provider_thread)?;
            let key = thread_fingerprint_key(AgentProvider::Kimi, session_id);
            let local_thread_id = self
                .store
                .local_provider_thread_id(AgentProvider::Kimi, session_id)
                .await?;
            let active = app_thread_status(&provider_thread) == "active";
            if local_thread_id.is_some()
                && !active
                && self.store.state_get(&key).await?.as_deref() == Some(&fingerprint)
            {
                continue;
            }
            let project_id = if let Some(local_thread_id) = local_thread_id.as_deref() {
                self.thread(local_thread_id).await?.project_id
            } else {
                self.project_for_app_thread(None, &provider_thread).await?
            };
            let local_thread_id = self
                .store
                .import_provider_thread(AgentProvider::Kimi, &project_id, &provider_thread)
                .await?;
            match self
                .import_and_sync_thread(AgentProvider::Kimi, &local_thread_id, &provider_thread)
                .await
            {
                Ok(()) => {
                    self.store.state_set(&key, &fingerprint).await?;
                    if let Some(project) = self.store.project(&project_id, &self.device_id).await? {
                        self.emit(
                            "project.summary",
                            Some(&project_id),
                            Some(&local_thread_id),
                            None,
                            serde_json::to_value(&project.summary)?,
                            true,
                        )
                        .await?;
                    }
                    self.emit_thread_summary(&local_thread_id).await?;
                    refreshed += 1;
                }
                Err(error) => {
                    tracing::warn!(%session_id,error=?error,"recent Kimi session reconciliation failed")
                }
            }
        }
        Ok(refreshed)
    }

    /// Force a single App Server thread into the local durable history outbox.
    /// Used by the rollout-file monitor so external terminal activity does not
    /// depend on this App Server instance receiving runtime notifications.
    pub async fn reconcile_app_thread(&self, app_id: &str) -> Result<()> {
        let _guard = self.history_import_lock.lock().await;
        let response = self
            .agents
            .codex
            .call_with_timeout(
                "thread/read",
                json!({"threadId":app_id,"includeTurns":true}),
                std::time::Duration::from_secs(180),
            )
            .await
            .with_context(|| format!("cannot read changed Codex thread {app_id}"))?;
        let app_thread = response.get("thread").unwrap_or(&response);
        if self
            .provider_thread_is_removed(AgentProvider::Codex, app_thread)
            .await?
        {
            return Ok(());
        }
        let project_id = if let Some(local_id) = self
            .store
            .local_provider_thread_id(AgentProvider::Codex, app_id)
            .await?
        {
            self.thread(&local_id).await?.project_id
        } else {
            self.project_for_app_thread(None, app_thread).await?
        };
        let local_thread = self
            .store
            .import_provider_thread(AgentProvider::Codex, &project_id, app_thread)
            .await?;
        self.store
            .import_app_history(&local_thread, app_thread)
            .await?;
        self.sync_thread(&local_thread).await?;
        self.store
            .state_set(
                &thread_fingerprint_key(AgentProvider::Codex, app_id),
                &thread_fingerprint(app_thread)?,
            )
            .await?;
        if let Some(project) = self.store.project(&project_id, &self.device_id).await? {
            self.emit(
                "project.summary",
                Some(&project_id),
                Some(&local_thread),
                None,
                serde_json::to_value(&project.summary)?,
                true,
            )
            .await?;
        }
        self.emit_thread_summary(&local_thread).await?;
        Ok(())
    }
    pub async fn discover_project(&self, project_id: &str) -> Result<usize> {
        let project = self
            .store
            .project(project_id, &self.device_id)
            .await?
            .context("project not found")?;
        let mut imported = 0;
        for provider in [AgentProvider::Codex, AgentProvider::Kimi] {
            match self
                .discover_pages(
                    provider,
                    Some(project_id),
                    Some(&project.canonical_path),
                    false,
                )
                .await
            {
                Ok(count) => imported += count,
                Err(error) => {
                    tracing::warn!(provider=provider.as_str(),error=?error,"provider project discovery unavailable")
                }
            }
        }
        Ok(imported)
    }
    pub async fn discover_all(&self) -> Result<usize> {
        self.store
            .state_set("history_discovery_complete", "false")
            .await?;
        self.store
            .state_set("history_completion_announced", "false")
            .await?;
        let mut imported = 0;
        for provider in [AgentProvider::Codex, AgentProvider::Kimi] {
            for archived in [false, true] {
                match self.discover_pages(provider, None, None, archived).await {
                    Ok(count) => imported += count,
                    Err(error) => {
                        tracing::warn!(provider=provider.as_str(),archived,error=?error,"provider history discovery unavailable")
                    }
                }
            }
        }
        self.store
            .state_set("history_discovery_complete", "true")
            .await?;
        self.maybe_emit_inventory_complete().await?;
        Ok(imported)
    }
    pub async fn maybe_emit_inventory_complete(&self) -> Result<()> {
        if self
            .store
            .state_get("history_discovery_complete")
            .await?
            .as_deref()
            != Some("true")
            || self
                .store
                .state_get("history_completion_announced")
                .await?
                .as_deref()
                == Some("true")
            || !self.store.pending_history(1).await?.is_empty()
        {
            return Ok(());
        }
        self.emit(
            "history.inventory_complete",
            None,
            None,
            None,
            json!({"completeness":"complete"}),
            true,
        )
        .await?;
        self.store
            .state_set("history_completion_announced", "true")
            .await?;
        Ok(())
    }
    async fn discover_pages(
        &self,
        provider: AgentProvider,
        fixed_project_id: Option<&str>,
        cwd: Option<&std::path::Path>,
        archived: bool,
    ) -> Result<usize> {
        let unassigned_project = self
            .store
            .ensure_unassigned_project(&self.device_id)
            .await?;
        let mut imported = 0_usize;
        let threads = self.agents.list_threads(provider, cwd, archived).await?;
        for app_thread in threads {
            if self
                .provider_thread_is_removed(provider, &app_thread)
                .await?
            {
                continue;
            }
            let project_id = self
                .project_for_app_thread_with_unassigned(
                    fixed_project_id,
                    &app_thread,
                    &unassigned_project,
                )
                .await?;
            let app_id = match app_thread.get("id").and_then(Value::as_str) {
                Some(id) => id,
                None => {
                    tracing::warn!("thread/list returned a thread without an id");
                    continue;
                }
            };
            let was_missing = self
                .store
                .local_provider_thread_id(provider, app_id)
                .await?
                .is_none();
            let local_thread = self
                .store
                .import_provider_thread(provider, &project_id, &app_thread)
                .await?;
            let fingerprint = thread_fingerprint(&app_thread)?;
            let key = thread_fingerprint_key(provider, app_id);
            let active = app_thread_status(&app_thread) == "active";
            if was_missing
                || active
                || self.store.state_get(&key).await?.as_deref() != Some(&fingerprint)
            {
                match self
                    .import_and_sync_thread(provider, &local_thread, &app_thread)
                    .await
                {
                    Ok(()) => {
                        self.store.state_set(&key, &fingerprint).await?;
                        imported += 1;
                    }
                    Err(error) => {
                        tracing::warn!(provider=provider.as_str(),%app_id,error=?error,"historical provider thread import failed; continuing discovery");
                    }
                }
            }
        }
        for project in self.store.list_projects(&self.device_id).await? {
            self.emit(
                "project.summary",
                Some(&project.id),
                None,
                None,
                serde_json::to_value(&project)?,
                true,
            )
            .await?;
        }
        Ok(imported)
    }

    async fn provider_thread_is_removed(
        &self,
        provider: AgentProvider,
        app_thread: &Value,
    ) -> Result<bool> {
        if let Some(app_id) = app_thread.get("id").and_then(Value::as_str)
            && self.store.provider_thread_removed(provider, app_id).await?
        {
            return Ok(true);
        }
        let Some(raw_cwd) = app_thread.get("cwd").and_then(Value::as_str) else {
            return Ok(false);
        };
        let Some(canonical) = directory::canonical_project_path(std::path::Path::new(raw_cwd)).ok()
        else {
            return Ok(false);
        };
        self.store.project_path_removed(&canonical).await
    }

    async fn project_for_app_thread(
        &self,
        fixed_project_id: Option<&str>,
        app_thread: &Value,
    ) -> Result<String> {
        let unassigned = self
            .store
            .ensure_unassigned_project(&self.device_id)
            .await?;
        self.project_for_app_thread_with_unassigned(fixed_project_id, app_thread, &unassigned)
            .await
    }

    async fn project_for_app_thread_with_unassigned(
        &self,
        fixed_project_id: Option<&str>,
        app_thread: &Value,
        unassigned_project: &str,
    ) -> Result<String> {
        if let Some(id) = fixed_project_id {
            return Ok(id.to_string());
        }
        let Some(raw_cwd) = app_thread.get("cwd").and_then(Value::as_str) else {
            return Ok(unassigned_project.to_string());
        };
        let raw_path = std::path::Path::new(raw_cwd);
        let canonical = directory::canonical_project_path(raw_path).ok();
        if let Some(id) = match canonical.as_deref() {
            Some(path) => self.store.project_by_path(path).await?,
            None => None,
        } {
            return Ok(id);
        }
        let Some(path) = directory::validate_project_path(&self.config, raw_path).ok() else {
            return Ok(unassigned_project.to_string());
        };
        let id = new_id("prj");
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "导入项目".into());
        self.store
            .create_project(&id, &name, &path, &json!({}))
            .await?;
        Ok(id)
    }

    async fn import_and_sync_thread(
        &self,
        provider: AgentProvider,
        local_thread: &str,
        app_thread: &Value,
    ) -> Result<()> {
        let _guard = self.history_import_lock.lock().await;
        let app_id = app_thread
            .get("id")
            .and_then(Value::as_str)
            .context("listed thread has no id")?;
        let detail = self
            .agents
            .read_thread(provider, app_id)
            .await
            .with_context(|| {
                format!(
                    "cannot read historical {} thread {app_id}",
                    provider.as_str()
                )
            })?;
        self.store.import_app_history(local_thread, &detail).await?;
        self.sync_thread(local_thread).await
    }
    pub async fn emit_inventory(&self) -> Result<()> {
        for project in self.store.list_projects(&self.device_id).await? {
            self.emit(
                "project.summary",
                Some(&project.id),
                None,
                None,
                serde_json::to_value(&project)?,
                true,
            )
            .await?;
        }
        for thread in self.store.list_threads(&self.device_id, None).await? {
            self.sync_thread(&thread.id).await?;
        }
        Ok(())
    }
    pub async fn emit(
        &self,
        event_type: &str,
        project_id: Option<&str>,
        thread_id: Option<&str>,
        turn_id: Option<&str>,
        payload: Value,
        durable: bool,
    ) -> Result<NuntiusEvent> {
        let stream = thread_id
            .map(|id| format!("thread:{id}"))
            .unwrap_or_else(|| format!("device:{}", self.device_id));
        let seq = self.store.next_stream_sequence(&stream).await?;
        let event = NuntiusEvent {
            event_id: new_id("evt"),
            user_id: None,
            device_id: self.device_id.clone(),
            project_id: project_id.map(str::to_owned),
            thread_id: thread_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
            stream_id: stream,
            seq,
            event_type: event_type.into(),
            durability: if durable {
                "durable".into()
            } else {
                "transient".into()
            },
            occurred_at: now(),
            payload,
        };
        if durable {
            self.store.enqueue_event(&event).await?;
        }
        let _ = self.events.send(event.clone());
        Ok(event)
    }
}

pub async fn process_app_events(executor: CommandExecutor) {
    let mut receiver = executor.agents.codex.subscribe();
    loop {
        match receiver.recv().await {
            Ok(message) => {
                if let Err(error) = process_app_event(&executor, message).await {
                    tracing::warn!(error=?error,"failed to process App Server event")
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped,
                    "App Server event processor lagged; reconciling history"
                );
                if let Err(error) = executor.discover_all().await {
                    tracing::warn!(error=?error, "history reconciliation after event loss failed");
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn process_app_event(executor: &CommandExecutor, message: Value) -> Result<()> {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("app_server/message");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let event_params = bounded_event_payload(&params, 256 * 1024);
    let app_thread = find_string(&params, &["threadId", "thread/id"]);
    let thread_id = if let Some(id) = app_thread.as_deref() {
        executor
            .store
            .local_provider_thread_id(AgentProvider::Codex, id)
            .await?
    } else {
        None
    };
    let project_id = if let Some(id) = thread_id.as_deref() {
        executor
            .store
            .thread(id, &executor.device_id)
            .await?
            .map(|t| t.project_id)
    } else {
        None
    };
    if let Some(request_id) = message
        .get("id")
        .filter(|_| message.get("method").is_some())
    {
        let approval_id = new_id("apr");
        executor
            .store
            .save_provider_request(
                AgentProvider::Codex,
                &approval_id,
                request_id,
                method,
                &event_params,
            )
            .await?;
        executor
            .emit(
                "approval.requested",
                project_id.as_deref(),
                thread_id.as_deref(),
                None,
                json!({"approvalId":approval_id,"method":method,"params":event_params}),
                true,
            )
            .await?;
        return Ok(());
    }
    if let Some(local_thread) = thread_id.as_deref() {
        if method == "turn/started" {
            let app_turn = find_string(&params, &["turnId", "turn/id"]);
            executor
                .store
                .mark_app_turn_started(local_thread, app_turn.as_deref())
                .await?;
            executor.sync_thread(local_thread).await?;
            executor.emit_thread_summary(local_thread).await?;
        } else if method == "turn/completed"
            || method == "turn/failed"
            || method == "turn/error"
            || method.starts_with("turn/interrupt")
        {
            let app_turn = find_string(&params, &["turnId", "turn/id"]);
            let status = if method == "turn/completed" {
                find_string(&params, &["turn/status", "status"])
                    .unwrap_or_else(|| "completed".into())
            } else if method.starts_with("turn/interrupt") {
                "interrupted".into()
            } else {
                "failed".into()
            };
            executor
                .store
                .complete_app_turn(local_thread, app_turn.as_deref(), &status)
                .await?;
            executor.sync_thread(local_thread).await?;
            executor.emit_thread_summary(local_thread).await?;
        } else if method == "thread/status/changed" {
            let status = find_string(&params, &["status/type", "status"])
                .unwrap_or_else(|| "unknown".into());
            if status == "active" {
                executor.store.touch_thread(local_thread, "active").await?;
            } else {
                let terminal = if status == "idle" {
                    "completed"
                } else {
                    "failed"
                };
                executor
                    .store
                    .complete_app_turn(local_thread, None, terminal)
                    .await?;
                if status != "idle" {
                    executor.store.touch_thread(local_thread, &status).await?;
                }
            }
            executor.sync_thread(local_thread).await?;
            executor.emit_thread_summary(local_thread).await?;
        }
    }
    let durable = !method.ends_with("/delta");
    let event_type = format!("app_server.{}", method.replace('/', "."));
    executor
        .emit(
            &event_type,
            project_id.as_deref(),
            thread_id.as_deref(),
            None,
            event_params,
            durable,
        )
        .await?;
    if method == "item/completed"
        && let Some(local_thread) = thread_id.as_deref()
    {
        let item = params.get("item").unwrap_or(&params);
        let kind = find_string(item, &["type", "kind"]).unwrap_or_default();
        if kind.to_ascii_lowercase().contains("agent")
            && let Some(text) = extract_text(item)
        {
            let app_turn = find_string(&params, &["turnId", "turn/id"]);
            let app_item = find_string(item, &["id"]);
            executor
                .store
                .record_agent_message(
                    local_thread,
                    app_turn.as_deref(),
                    app_item.as_deref(),
                    &text,
                    item,
                )
                .await?;
            executor.sync_thread(local_thread).await?;
        }
    }
    if (method == "turn/completed"
        || method == "turn/failed"
        || method == "turn/error"
        || method.starts_with("turn/interrupt"))
        && let Some(local_thread) = thread_id.as_deref()
    {
        executor.refresh_thread_history(local_thread).await?;
        executor.emit_thread_summary(local_thread).await?;
    }
    Ok(())
}

pub async fn process_kimi_events(executor: CommandExecutor) {
    let mut receiver = executor.agents.kimi.subscribe();
    loop {
        match receiver.recv().await {
            Ok(message) => {
                if let Err(error) = process_kimi_event(&executor, message).await {
                    tracing::warn!(error=?error, "failed to process Kimi event")
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "Kimi event processor lagged; reconciling history");
                if let Err(error) = executor.discover_all().await {
                    tracing::warn!(error=?error, "history reconciliation after Kimi event loss failed");
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn process_kimi_event(executor: &CommandExecutor, message: Value) -> Result<()> {
    let event_type = message
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let session_id = message
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .pointer("/payload/sessionId")
                .and_then(Value::as_str)
        })
        .context("Kimi event has no session id")?;
    let Some(thread_id) = executor
        .store
        .local_provider_thread_id(AgentProvider::Kimi, session_id)
        .await?
    else {
        return Ok(());
    };
    let thread = executor.thread(&thread_id).await?;
    let payload = message.get("payload").cloned().unwrap_or(Value::Null);
    let turn_id = kimi_event_id(&payload, "turnId");

    if event_type == "nuntius.resync_required" {
        executor.refresh_thread_history(&thread_id).await?;
        executor.emit_thread_summary(&thread_id).await?;
        return Ok(());
    }

    if event_type == "event.approval.requested" {
        let provider_approval_id = payload
            .get("approval_id")
            .and_then(Value::as_str)
            .context("Kimi approval event has no approval_id")?;
        let approval_id = format!("apr_kimi_{provider_approval_id}");
        executor
            .store
            .save_provider_request(
                AgentProvider::Kimi,
                &approval_id,
                &json!({"approvalId":provider_approval_id,"sessionId":session_id}),
                "kimi/approval",
                &payload,
            )
            .await?;
        executor
            .emit(
                "approval.requested",
                Some(&thread.project_id),
                Some(&thread_id),
                turn_id.as_deref(),
                json!({"approvalId":approval_id,"method":"kimi/approval","params":payload}),
                true,
            )
            .await?;
        return Ok(());
    }

    match event_type {
        "turn.started" => {
            executor
                .store
                .mark_app_turn_started(&thread_id, None)
                .await?;
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
        }
        "turn.ended" => {
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let status = match reason {
                "cancelled" => "interrupted",
                "failed" | "blocked" => "failed",
                _ => "completed",
            };
            executor
                .store
                .complete_app_turn(&thread_id, None, status)
                .await?;
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
        }
        "event.session.work_changed" => {
            if payload.get("busy").and_then(Value::as_bool) == Some(true) {
                executor.store.touch_thread(&thread_id, "active").await?;
            } else {
                executor
                    .store
                    .complete_app_turn(&thread_id, None, "completed")
                    .await?;
            }
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
        }
        _ => {}
    }

    let durable = message.get("volatile").and_then(Value::as_bool) != Some(true);
    executor
        .emit(
            &format!("agent.{event_type}"),
            Some(&thread.project_id),
            Some(&thread_id),
            turn_id.as_deref(),
            payload,
            durable,
        )
        .await?;

    if event_type == "turn.ended" {
        executor.refresh_thread_history(&thread_id).await?;
        executor.emit_thread_summary(&thread_id).await?;
    }
    Ok(())
}

fn kimi_event_id(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_i64().map(|value| value.to_string()))
            .or_else(|| value.as_u64().map(|value| value.to_string()))
    })
}

fn text_input(text: &str) -> Result<Vec<Value>> {
    if text.trim().is_empty() {
        bail!("turn text cannot be empty")
    }
    Ok(vec![json!({"type":"text","text":text})])
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
fn is_missing_app_thread(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<AppServerCallError>()
        .is_some_and(AppServerCallError::is_missing_thread)
}
fn derive_title(text: &str) -> String {
    text.chars().take(40).collect()
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
fn thread_fingerprint_key(provider: AgentProvider, app_thread_id: &str) -> String {
    format!(
        "provider_thread_fingerprint:{}:{app_thread_id}",
        provider.as_str()
    )
}
fn thread_fingerprint(thread: &Value) -> Result<String> {
    let identity = json!({
        "id": thread.get("id"),
        "updatedAt": thread.get("updatedAt"),
        "recencyAt": thread.get("recencyAt"),
        "status": thread.get("status"),
        "name": thread.get("name"),
        "preview": thread.get("preview"),
        "cwd": thread.get("cwd"),
        "archived": thread.get("archived"),
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&identity)?)))
}
fn extract_id(value: &Value, paths: &[&str]) -> Option<String> {
    find_string(value, paths)
}
fn find_string(value: &Value, paths: &[&str]) -> Option<String> {
    for path in paths {
        let mut current = value;
        let mut found = true;
        for key in path.split('/') {
            match current.get(key) {
                Some(next) => current = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found && let Some(value) = current.as_str() {
            return Some(value.into());
        }
    }
    None
}
fn extract_text(item: &Value) -> Option<String> {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return Some(text.into());
    }
    if let Some(content) = item.get("content").and_then(Value::as_array) {
        let parts = content
            .iter()
            .filter_map(|v| v.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

fn bounded_event_payload(value: &Value, limit: usize) -> Value {
    match serde_json::to_vec(value) {
        Ok(encoded) if encoded.len() <= limit => value.clone(),
        Ok(encoded) => json!({"truncated":true,"originalBytes":encoded.len()}),
        Err(_) => json!({"truncated":true}),
    }
}

fn validate_command(kind: &DeviceCommandKind) -> Result<()> {
    fn text(field: &str, value: &str, maximum: usize) -> Result<()> {
        if value.trim().is_empty() || value.len() > maximum {
            bail!("{field} must contain 1 to {maximum} bytes")
        }
        Ok(())
    }
    fn value(field: &str, value: &Value, maximum: usize) -> Result<()> {
        if serde_json::to_vec(value)?.len() > maximum {
            bail!("{field} must not exceed {maximum} bytes")
        }
        Ok(())
    }
    match kind {
        DeviceCommandKind::ProjectCreate(request) => {
            text("directoryRef", &request.directory_ref, 256)?;
            text("displayName", &request.display_name, 128)?;
            value("defaults", &request.defaults, 64 * 1024)?;
        }
        DeviceCommandKind::ProjectDelete { project_id } => {
            text("projectId", project_id, 128)?;
        }
        DeviceCommandKind::ThreadCreate { request, .. } => {
            if let Some(title) = &request.title {
                text("title", title, 256)?;
            }
            if let Some(message) = &request.first_message {
                text("firstMessage", message, 256 * 1024)?;
            }
            value("options", &request.options, 64 * 1024)?;
        }
        DeviceCommandKind::TurnStart { request, .. } => {
            text("text", &request.text, 256 * 1024)?;
            value("options", &request.options, 64 * 1024)?;
        }
        DeviceCommandKind::TurnSteer { request, .. } => {
            text("text", &request.text, 256 * 1024)?;
        }
        DeviceCommandKind::ApprovalDecide { request, .. } => {
            if let Some(response) = &request.response {
                value("response", response, 128 * 1024)?;
            }
        }
        DeviceCommandKind::Refresh
        | DeviceCommandKind::ThreadArchive { .. }
        | DeviceCommandKind::TurnInterrupt { .. }
        | DeviceCommandKind::HistorySync { .. } => {}
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};
    use tempfile::TempDir;

    fn fake_app_server(temp: &TempDir) -> (PathBuf, PathBuf) {
        let script = temp.path().join("fake-app-server.sh");
        let calls = temp.path().join("app-server-calls.jsonl");
        let source = r#"#!/bin/sh
calls='__CALLS__'
thread_number=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$calls"
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"userAgent":"fake","platformFamily":"unix","platformOs":"test"}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      thread_number=$((thread_number + 1))
      printf '{"id":%s,"result":{"thread":{"id":"app_new_%s","status":{"type":"idle"}}}}\n' "$id" "$thread_number"
      ;;
    *'"method":"thread/resume"'*)
      printf '{"id":%s,"error":{"code":-32600,"message":"no rollout found for thread id unexpected"}}\n' "$id"
      ;;
    *'"method":"turn/start"'*'"threadId":"app_missing"'*)
      printf '{"id":%s,"error":{"code":-32600,"message":"no rollout found for thread id app_missing"}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"app_turn_1","status":"inProgress"}}}\n' "$id"
      ;;
  esac
done
"#
        .replace("__CALLS__", calls.to_string_lossy().as_ref());
        std::fs::write(&script, source).unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        (script, calls)
    }

    async fn executor(temp: &TempDir, script: PathBuf) -> CommandExecutor {
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let data = temp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let store = ClientStore::open(&data).await.unwrap();
        store
            .create_project("prj_test", "Test", &workspace, &json!({}))
            .await
            .unwrap();
        let config = ClientConfig {
            device_id: Some("dev_test".into()),
            allowed_roots: vec![workspace],
            codex_command: script.to_string_lossy().into_owned(),
            codex_args: Vec::new(),
            ..ClientConfig::default()
        };
        let (events, _) = broadcast::channel(64);
        let (command_acks, _) = broadcast::channel(64);
        CommandExecutor {
            config: Arc::new(config.clone()),
            store,
            agents: AgentRuntimes::new(Arc::new(config)),
            device_id: "dev_test".into(),
            events,
            command_acks,
            command_notify: Arc::new(Notify::new()),
            history_import_lock: Arc::new(Mutex::new(())),
        }
    }

    #[tokio::test]
    async fn starts_first_turn_without_resuming_an_unpersisted_thread() {
        let temp = TempDir::new().unwrap();
        let (script, calls) = fake_app_server(&temp);
        let executor = executor(&temp, script).await;
        let result = executor
            .create_thread(
                "prj_test",
                &CreateThreadRequest {
                    title: None,
                    first_message: Some("hello".into()),
                    provider: AgentProvider::Codex,
                    access_mode: ConversationAccessMode::Full,
                    options: json!({"sandbox":"danger-full-access"}),
                },
            )
            .await
            .unwrap();
        let thread_id = result.get("threadId").and_then(Value::as_str).unwrap();
        assert!(executor.store.thread_has_turns(thread_id).await.unwrap());
        let calls = std::fs::read_to_string(calls).unwrap();
        assert!(calls.contains("\"method\":\"thread/start\""));
        assert!(calls.contains("\"method\":\"turn/start\""));
        assert!(!calls.contains("\"method\":\"thread/resume\""));
        executor.agents.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recreates_an_empty_thread_when_its_rollout_is_missing() {
        let temp = TempDir::new().unwrap();
        let (script, calls) = fake_app_server(&temp);
        let executor = executor(&temp, script).await;
        executor
            .store
            .create_thread(
                "thr_test",
                "prj_test",
                "app_missing",
                "Thread",
                &json!({"approvalPolicy":"never"}),
            )
            .await
            .unwrap();
        executor
            .start_turn(
                "thr_test",
                &StartTurnRequest {
                    text: "retry".into(),
                    access_mode: ConversationAccessMode::Full,
                    options: json!({}),
                },
            )
            .await
            .unwrap();
        let thread = executor.thread("thr_test").await.unwrap();
        assert_eq!(thread.app_server_thread_id.as_deref(), Some("app_new_1"));
        assert!(executor.store.thread_has_turns("thr_test").await.unwrap());
        let calls = std::fs::read_to_string(calls).unwrap();
        assert_eq!(calls.matches("\"method\":\"turn/start\"").count(), 2);
        assert_eq!(calls.matches("\"method\":\"thread/start\"").count(), 1);
        assert!(!calls.contains("\"method\":\"thread/resume\""));
        executor.agents.shutdown().await.unwrap();
    }
}
