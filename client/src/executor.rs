use crate::{
    agent::{AgentRuntimes, AgentThreadState},
    app_server::AppServerCallError,
    attachments,
    config::ClientConfig,
    directory, pairing,
    protocol::*,
    store::ClientStore,
};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify, RwLock, broadcast};

const STARTUP_RECOVERY_CONCURRENCY: usize = 4;
const STARTUP_RECOVERY_CALL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub struct ProviderHistoryLocks {
    codex: Arc<Mutex<()>>,
    kimi: Arc<Mutex<()>>,
    pi: Arc<Mutex<()>>,
}

impl ProviderHistoryLocks {
    fn for_provider(&self, provider: AgentProvider) -> &Mutex<()> {
        match provider {
            AgentProvider::Codex => &self.codex,
            AgentProvider::Kimi => &self.kimi,
            AgentProvider::Pi => &self.pi,
        }
    }
}

#[derive(Clone)]
pub struct CommandExecutor {
    pub config: Arc<ClientConfig>,
    pub store: ClientStore,
    pub agents: AgentRuntimes,
    pub device_id: String,
    pub display_name: Arc<RwLock<String>>,
    pub events: broadcast::Sender<NuntiusEvent>,
    pub command_acks: broadcast::Sender<TunnelFrame>,
    pub command_notify: Arc<Notify>,
    pub history_import_locks: ProviderHistoryLocks,
}

impl CommandExecutor {
    pub async fn apply_device_display_name(&self, display_name: &str) -> Result<()> {
        let desired = display_name.to_owned();
        let saved =
            tokio::task::spawn_blocking(move || ClientConfig::update_display_name(&desired))
                .await
                .context("device name configuration task failed")??;
        *self.display_name.write().await = saved.clone();
        tracing::info!(display_name = %saved, "device display name synchronized");
        Ok(())
    }

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
            DeviceCommandKind::ProviderUsageRefresh => {
                let reports = crate::provider_usage::collect_and_emit_all(self).await?;
                Ok(json!({
                    "reports": reports.into_iter().map(|report| json!({
                        "reportId": report.report_id,
                        "provider": report.provider.as_str(),
                        "status": report.status,
                    })).collect::<Vec<_>>()
                }))
            }
            DeviceCommandKind::ProjectCreate(request) => self.create_project(request).await,
            DeviceCommandKind::ProjectDelete { project_id } => {
                self.delete_project(project_id).await
            }
            DeviceCommandKind::ThreadCreate {
                project_id,
                request,
            } => self.create_thread(project_id, request).await,
            DeviceCommandKind::ThreadRename { thread_id, title } => {
                self.thread(thread_id).await?;
                self.store
                    .set_thread_display_title(thread_id, title.as_deref())
                    .await?;
                let updated = self.thread(thread_id).await?;
                self.emit_thread_summary(thread_id).await?;
                if let Err(error) = self.sync_thread(thread_id).await {
                    tracing::warn!(%thread_id,error=?error,"renamed thread history sync deferred");
                }
                Ok(json!({
                    "threadId": thread_id,
                    "title": &updated.title,
                    "thread": updated,
                }))
            }
            DeviceCommandKind::ThreadMarkViewed { thread_id } => {
                self.thread(thread_id).await?;
                let changed = self.store.mark_thread_viewed(thread_id).await?;
                let updated = self.thread(thread_id).await?;
                if changed {
                    self.emit_thread_summary(thread_id).await?;
                    if let Err(error) = self.sync_thread(thread_id).await {
                        tracing::warn!(%thread_id,error=?error,"viewed thread history sync deferred");
                    }
                }
                Ok(json!({
                    "threadId": thread_id,
                    "changed": changed,
                    "thread": updated,
                }))
            }
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
            DeviceCommandKind::TurnStart {
                thread_id,
                request,
                attachments,
            } => self.start_turn(thread_id, request, attachments).await,
            DeviceCommandKind::TurnSteer {
                thread_id,
                request,
                attachments,
            } => {
                let thread = self.command_thread(thread_id).await?;
                let state = self.resume_provider_thread(&thread).await?;
                let (input, views) = self
                    .prepare_user_input(thread_id, &request.text, attachments)
                    .await?;
                let options = self.provider_turn_options(&thread, &json!({})).await?;
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
                        &options,
                        request.client_message_id.as_deref(),
                    )
                    .await?;
                self.emit(
                    "turn.steered",
                    Some(&thread.project_id),
                    Some(thread_id),
                    None,
                    json!({"text":request.text,"attachments":views,"clientMessageId":request.client_message_id,"providerResult":result}),
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
                    && !(matches!(thread.provider, AgentProvider::Kimi | AgentProvider::Pi)
                        && state.status == "active")
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
                let approval_thread =
                    if let Some(thread_id) = self.store.approval_thread(approval_id).await? {
                        Some(self.thread(&thread_id).await?)
                    } else {
                        None
                    };
                let provider_session_id = approval_thread
                    .as_ref()
                    .and_then(|thread| thread.app_server_thread_id.clone());
                if let Err(error) = self
                    .agents
                    .resolve_approval(
                        provider,
                        provider_session_id.as_deref(),
                        request_id,
                        &request.decision,
                        request.response.clone(),
                    )
                    .await
                {
                    self.store
                        .finish_app_request(
                            approval_id,
                            "unknown",
                            Some(&request.decision),
                            Some("app_server_response_outcome_unknown"),
                        )
                        .await?;
                    self.emit_approval_resolved(
                        approval_id,
                        approval_thread.as_ref(),
                        "unknown",
                        Some(&request.decision),
                    )
                    .await;
                    return Err(error).context("approval outcome is unknown");
                }
                self.store
                    .finish_app_request(approval_id, "decided", Some(&request.decision), None)
                    .await?;
                self.emit_approval_resolved(
                    approval_id,
                    approval_thread.as_ref(),
                    "decided",
                    Some(&request.decision),
                )
                .await;
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

    /// Reattach every thread that was active before the client process
    /// restarted. Failed candidates remain `recovering` and are returned for
    /// the background retry loop.
    pub async fn recover_threads_once(&self, thread_ids: &[String]) -> Vec<String> {
        stream::iter(thread_ids.iter().cloned().map(|thread_id| {
            let executor = self.clone();
            async move {
                let result = executor.recover_thread(&thread_id).await;
                (thread_id, result)
            }
        }))
        .buffer_unordered(STARTUP_RECOVERY_CONCURRENCY)
        .filter_map(|(thread_id, result)| async move {
            match result {
                Ok(()) => None,
                Err(error) => {
                    tracing::warn!(%thread_id,error=?error,"running thread recovery deferred");
                    Some(thread_id)
                }
            }
        })
        .collect()
        .await
    }

    pub async fn retry_thread_recovery(&self, mut pending: Vec<String>) {
        let mut delay = Duration::from_secs(2);
        while !pending.is_empty() {
            tokio::time::sleep(delay).await;
            let previous = pending.len();
            pending = self.recover_threads_once(&pending).await;
            if pending.len() < previous {
                tracing::info!(
                    remaining = pending.len(),
                    "running thread recovery made progress"
                );
                delay = Duration::from_secs(2);
            } else {
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
        tracing::info!("all running threads recovered after restart");
    }

    async fn recover_thread(&self, thread_id: &str) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        let state = match self
            .resume_provider_thread_with_timeout(&thread, STARTUP_RECOVERY_CALL_TIMEOUT)
            .await
        {
            Ok(state) => state,
            Err(error) if is_missing_app_thread(&error) => {
                self.store.set_thread_status(thread_id, "unknown").await?;
                self.sync_thread(thread_id).await?;
                self.emit_thread_summary(thread_id).await?;
                tracing::warn!(%thread_id,"previously running App Server thread no longer exists");
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        match state.status.as_str() {
            "active" => {
                self.store
                    .mark_app_turn_started(thread_id, state.active_turn_id.as_deref())
                    .await?;
            }
            "idle" => {
                self.store
                    .complete_app_turn(thread_id, state.active_turn_id.as_deref(), "completed")
                    .await?;
            }
            status => {
                self.store
                    .complete_app_turn(thread_id, state.active_turn_id.as_deref(), "failed")
                    .await?;
                self.store.set_thread_status(thread_id, status).await?;
            }
        }

        // Resume establishes the live App Server attachment. A bounded full
        // read immediately fills output missed while the processes restarted;
        // rollout monitoring remains the fallback if this optional backfill
        // is temporarily unavailable.
        if let Err(error) = self
            .refresh_thread_history_with_timeout(thread_id, STARTUP_RECOVERY_CALL_TIMEOUT)
            .await
        {
            tracing::warn!(%thread_id,error=?error,"recovered thread history backfill deferred");
            self.sync_thread(thread_id).await?;
        }
        self.emit_thread_summary(thread_id).await?;
        Ok(())
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
                attachment_ids: Vec::new(),
                client_message_id: None,
                access_mode: request.access_mode,
                options: Value::Object(Map::new()),
            };
            let _ = self.start_turn(&id, &start, &[]).await?;
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

    async fn start_turn(
        &self,
        thread_id: &str,
        request: &StartTurnRequest,
        attachments: &[AttachmentRef],
    ) -> Result<Value> {
        // This is the user's current authorization choice for the conversation.
        // Codex cannot change an active turn's approval policy through turn/steer,
        // so approval callbacks must also consult this persisted bridge state.
        self.store
            .set_thread_access_mode(thread_id, request.access_mode)
            .await?;
        let (input, attachment_views) = self
            .prepare_user_input(thread_id, &request.text, attachments)
            .await?;
        let mut thread = self.command_thread(thread_id).await?;
        // A new App Server thread has no rollout until its first turn starts.
        // Calling thread/resume here is therefore invalid; follow the protocol's
        // thread/start -> turn/start lifecycle directly.
        if !self.store.thread_has_turns(thread_id).await? {
            return match self
                .begin_turn(&thread, request, &input, &attachment_views)
                .await
            {
                Ok(result) => Ok(result),
                Err(error)
                    if (thread.provider == AgentProvider::Codex
                        && is_missing_app_thread(&error))
                        || (thread.provider == AgentProvider::Pi
                            && is_missing_pi_session(&error)) =>
                {
                    thread = self.recreate_empty_provider_thread(&thread).await?;
                    self.begin_turn(&thread, request, &input, &attachment_views)
                        .await
                }
                Err(error) => Err(error),
            };
        }
        let state = self.resume_provider_thread(&thread).await?;
        if state.active_turn_id.is_some()
            || (matches!(thread.provider, AgentProvider::Kimi | AgentProvider::Pi)
                && state.status == "active")
        {
            let options = self
                .provider_turn_options(&thread, &request.options)
                .await?;
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
                    &options,
                    request.client_message_id.as_deref(),
                )
                .await?;
            self.emit(
                "turn.steered",
                Some(&thread.project_id),
                Some(thread_id),
                None,
                json!({"text":request.text,"attachments":attachment_views,"clientMessageId":request.client_message_id,"providerResult":result}),
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
        self.begin_turn(&thread, request, &input, &attachment_views)
            .await
    }

    async fn prepare_user_input(
        &self,
        thread_id: &str,
        text: &str,
        attachment_refs: &[AttachmentRef],
    ) -> Result<(Vec<Value>, Vec<AttachmentView>)> {
        if text.trim().is_empty() && attachment_refs.is_empty() {
            bail!("a turn requires text or at least one image")
        }
        let mut input = Vec::with_capacity(attachment_refs.len() + 1);
        if !text.trim().is_empty() {
            input.push(json!({"type":"text","text":text}));
        }
        let mut views = Vec::with_capacity(attachment_refs.len());
        let access_token = if attachment_refs.is_empty() {
            None
        } else {
            Some(pairing::access_token(&self.config).await?)
        };
        for attachment in attachment_refs {
            let local_path = attachments::ensure_local(
                &self.config,
                thread_id,
                attachment,
                access_token
                    .as_deref()
                    .expect("token exists for image input"),
            )
            .await
            .with_context(|| format!("cannot receive attachment {}", attachment.id))?;
            let view = self
                .store
                .upsert_attachment(thread_id, attachment, &local_path)
                .await?;
            input.push(json!({"type":"localImage","path":local_path,"detail":"auto"}));
            views.push(view);
        }
        Ok((input, views))
    }

    async fn provider_turn_options(
        &self,
        thread: &ThreadSummary,
        requested: &Value,
    ) -> Result<Value> {
        if thread.provider == AgentProvider::Codex {
            return Ok(requested.clone());
        }
        let saved = self.store.app_server_options(&thread.id).await?;
        Ok(merge_turn_options(&saved, requested))
    }

    async fn begin_turn(
        &self,
        thread: &ThreadSummary,
        request: &StartTurnRequest,
        input: &[Value],
        attachments: &[AttachmentView],
    ) -> Result<Value> {
        let options = self.provider_turn_options(thread, &request.options).await?;
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
                &options,
                request.client_message_id.as_deref(),
            )
            .await?;
        // Kimi's durable transcript is keyed by `user_message_id`; `prompt_id`
        // is queue/runtime identity and is not guaranteed to appear on history
        // messages. Pi's prompt_id is already its stable JSONL entry id.
        let provider_turn = if thread.provider == AgentProvider::Kimi {
            extract_id(&result, &["user_message_id"]).or_else(|| {
                extract_id(
                    &result,
                    &["turn/id", "turnId", "id", "prompt_id", "prompt/prompt_id"],
                )
            })
        } else {
            extract_id(
                &result,
                &["turn/id", "turnId", "id", "prompt_id", "prompt/prompt_id"],
            )
        };
        let local_turn = self
            .store
            .record_user_turn(
                &thread.id,
                provider_turn.as_deref(),
                &request.text,
                attachments,
                request.access_mode,
            )
            .await?;
        self.sync_thread(&thread.id).await?;
        self.emit_thread_summary(&thread.id).await?;
        self.emit(
            "turn.started",
            Some(&thread.project_id),
            Some(&thread.id),
            Some(&local_turn),
            json!({"text":request.text,"attachments":attachments,"clientMessageId":request.client_message_id,"providerResult":result}),
            true,
        )
        .await?;
        Ok(json!({"operation":"start","turnId":local_turn,"providerResult":result}))
    }

    async fn recreate_empty_provider_thread(&self, thread: &ThreadSummary) -> Result<ThreadSummary> {
        if self.store.thread_has_turns(&thread.id).await? {
            bail!("refusing to replace a provider thread that already has local history")
        }
        if !matches!(thread.provider, AgentProvider::Codex | AgentProvider::Pi) {
            bail!("only an empty Codex or Pi thread can be recreated")
        }
        let options = self.store.app_server_options(&thread.id).await?;
        let access_mode = self.store.thread_access_mode(&thread.id).await?;
        let app_id = self
            .start_provider_thread(
                thread.provider,
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
            provider = thread.provider.as_str(),
            previous_app_thread_id = ?thread.app_server_thread_id,
            app_thread_id = %app_id,
            "recreated empty provider thread after missing provider session"
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

    async fn resume_provider_thread_with_timeout(
        &self,
        thread: &ThreadSummary,
        timeout: Duration,
    ) -> Result<AgentThreadState> {
        tokio::time::timeout(timeout, self.resume_provider_thread(thread))
            .await
            .context("provider thread resume timed out")?
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
    async fn emit_approval_resolved(
        &self,
        approval_id: &str,
        thread: Option<&ThreadSummary>,
        status: &str,
        decision: Option<&str>,
    ) {
        if let Err(error) = self
            .emit(
                "approval.resolved",
                thread.map(|value| value.project_id.as_str()),
                thread.map(|value| value.id.as_str()),
                None,
                json!({"approvalId":approval_id,"status":status,"decision":decision}),
                true,
            )
            .await
        {
            tracing::warn!(approval_id, status, error=?error, "approval resolution event could not be queued");
        }
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
        let mut batches = Vec::with_capacity(chunk_count);
        for (index, records) in chunks.into_iter().enumerate() {
            let cursor = new_id("hist");
            let chunk_hash = hex::encode(Sha256::digest(serde_json::to_vec(&records)?));
            batches.push(HistoryBatch {
                batch_id: new_id("hbatch"),
                device_id: self.device_id.clone(),
                thread_id: thread_id.into(),
                from_cursor: previous_cursor,
                to_cursor: cursor.clone(),
                inventory_revision: revision,
                payload_hash: chunk_hash,
                complete: index + 1 == chunk_count,
                records,
            });
            previous_cursor = Some(cursor);
        }
        self.store
            .replace_history(thread_id, revision, &batches, &payload_hash)
            .await?;
        Ok(())
    }
    pub async fn refresh_thread_history(&self, thread_id: &str) -> Result<()> {
        self.refresh_thread_history_with_timeout(thread_id, Duration::from_secs(180))
            .await
    }

    async fn refresh_thread_history_with_timeout(
        &self,
        thread_id: &str,
        timeout: Duration,
    ) -> Result<()> {
        let thread = self.thread(thread_id).await?;
        let _guard = self
            .history_import_locks
            .for_provider(thread.provider)
            .lock()
            .await;
        let app_thread_id = thread
            .app_server_thread_id
            .as_deref()
            .context("thread has no provider session id")?;
        let provider_thread = tokio::time::timeout(
            timeout,
            self.agents.read_thread(thread.provider, app_thread_id),
        )
        .await
        .context("provider thread history read timed out")??;
        self.store
            .import_app_history(thread_id, &provider_thread)
            .await?;
        self.sync_thread(thread_id).await?;
        self.store
            .state_set(
                &thread_fingerprint_key(thread.provider, app_thread_id),
                &thread_fingerprint(&provider_thread)?,
            )
            .await?;
        if thread.provider == AgentProvider::Kimi
            && let Some(seq) = provider_thread
                .pointer("/_nuntiusProviderCursor/seq")
                .and_then(Value::as_u64)
        {
            self.agents
                .kimi
                .ack_event(
                    app_thread_id,
                    seq,
                    provider_thread
                        .pointer("/_nuntiusProviderCursor/epoch")
                        .and_then(Value::as_str),
                )
                .await?;
            if app_thread_status(&provider_thread) == "active" {
                self.agents.kimi.retry_subscription(app_thread_id).await;
            }
        }
        Ok(())
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

    /// Reconcile sessions created or changed outside Nuntius for providers
    /// with a service/file-owned session index (Kimi's web service, Pi's
    /// session directory), so there is no rollout-file watcher equivalent to
    /// Codex's.
    pub async fn reconcile_provider_recent(
        &self,
        provider: AgentProvider,
        archived: bool,
    ) -> Result<usize> {
        let threads = self.agents.list_threads(provider, None, archived).await?;
        let mut refreshed = 0;
        for provider_thread in threads {
            let Some(session_id) = provider_thread.get("id").and_then(Value::as_str) else {
                continue;
            };
            if self
                .provider_thread_is_removed(provider, &provider_thread)
                .await?
            {
                continue;
            }
            let fingerprint = thread_fingerprint(&provider_thread)?;
            let key = thread_fingerprint_key(provider, session_id);
            let local_thread_id = self
                .store
                .local_provider_thread_id(provider, session_id)
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
                .import_provider_thread(provider, &project_id, &provider_thread)
                .await?;
            match self
                .import_and_sync_thread(provider, &local_thread_id, &provider_thread)
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
                    tracing::warn!(provider=provider.as_str(),%session_id,error=?error,"recent provider session reconciliation failed")
                }
            }
        }
        Ok(refreshed)
    }

    /// Import the cheap, durable inventory projection embedded in a rollout.
    ///
    /// `thread/list` can be incomplete when Codex's state database lags behind
    /// its rollout files. The monitor calls this before the expensive
    /// `thread/read` hydration so every existing workspace and conversation is
    /// visible immediately, even while full history is still reconciling.
    pub async fn import_rollout_inventory(&self, app_thread: &Value) -> Result<bool> {
        let app_id = app_thread
            .get("id")
            .and_then(Value::as_str)
            .context("rollout inventory has no thread id")?;
        let _guard = self
            .history_import_locks
            .for_provider(AgentProvider::Codex)
            .lock()
            .await;
        if self
            .store
            .local_provider_thread_id(AgentProvider::Codex, app_id)
            .await?
            .is_some()
        {
            return Ok(false);
        }
        if self
            .provider_thread_is_removed(AgentProvider::Codex, app_thread)
            .await?
        {
            return Ok(false);
        }
        let Some(raw_cwd) = app_thread.get("cwd").and_then(Value::as_str) else {
            return Ok(false);
        };
        if directory::validate_project_path(&self.config, std::path::Path::new(raw_cwd)).is_err() {
            // Deleted, temporary and out-of-scope working directories must not
            // turn into empty projects merely because an old rollout remains.
            return Ok(false);
        }
        let project_id = self.project_for_app_thread(None, app_thread).await?;
        let local_thread = self
            .store
            .import_provider_thread(AgentProvider::Codex, &project_id, app_thread)
            .await?;
        self.sync_thread(&local_thread).await?;
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
        Ok(true)
    }

    /// Force a single App Server thread into the local durable history outbox.
    /// Used by the rollout-file monitor so external terminal activity does not
    /// depend on this App Server instance receiving runtime notifications.
    pub async fn reconcile_app_thread(&self, app_id: &str) -> Result<()> {
        let _guard = self
            .history_import_locks
            .for_provider(AgentProvider::Codex)
            .lock()
            .await;
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
        for provider in [AgentProvider::Codex, AgentProvider::Kimi, AgentProvider::Pi] {
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
        for provider in [AgentProvider::Codex, AgentProvider::Kimi, AgentProvider::Pi] {
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
        match self
            .store
            .create_project(&id, &name, &path, &json!({}))
            .await
        {
            Ok(()) => Ok(id),
            Err(error) => {
                // Full App Server discovery and rollout inventory recovery run
                // concurrently at startup. If both discover the same cwd,
                // converge on the winner of the canonical-path UNIQUE key.
                if let Some(existing) = self.store.project_by_path(&path).await? {
                    Ok(existing)
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn import_and_sync_thread(
        &self,
        provider: AgentProvider,
        local_thread: &str,
        app_thread: &Value,
    ) -> Result<()> {
        let _guard = self
            .history_import_locks
            .for_provider(provider)
            .lock()
            .await;
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
        self.sync_thread(local_thread).await?;
        if provider == AgentProvider::Kimi
            && let Some(seq) = detail
                .pointer("/_nuntiusProviderCursor/seq")
                .and_then(Value::as_u64)
        {
            self.agents
                .kimi
                .ack_event(
                    app_id,
                    seq,
                    detail
                        .pointer("/_nuntiusProviderCursor/epoch")
                        .and_then(Value::as_str),
                )
                .await?;
            if app_thread_status(&detail) == "active" {
                self.agents.kimi.retry_subscription(app_id).await;
            }
        }
        Ok(())
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
        // Browser replay has a separate bounded journal. It intentionally also
        // includes transient deltas so a page reload can resume the current item;
        // maintenance caps the journal and completed items remain in history.
        self.store.append_browser_event(&event).await?;
        let _ = self.events.send(event.clone());
        Ok(event)
    }
}

pub async fn process_app_events(
    executor: CommandExecutor,
    mut receiver: broadcast::Receiver<crate::app_server::AppServerEvent>,
) {
    loop {
        match receiver.recv().await {
            Ok(event) => {
                if let Err(error) = process_app_event(&executor, event.payload.clone()).await {
                    tracing::warn!(error=?error,"failed to process App Server event")
                }
                if let Err(error) = executor.agents.codex.acknowledge(&event).await {
                    tracing::warn!(error=?error, "failed to persist Agent Host event cursor");
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
    if method == "nuntius/resync_required" {
        executor.discover_all().await?;
        return Ok(());
    }
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
        let approval_id = codex_approval_id(method, &params);
        let approval_context = if let (Some(local_thread), Some(app_turn)) = (
            thread_id.as_deref(),
            find_string(&event_params, &["turnId", "turn/id"]),
        ) {
            executor
                .store
                .app_turn_approval_context(local_thread, &app_turn)
                .await?
        } else {
            None
        };
        let thread_access_mode = if let Some(local_thread) = thread_id.as_deref() {
            Some(executor.store.thread_access_mode(local_thread).await?)
        } else {
            None
        };
        let automatic = codex_automatic_approval_response(
            method,
            &params,
            approval_context.as_ref().map(|(status, _)| status.as_str()),
            thread_access_mode,
        );
        if let Some((decision, response)) = automatic {
            executor
                .agents
                .resolve_approval(
                    AgentProvider::Codex,
                    None,
                    request_id.clone(),
                    decision,
                    Some(response),
                )
                .await?;
            executor
                .store
                .finish_app_request(&approval_id, "decided", Some(decision), None)
                .await?;
            let approval_thread = if let Some(local_thread) = thread_id.as_deref() {
                executor
                    .store
                    .thread(local_thread, &executor.device_id)
                    .await?
            } else {
                None
            };
            executor
                .emit_approval_resolved(
                    &approval_id,
                    approval_thread.as_ref(),
                    "decided",
                    Some(decision),
                )
                .await;
            tracing::info!(
                method,
                approval_id,
                decision,
                "automatically resolved Codex approval request"
            );
            return Ok(());
        }
        let inserted = executor
            .store
            .save_provider_request(
                AgentProvider::Codex,
                &approval_id,
                request_id,
                method,
                &event_params,
                project_id.as_deref(),
                thread_id.as_deref(),
            )
            .await?;
        if inserted {
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
        }
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
    let emitted = executor
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
                    &emitted.occurred_at,
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
                let cursor = kimi_message_cursor(&message);
                match process_kimi_event(&executor, message).await {
                    Ok(()) => {
                        if let Some((session_id, seq, epoch)) = cursor
                            && let Err(error) = executor
                                .agents
                                .kimi
                                .ack_event(&session_id, seq, epoch.as_deref())
                                .await
                        {
                            tracing::warn!(
                                error=?error,
                                %session_id,
                                seq,
                                "failed to persist Kimi event cursor"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(error=?error, "failed to process Kimi event"),
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
    let mut payload = message.get("payload").cloned().unwrap_or(Value::Null);
    if let Some(object) = payload.as_object_mut() {
        if !object.contains_key("streamOffset")
            && let Some(offset) = message.get("offset")
        {
            object.insert("streamOffset".into(), offset.clone());
        }
        if !object.contains_key("providerSeq")
            && let Some(seq) = message.get("seq")
        {
            object.insert("providerSeq".into(), seq.clone());
        }
    }
    let provider_turn_id = kimi_provider_turn_id(&payload);
    let provider_turn = if let Some(provider_turn_id) = provider_turn_id.as_deref() {
        executor
            .store
            .local_turn_id_for_app(&thread_id, provider_turn_id)
            .await?
    } else {
        None
    };
    let mut local_turn_id = provider_turn;
    if local_turn_id.is_none() {
        local_turn_id = executor.store.active_local_turn_id(&thread_id).await?;
    }

    if kimi_non_main_transcript_event(event_type, &payload) {
        return Ok(());
    }

    if event_type == "nuntius.resync_required" {
        executor.refresh_thread_history(&thread_id).await?;
        executor.emit_thread_summary(&thread_id).await?;
        executor.agents.kimi.retry_subscription(session_id).await;
        return Ok(());
    }

    if event_type == "event.assistant.delta" && payload.get("delta").is_some_and(Value::is_object)
    {
        let durable = message.get("volatile").and_then(Value::as_bool) != Some(true);
        let message_id = payload
            .get("message_id")
            .and_then(Value::as_str)
            .unwrap_or("kimi-assistant");
        let content_index = payload
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        for (kind, field) in [
            ("agent.assistant.delta", "text"),
            ("agent.thinking.delta", "thinking"),
        ] {
            if let Some(delta) = payload
                .pointer(&format!("/delta/{field}"))
                .and_then(Value::as_str)
                .filter(|delta| !delta.is_empty())
            {
                executor
                    .emit(
                        kind,
                        Some(&thread.project_id),
                        Some(&thread_id),
                        local_turn_id.as_deref(),
                        json!({
                            "delta":delta,
                            "itemId":format!("{message_id}:{content_index}:{field}"),
                            "providerSeq":payload.get("providerSeq"),
                        }),
                        durable,
                    )
                    .await?;
            }
        }
        return Ok(());
    }

    if event_type == "event.approval.requested" {
        let provider_approval_id = payload
            .get("approval_id")
            .and_then(Value::as_str)
            .context("Kimi approval event has no approval_id")?;
        let approval_id = format!("apr_kimi_{provider_approval_id}");
        let inserted = executor
            .store
            .save_provider_request(
                AgentProvider::Kimi,
                &approval_id,
                &json!({"approvalId":provider_approval_id,"sessionId":session_id}),
                "kimi/approval",
                &payload,
                Some(&thread.project_id),
                Some(&thread_id),
            )
            .await?;
        if inserted {
            executor
                .emit(
                    "approval.requested",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    local_turn_id.as_deref(),
                    json!({"approvalId":approval_id,"method":"kimi/approval","params":payload}),
                    true,
                )
                .await?;
        }
        return Ok(());
    }

    if event_type == "event.question.requested" {
        let provider_question_id = payload
            .get("question_id")
            .and_then(Value::as_str)
            .context("Kimi question event has no question_id")?;
        let approval_id = format!("apr_kimi_question_{provider_question_id}");
        let inserted = executor
            .store
            .save_provider_request(
                AgentProvider::Kimi,
                &approval_id,
                &json!({
                    "kind":"question",
                    "questionId":provider_question_id,
                    "sessionId":session_id,
                }),
                "kimi/question",
                &payload,
                Some(&thread.project_id),
                Some(&thread_id),
            )
            .await?;
        if inserted {
            executor
                .emit(
                    "approval.requested",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    local_turn_id.as_deref(),
                    json!({
                        "approvalId":approval_id,
                        "method":"kimi/question",
                        "params":payload,
                    }),
                    true,
                )
                .await?;
        }
        return Ok(());
    }

    let resolved_approval = match event_type {
        "event.approval.resolved" => payload
            .get("approval_id")
            .and_then(Value::as_str)
            .map(|id| {
                let decision = match payload.get("decision").and_then(Value::as_str) {
                    Some("approved") => "accept",
                    Some("rejected") => "decline",
                    _ => "cancel",
                };
                (format!("apr_kimi_{id}"), "decided", Some(decision))
            }),
        "event.approval.expired" => payload
            .get("approval_id")
            .and_then(Value::as_str)
            .map(|id| (format!("apr_kimi_{id}"), "unknown", None)),
        "event.question.answered" => payload
            .get("question_id")
            .and_then(Value::as_str)
            .map(|id| {
                (
                    format!("apr_kimi_question_{id}"),
                    "decided",
                    Some("accept"),
                )
            }),
        "event.question.dismissed" => payload
            .get("question_id")
            .and_then(Value::as_str)
            .map(|id| {
                (
                    format!("apr_kimi_question_{id}"),
                    "decided",
                    Some("cancel"),
                )
            }),
        _ => None,
    };
    if let Some((approval_id, status, decision)) = resolved_approval {
        executor
            .store
            .finish_app_request(&approval_id, status, decision, None)
            .await?;
        executor
            .emit_approval_resolved(&approval_id, Some(&thread), status, decision)
            .await;
        return Ok(());
    }

    let mut reconcile_terminal = false;
    match event_type {
        "turn.started" => {
            local_turn_id = Some(
                executor
                    .store
                    .mark_app_turn_started(&thread_id, None)
                    .await?,
            );
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
        }
        "turn.ended" => {
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let status = kimi_turn_status(reason);
            if local_turn_id.is_some() {
                executor
                    .store
                    .complete_app_turn(&thread_id, None, status)
                    .await?;
            }
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
            reconcile_terminal = true;
        }
        "event.session.work_changed" => {
            let busy = payload.get("busy").and_then(Value::as_bool) == Some(true);
            let main_turn_active = payload
                .get("main_turn_active")
                .and_then(Value::as_bool)
                .unwrap_or(busy);
            if main_turn_active {
                local_turn_id = Some(
                    executor
                        .store
                        .mark_app_turn_started(&thread_id, None)
                        .await?,
                );
            } else {
                let status = payload
                    .get("last_turn_reason")
                    .and_then(Value::as_str)
                    .map(kimi_turn_status)
                    .unwrap_or("completed");
                if local_turn_id.is_some() {
                    executor
                        .store
                        .complete_app_turn(&thread_id, None, status)
                        .await?;
                }
                reconcile_terminal = true;
            }
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
        }
        _ => {}
    }

    let durable = message.get("volatile").and_then(Value::as_bool) != Some(true);
    let (emitted_event_type, emitted_payload) = match event_type {
        "event.tool.started" => (
            "tool.call.started",
            json!({
                "toolCallId":payload.get("tool_call_id"),
                "name":payload.get("tool_name"),
                "args":payload.get("input"),
            }),
        ),
        "event.tool.output" => (
            "tool.progress",
            json!({
                "toolCallId":payload.get("tool_call_id"),
                "update":payload.get("chunk"),
                "stream":payload.get("stream"),
                "mode":"append",
            }),
        ),
        "event.tool.progress" => (
            "tool.progress",
            json!({
                "toolCallId":payload.get("tool_call_id"),
                "update":payload.get("message"),
                "progress":payload.get("progress"),
                "mode":"append",
            }),
        ),
        "event.tool.completed" => (
            "tool.result",
            json!({
                "toolCallId":payload.get("tool_call_id"),
                "output":payload.get("output"),
                "isError":payload.get("is_error"),
                "durationMs":payload.get("duration_ms"),
            }),
        ),
        _ => (event_type, payload),
    };
    executor
        .emit(
            &format!("agent.{emitted_event_type}"),
            Some(&thread.project_id),
            Some(&thread_id),
            local_turn_id.as_deref(),
            emitted_payload,
            durable,
        )
        .await?;

    if reconcile_terminal {
        executor.refresh_thread_history(&thread_id).await?;
        executor.emit_thread_summary(&thread_id).await?;
        executor.agents.kimi.unsubscribe_session(session_id).await;
    }
    Ok(())
}

pub async fn process_pi_events(executor: CommandExecutor) {
    let mut receiver = executor.agents.pi.subscribe();
    loop {
        match receiver.recv().await {
            Ok(message) => {
                if let Err(error) = process_pi_event(&executor, message).await {
                    tracing::warn!(error=?error, "failed to process Pi event")
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "Pi event processor lagged; reconciling history");
                if let Err(error) = executor.discover_all().await {
                    tracing::warn!(error=?error, "history reconciliation after Pi event loss failed");
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Pi RPC events are translated into the same `agent.*` vocabulary the Kimi
/// pipeline emits, so both consoles reuse one live-stream merger.
async fn process_pi_event(executor: &CommandExecutor, message: Value) -> Result<()> {
    let session_id = message
        .get("session_id")
        .and_then(Value::as_str)
        .context("Pi event has no session id")?;
    if session_id.is_empty() {
        // The process has not been registered to a session yet.
        return Ok(());
    }
    let event = message.get("event").cloned().unwrap_or(Value::Null);
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    let Some(thread_id) = executor
        .store
        .local_provider_thread_id(AgentProvider::Pi, session_id)
        .await?
    else {
        return Ok(());
    };
    let thread = executor.thread(&thread_id).await?;

    if event_type == "extension_ui_request" {
        let method = event.get("method").and_then(Value::as_str).unwrap_or("");
        if !matches!(method, "select" | "confirm" | "input" | "editor") {
            return Ok(());
        }
        let request_id = event
            .get("id")
            .and_then(Value::as_str)
            .context("Pi extension UI request has no id")?;
        let approval_id = format!("apr_pi_{request_id}");
        let inserted = executor
            .store
            .save_provider_request(
                AgentProvider::Pi,
                &approval_id,
                &json!({"sessionId":session_id,"requestId":request_id,"uiMethod":method}),
                "pi/extension_ui",
                &event,
                Some(&thread.project_id),
                Some(&thread_id),
            )
            .await?;
        if inserted {
            executor
                .emit(
                    "approval.requested",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    None,
                    json!({"approvalId":approval_id,"method":"pi/extension_ui","params":event}),
                    true,
                )
                .await?;
        }
        return Ok(());
    }

    match event_type {
        "agent_start" => {
            let local_turn_id = executor
                .store
                .mark_app_turn_started(&thread_id, None)
                .await?;
            executor
                .store
                .state_set(&format!("pi:last-end:{session_id}"), "running")
                .await?;
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
            executor
                .emit(
                    "agent.turn.started",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    Some(&local_turn_id),
                    json!({}),
                    true,
                )
                .await?;
        }
        "agent_end" => {
            let reason = pi_agent_end_reason(&event);
            executor
                .store
                .state_set(&format!("pi:last-end:{session_id}"), reason)
                .await?;
            let local_turn_id = executor.store.active_local_turn_id(&thread_id).await?;
            executor
                .emit(
                    "agent.run.ended",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    local_turn_id.as_deref(),
                    json!({
                        "reason":reason,
                        "willRetry":event.get("willRetry").and_then(Value::as_bool).unwrap_or(false),
                    }),
                    true,
                )
                .await?;
        }
        "agent_settled" => {
            let local_turn_id = executor.store.active_local_turn_id(&thread_id).await?;
            let app_turn_id = executor.store.active_app_turn_id(&thread_id).await?;
            let reason = executor
                .store
                .state_get(&format!("pi:last-end:{session_id}"))
                .await?
                .filter(|reason| reason != "running")
                .unwrap_or_else(|| "completed".into());
            executor
                .store
                .complete_app_turn(
                    &thread_id,
                    app_turn_id.as_deref(),
                    kimi_turn_status(&reason),
                )
                .await?;
            executor.sync_thread(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
            executor
                .emit(
                    "agent.turn.ended",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    local_turn_id.as_deref(),
                    json!({"reason":reason}),
                    true,
                )
                .await?;
            executor.refresh_thread_history(&thread_id).await?;
            executor.emit_thread_summary(&thread_id).await?;
        }
        "message_update" => {
            let delta = event.get("assistantMessageEvent").cloned().unwrap_or(Value::Null);
            let (kind, text) = match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => (
                    "agent.assistant.delta",
                    delta.get("delta").and_then(Value::as_str),
                ),
                Some("thinking_delta") => (
                    "agent.thinking.delta",
                    delta.get("delta").and_then(Value::as_str),
                ),
                _ => ("", None),
            };
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                executor
                    .emit(
                        kind,
                        Some(&thread.project_id),
                        Some(&thread_id),
                        None,
                        json!({"delta":text}),
                        false,
                    )
                    .await?;
            }
        }
        "tool_execution_start" => {
            executor
                .emit(
                    "agent.tool.call.started",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    None,
                    json!({
                        "toolCallId": event.get("toolCallId"),
                        "name": event.get("toolName"),
                        "args": event.get("args"),
                    }),
                    true,
                )
                .await?;
        }
        "tool_execution_update" => {
            let update = event
                .pointer("/partialResult/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !update.is_empty() {
                executor
                    .emit(
                        "agent.tool.progress",
                        Some(&thread.project_id),
                        Some(&thread_id),
                        None,
                        json!({
                            "toolCallId": event.get("toolCallId"),
                            "name": event.get("toolName"),
                            "update": update,
                            "mode": "replace",
                        }),
                        false,
                    )
                    .await?;
            }
        }
        "tool_execution_end" => {
            let output = event
                .pointer("/result/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            executor
                .emit(
                    "agent.tool.result",
                    Some(&thread.project_id),
                    Some(&thread_id),
                    None,
                    json!({
                        "toolCallId": event.get("toolCallId"),
                        "name": event.get("toolName"),
                        "output": output,
                        "isError": event.get("isError").and_then(Value::as_bool).unwrap_or(false),
                    }),
                    true,
                )
                .await?;
        }
        _ => {}
    }
    Ok(())
}

fn pi_agent_end_reason(event: &Value) -> &'static str {
    let stop_reason = event
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .and_then(|message| message.get("stopReason").and_then(Value::as_str));
    match stop_reason {
        Some("aborted") => "cancelled",
        Some("error") => "failed",
        _ => "completed",
    }
}

fn kimi_turn_status(reason: &str) -> &'static str {
    match reason {
        "cancelled" => "interrupted",
        "failed" | "blocked" => "failed",
        _ => "completed",
    }
}

fn kimi_message_cursor(message: &Value) -> Option<(String, u64, Option<String>)> {
    if message.get("volatile").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let session_id = message
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/payload/session_id").and_then(Value::as_str))?
        .to_owned();
    let seq = if message.get("type").and_then(Value::as_str)
        == Some("nuntius.resync_required")
    {
        message.pointer("/payload/current_seq").and_then(Value::as_u64)
    } else {
        message.get("seq").and_then(Value::as_u64)
    }?;
    let epoch = message
        .get("epoch")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/payload/epoch").and_then(Value::as_str))
        .map(str::to_owned);
    Some((session_id, seq, epoch))
}

fn kimi_provider_turn_id(payload: &Value) -> Option<String> {
    [
        "user_message_id",
        "userMessageId",
        "prompt_id",
        "promptId",
        "turnId",
        "turn_id",
        "current_prompt_id",
    ]
    .into_iter()
    .find_map(|key| kimi_event_id(payload, key))
}

fn kimi_non_main_transcript_event(event_type: &str, payload: &Value) -> bool {
    let agent_id = payload
        .get("agentId")
        .and_then(Value::as_str)
        .or_else(|| payload.get("agent_id").and_then(Value::as_str));
    if agent_id.is_none_or(|agent_id| agent_id == "main") {
        return false;
    }
    let event_type = event_type.strip_prefix("event.").unwrap_or(event_type);
    event_type.starts_with("turn.")
        || event_type.starts_with("assistant.")
        || event_type.starts_with("thinking.")
        || event_type.starts_with("tool.")
        || event_type.starts_with("prompt.")
        || event_type == "error"
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

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
fn merge_turn_options(saved: &Value, requested: &Value) -> Value {
    let mut options = object(saved.clone());
    options.extend(object(requested.clone()));
    Value::Object(options)
}
fn is_missing_app_thread(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<AppServerCallError>()
        .is_some_and(AppServerCallError::is_missing_thread)
}
/// Pi only persists a session file on its first message; a thread created
/// but never prompted has nothing to reattach to after a Client restart.
fn is_missing_pi_session(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("Pi session") && message.contains("not found")
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

fn codex_approval_id(method: &str, params: &Value) -> String {
    let stable_request_id = find_string(
        params,
        &[
            "itemId",
            "callId",
            "requestId",
            "item/id",
            "call/id",
            "request/id",
        ],
    );
    let Some(stable_request_id) = stable_request_id else {
        return new_id("apr");
    };
    let identity = json!({
        "method": method,
        "threadId": find_string(params, &["threadId", "thread/id", "conversationId"]),
        "turnId": find_string(params, &["turnId", "turn/id"]),
        "requestId": stable_request_id,
    });
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&identity).expect("approval identity is JSON serializable"),
    ));
    format!("apr_codex_{}", &digest[..32])
}

fn provider_turn_status_is_active(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "active" | "running" | "inprogress" | "recovering" | "stalled"
    )
}

fn codex_approval_response(
    method: &str,
    params: &Value,
    approve: bool,
) -> Option<(&'static str, Value)> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let decision = if approve { "accept" } else { "cancel" };
            Some((decision, json!({"decision":decision})))
        }
        "item/permissions/requestApproval" => {
            let permissions = if approve {
                params
                    .get("permissions")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            } else {
                json!({})
            };
            Some((
                if approve { "accept" } else { "cancel" },
                json!({"permissions":permissions,"scope":"turn"}),
            ))
        }
        // These server requests require actual user-provided content or a
        // client-side tool implementation. Full filesystem access must not
        // fabricate an answer for them.
        "item/tool/requestUserInput"
        | "mcpServer/elicitation/request"
        | "item/tool/call"
        | "account/chatgptAuthTokens/refresh"
        | "attestation/generate" => None,
        _ => None,
    }
}

fn codex_automatic_approval_response(
    method: &str,
    params: &Value,
    turn_status: Option<&str>,
    thread_access_mode: Option<ConversationAccessMode>,
) -> Option<(&'static str, Value)> {
    if turn_status.is_some_and(|status| !provider_turn_status_is_active(status)) {
        return codex_approval_response(method, params, false);
    }
    if thread_access_mode == Some(ConversationAccessMode::Full) {
        return codex_approval_response(method, params, true);
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
    fn message(
        text_value: &str,
        attachment_ids: &[String],
        client_message_id: Option<&str>,
        attachments: &[AttachmentRef],
    ) -> Result<()> {
        if text_value.trim().is_empty() && attachments.is_empty() {
            bail!("message requires text or an image")
        }
        if text_value.len() > 256 * 1024 || attachments.len() > 4 {
            bail!("message exceeds the input limit")
        }
        if attachment_ids.len() != attachments.len()
            || attachment_ids
                .iter()
                .zip(attachments)
                .any(|(id, attachment)| id != &attachment.id)
        {
            bail!("attachment references do not match the message")
        }
        if let Some(client_message_id) = client_message_id {
            text("clientMessageId", client_message_id, 128)?;
        }
        for attachment in attachments {
            text("attachmentId", &attachment.id, 128)?;
            text("attachmentName", &attachment.original_name, 180)?;
            if attachment.sha256.len() != 64
                || !attachment
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                bail!("attachment checksum is invalid")
            }
            if !matches!(
                (attachment.mime_type.as_str(), attachment.extension.as_str()),
                ("image/jpeg", "jpg") | ("image/png", "png") | ("image/webp", "webp")
            ) {
                bail!("attachment media type is invalid")
            }
            if attachment.byte_size <= 0 || attachment.byte_size > 20 * 1024 * 1024 {
                bail!("attachment size is invalid")
            }
            let pixels = u64::from(attachment.width) * u64::from(attachment.height);
            if pixels == 0 || pixels > 50_000_000 {
                bail!("attachment dimensions are invalid")
            }
        }
        Ok(())
    }
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
        DeviceCommandKind::ThreadRename { title, .. } => {
            if let Some(title) = title {
                text("title", title, 256)?;
                if title.chars().any(char::is_control) {
                    bail!("title must be a single line without control characters")
                }
            }
        }
        DeviceCommandKind::TurnStart {
            request,
            attachments,
            ..
        } => {
            message(
                &request.text,
                &request.attachment_ids,
                request.client_message_id.as_deref(),
                attachments,
            )?;
            value("options", &request.options, 64 * 1024)?;
        }
        DeviceCommandKind::TurnSteer {
            request,
            attachments,
            ..
        } => {
            message(
                &request.text,
                &request.attachment_ids,
                request.client_message_id.as_deref(),
                attachments,
            )?;
        }
        DeviceCommandKind::ApprovalDecide { request, .. } => {
            if let Some(response) = &request.response {
                value("response", response, 128 * 1024)?;
            }
        }
        DeviceCommandKind::Refresh
        | DeviceCommandKind::ProviderUsageRefresh
        | DeviceCommandKind::ThreadArchive { .. }
        | DeviceCommandKind::ThreadMarkViewed { .. }
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

    #[test]
    fn codex_approval_identity_is_stable_across_transport_requests() {
        let params = json!({
            "threadId":"app_thread",
            "turnId":"app_turn",
            "itemId":"exec_stable",
        });
        let first = codex_approval_id("item/commandExecution/requestApproval", &params);
        let second = codex_approval_id("item/commandExecution/requestApproval", &params);
        assert_eq!(first, second);
        assert_ne!(
            first,
            codex_approval_id("item/fileChange/requestApproval", &params)
        );
    }

    #[test]
    fn codex_full_access_only_auto_answers_permission_approvals() {
        assert_eq!(
            codex_approval_response("item/commandExecution/requestApproval", &json!({}), true,),
            Some(("accept", json!({"decision":"accept"})))
        );
        assert_eq!(
            codex_approval_response("item/tool/requestUserInput", &json!({}), true),
            None
        );
        assert_eq!(
            codex_approval_response("item/commandExecution/requestApproval", &json!({}), false,),
            Some(("cancel", json!({"decision":"cancel"})))
        );
    }

    #[test]
    fn kimi_cursor_ignores_volatile_frames_and_tracks_resync_watermarks() {
        assert!(kimi_message_cursor(&json!({
            "type":"assistant.delta",
            "session_id":"session_1",
            "seq":7,
            "volatile":true,
            "payload":{"delta":"x"}
        }))
        .is_none());
        assert_eq!(
            kimi_message_cursor(&json!({
                "type":"nuntius.resync_required",
                "session_id":"session_1",
                "payload":{"current_seq":9,"epoch":"epoch_2"}
            })),
            Some(("session_1".into(), 9, Some("epoch_2".into())))
        );
    }

    #[test]
    fn kimi_subagent_transcript_events_do_not_mix_into_the_main_turn() {
        assert!(kimi_non_main_transcript_event(
            "assistant.delta",
            &json!({"agentId":"subagent_1","delta":"hidden"})
        ));
        assert!(!kimi_non_main_transcript_event(
            "event.approval.requested",
            &json!({"agentId":"subagent_1"})
        ));
        assert!(!kimi_non_main_transcript_event(
            "assistant.delta",
            &json!({"agentId":"main","delta":"visible"})
        ));
    }

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
    *'"method":"thread/resume"'*'"threadId":"app_active"'*)
      printf '{"id":%s,"result":{"thread":{"id":"app_active","status":{"type":"active"}},"initialTurnsPage":{"data":[{"id":"app_active_turn","status":"inProgress"}]}}}\n' "$id"
      ;;
    *'"method":"thread/resume"'*'"threadId":"app_idle"'*)
      printf '{"id":%s,"result":{"thread":{"id":"app_idle","status":{"type":"idle"}},"initialTurnsPage":{"data":[]}}}\n' "$id"
      ;;
    *'"method":"thread/resume"'*'"threadId":"app_unavailable"'*)
      printf '{"id":%s,"error":{"code":-32000,"message":"provider temporarily unavailable"}}\n' "$id"
      ;;
    *'"method":"thread/resume"'*)
      printf '{"id":%s,"error":{"code":-32600,"message":"no rollout found for thread id unexpected"}}\n' "$id"
      ;;
    *'"method":"thread/read"'*'"threadId":"app_active"'*)
      printf '{"id":%s,"result":{"thread":{"id":"app_active","status":{"type":"active"},"turns":[{"id":"app_active_turn","status":"inProgress","items":[]}]}}}\n' "$id"
      ;;
    *'"method":"thread/read"'*'"threadId":"app_idle"'*)
      printf '{"id":%s,"result":{"thread":{"id":"app_idle","status":{"type":"idle"},"turns":[{"id":"app_idle_turn","status":"completed","items":[]}]}}}\n' "$id"
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
            agents: AgentRuntimes::new_local(Arc::new(config)),
            device_id: "dev_test".into(),
            display_name: Arc::new(RwLock::new("Nuntius Device".into())),
            events,
            command_acks,
            command_notify: Arc::new(Notify::new()),
            history_import_locks: ProviderHistoryLocks::default(),
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
                    attachment_ids: Vec::new(),
                    client_message_id: None,
                    access_mode: ConversationAccessMode::Full,
                    options: json!({}),
                },
                &[],
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

    #[tokio::test]
    async fn recovers_each_running_thread_and_retries_only_unavailable_ones() {
        let temp = TempDir::new().unwrap();
        let (script, calls) = fake_app_server(&temp);
        let executor = executor(&temp, script).await;
        for (local_id, app_id, turn_id) in [
            ("thr_active", "app_active", "app_active_turn"),
            ("thr_idle", "app_idle", "app_idle_turn"),
            ("thr_unavailable", "app_unavailable", "app_unavailable_turn"),
        ] {
            executor
                .store
                .create_thread(local_id, "prj_test", app_id, "Restarted thread", &json!({}))
                .await
                .unwrap();
            executor
                .store
                .mark_app_turn_started(local_id, Some(turn_id))
                .await
                .unwrap();
        }

        let candidates = executor.store.recover_process_state().await.unwrap();
        let pending = executor.recover_threads_once(&candidates).await;
        assert_eq!(pending, vec!["thr_unavailable"]);
        assert_eq!(
            executor.thread("thr_active").await.unwrap().status,
            "active"
        );
        assert_eq!(executor.thread("thr_idle").await.unwrap().status, "idle");
        assert_eq!(
            executor.thread("thr_unavailable").await.unwrap().status,
            "recovering"
        );
        assert_eq!(
            executor
                .store
                .active_app_turn_id("thr_active")
                .await
                .unwrap()
                .as_deref(),
            Some("app_active_turn")
        );
        assert!(
            executor
                .store
                .active_app_turn_id("thr_idle")
                .await
                .unwrap()
                .is_none()
        );
        let calls = std::fs::read_to_string(calls).unwrap();
        for app_id in ["app_active", "app_idle", "app_unavailable"] {
            assert!(calls.contains(&format!("\"threadId\":\"{app_id}\"")));
        }
        executor.agents.shutdown().await.unwrap();
    }

    #[test]
    fn kimi_turn_options_inherit_saved_selection_and_allow_request_overrides() {
        assert_eq!(
            merge_turn_options(
                &json!({
                    "model": "kimi-code/k3",
                    "thinking": "max",
                    "plan_mode": false
                }),
                &json!({"thinking": "high", "plan_mode": true}),
            ),
            json!({
                "model": "kimi-code/k3",
                "thinking": "high",
                "plan_mode": true
            })
        );
    }

    #[test]
    fn current_full_access_bridges_an_active_ask_turn() {
        let params = json!({"permissions":{"network":true}});
        assert_eq!(
            codex_automatic_approval_response(
                "item/permissions/requestApproval",
                &params,
                Some("inProgress"),
                Some(ConversationAccessMode::Full),
            ),
            Some((
                "accept",
                json!({"permissions":{"network":true},"scope":"turn"})
            ))
        );
        assert!(
            codex_automatic_approval_response(
                "item/permissions/requestApproval",
                &params,
                Some("inProgress"),
                Some(ConversationAccessMode::Ask),
            )
            .is_none()
        );
    }

    #[test]
    fn terminal_turn_is_cancelled_even_when_the_thread_is_full_access() {
        assert_eq!(
            codex_automatic_approval_response(
                "item/commandExecution/requestApproval",
                &json!({}),
                Some("completed"),
                Some(ConversationAccessMode::Full),
            ),
            Some(("cancel", json!({"decision":"cancel"})))
        );
    }
}
