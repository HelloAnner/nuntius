use crate::{
    app_server::AppServerRuntime, config::ClientConfig, directory, protocol::*, store::ClientStore,
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
    pub app: AppServerRuntime,
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
            DeviceCommandKind::ThreadCreate {
                project_id,
                request,
            } => self.create_thread(project_id, request).await,
            DeviceCommandKind::ThreadArchive {
                thread_id,
                archived,
            } => {
                let thread = self.command_thread(thread_id).await?;
                let method = if *archived {
                    "thread/archive"
                } else {
                    "thread/unarchive"
                };
                let result = self
                    .app
                    .call(method, json!({"threadId":thread.app_server_thread_id}))
                    .await?;
                self.store.set_thread_archived(thread_id, *archived).await?;
                self.sync_thread(thread_id).await?;
                Ok(result)
            }
            DeviceCommandKind::TurnStart { thread_id, request } => {
                self.start_turn(thread_id, request).await
            }
            DeviceCommandKind::TurnSteer { thread_id, request } => {
                let thread = self.command_thread(thread_id).await?;
                let state = self.resume_app_thread(&thread).await?;
                let app_turn = state.active_turn_id.context("no active turn to steer")?;
                self.app.call("turn/steer",json!({"threadId":thread.app_server_thread_id,"expectedTurnId":app_turn,"input":[{"type":"text","text":request.text}]})).await
            }
            DeviceCommandKind::TurnInterrupt { thread_id } => {
                let thread = self.command_thread(thread_id).await?;
                let state = self.resume_app_thread(&thread).await?;
                let Some(app_turn) = state.active_turn_id else {
                    if state.status == "active" {
                        bail!("active App Server turn identity is unavailable")
                    }
                    self.store.touch_thread(thread_id, "idle").await?;
                    return Ok(json!({"alreadyTerminal":true}));
                };
                let result = self
                    .app
                    .call(
                        "turn/interrupt",
                        json!({"threadId":thread.app_server_thread_id,"turnId":app_turn}),
                    )
                    .await?;
                self.store.touch_thread(thread_id, "interrupted").await?;
                self.sync_thread(thread_id).await?;
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
                let (request_id, _method) = self
                    .store
                    .claim_app_request(approval_id)
                    .await?
                    .context("approval is missing or already decided")?;
                let app_decision = if request.decision == "accept_for_session" {
                    "acceptForSession"
                } else {
                    request.decision.as_str()
                };
                let response = request
                    .response
                    .clone()
                    .unwrap_or_else(|| json!({"decision":app_decision}));
                if let Err(error) = self.app.respond(request_id, response).await {
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

    async fn create_thread(
        &self,
        project_id: &str,
        request: &CreateThreadRequest,
    ) -> Result<Value> {
        let project = self
            .store
            .project(project_id, &self.device_id)
            .await?
            .context("project not found")?;
        let mut params = object(project.defaults.clone());
        params.extend(object(request.options.clone()));
        params.insert("cwd".into(), json!(project.canonical_path));
        let result = self.app.call("thread/start", Value::Object(params)).await?;
        let app_id = extract_id(&result, &["thread/id", "threadId", "id"])
            .context("thread/start response has no thread id")?;
        let id = new_id("thr");
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
        self.store
            .create_thread(&id, project_id, &app_id, &title)
            .await?;
        self.sync_thread(&id).await?;
        if let Some(text) = request
            .first_message
            .as_ref()
            .filter(|v| !v.trim().is_empty())
        {
            let start = StartTurnRequest {
                text: text.clone(),
                options: Value::Object(Map::new()),
            };
            let _ = self.start_turn(&id, &start).await?;
        }
        let thread = self.thread(&id).await?;
        Ok(json!({"threadId":id,"appServerThreadId":app_id,"thread":thread}))
    }

    async fn start_turn(&self, thread_id: &str, request: &StartTurnRequest) -> Result<Value> {
        if request.text.trim().is_empty() {
            bail!("turn text cannot be empty")
        };
        let thread = self.command_thread(thread_id).await?;
        let state = self.resume_app_thread(&thread).await?;
        if let Some(app_turn) = state.active_turn_id {
            let result = self
                .app
                .call(
                    "turn/steer",
                    json!({
                        "threadId":thread.app_server_thread_id,
                        "expectedTurnId":app_turn,
                        "input":[{"type":"text","text":request.text}]
                    }),
                )
                .await?;
            return Ok(
                json!({"operation":"steer","appServerTurnId":app_turn,"appServerResult":result}),
            );
        }
        if state.status == "active" {
            bail!("active App Server turn identity is unavailable")
        }
        if state.status == "systemError" || state.status == "notLoaded" {
            bail!(
                "App Server thread cannot accept input while status is {}",
                state.status
            )
        }
        let mut params = object(request.options.clone());
        params.insert("threadId".into(), json!(thread.app_server_thread_id));
        params.insert("input".into(), json!([{"type":"text","text":request.text}]));
        let result = self.app.call("turn/start", Value::Object(params)).await?;
        let app_turn = extract_id(&result, &["turn/id", "turnId", "id"]);
        let local_turn = self
            .store
            .record_user_turn(thread_id, app_turn.as_deref(), &request.text)
            .await?;
        self.sync_thread(thread_id).await?;
        self.emit(
            "turn.started",
            Some(&thread.project_id),
            Some(thread_id),
            Some(&local_turn),
            json!({"text":request.text,"appServerResult":result}),
            true,
        )
        .await?;
        Ok(json!({"operation":"start","turnId":local_turn,"appServerResult":result}))
    }

    async fn resume_app_thread(&self, thread: &ThreadSummary) -> Result<ResumedThreadState> {
        let app_thread_id = thread
            .app_server_thread_id
            .as_deref()
            .context("thread has no App Server id")?;
        let result = self
            .app
            .call(
                "thread/resume",
                json!({
                    "threadId": app_thread_id,
                    "initialTurnsPage": {
                        "limit": 1,
                        "sortDirection": "desc",
                        "itemsView": "notLoaded"
                    }
                }),
            )
            .await?;
        let app_thread = result.get("thread").unwrap_or(&result);
        let status = app_thread_status(app_thread).to_owned();
        let mut active_turn_id = result
            .pointer("/initialTurnsPage/data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(
                app_thread
                    .get("turns")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
            .find(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if active_turn_id.is_none() && status == "active" {
            active_turn_id = self.store.active_app_turn_id(&thread.id).await?;
        }
        Ok(ResumedThreadState {
            status,
            active_turn_id,
        })
    }

    async fn thread(&self, id: &str) -> Result<ThreadSummary> {
        self.store
            .thread(id, &self.device_id)
            .await?
            .context("thread not found")
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
            .context("thread has no App Server id")?;
        let response = self
            .app
            .call_with_timeout(
                "thread/read",
                json!({"threadId":app_thread_id,"includeTurns":true}),
                std::time::Duration::from_secs(180),
            )
            .await?;
        let app_thread = response.get("thread").unwrap_or(&response);
        self.store.import_app_history(thread_id, app_thread).await?;
        self.sync_thread(thread_id).await?;
        self.store
            .state_set(
                &thread_fingerprint_key(app_thread_id),
                &thread_fingerprint(app_thread)?,
            )
            .await
    }

    /// Reconcile the most recently changed Codex sessions, including sessions
    /// created by a different CLI/App Server process on this workstation.
    pub async fn reconcile_recent(&self, archived: bool) -> Result<usize> {
        let response = self
            .app
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
            let fingerprint = thread_fingerprint(app_thread)?;
            let key = thread_fingerprint_key(app_id);
            let missing = self.store.local_thread_id(app_id).await?.is_none();
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

    /// Force a single App Server thread into the local durable history outbox.
    /// Used by the rollout-file monitor so external terminal activity does not
    /// depend on this App Server instance receiving runtime notifications.
    pub async fn reconcile_app_thread(&self, app_id: &str) -> Result<()> {
        let _guard = self.history_import_lock.lock().await;
        let response = self
            .app
            .call_with_timeout(
                "thread/read",
                json!({"threadId":app_id,"includeTurns":true}),
                std::time::Duration::from_secs(180),
            )
            .await
            .with_context(|| format!("cannot read changed Codex thread {app_id}"))?;
        let app_thread = response.get("thread").unwrap_or(&response);
        let project_id = if let Some(local_id) = self.store.local_thread_id(app_id).await? {
            self.thread(&local_id).await?.project_id
        } else {
            self.project_for_app_thread(None, app_thread).await?
        };
        let local_thread = self
            .store
            .import_app_thread(&project_id, app_thread)
            .await?;
        self.store
            .import_app_history(&local_thread, app_thread)
            .await?;
        self.sync_thread(&local_thread).await?;
        self.store
            .state_set(
                &thread_fingerprint_key(app_id),
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
        Ok(())
    }
    pub async fn discover_project(&self, project_id: &str) -> Result<usize> {
        let project = self
            .store
            .project(project_id, &self.device_id)
            .await?
            .context("project not found")?;
        self.discover_pages(Some(project_id), Some(&project.canonical_path), false)
            .await
    }
    pub async fn discover_all(&self) -> Result<usize> {
        self.store
            .state_set("history_discovery_complete", "false")
            .await?;
        self.store
            .state_set("history_completion_announced", "false")
            .await?;
        let mut imported = self.discover_pages(None, None, false).await?;
        imported += self.discover_pages(None, None, true).await?;
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
        fixed_project_id: Option<&str>,
        cwd: Option<&std::path::Path>,
        archived: bool,
    ) -> Result<usize> {
        let unassigned_project = self
            .store
            .ensure_unassigned_project(&self.device_id)
            .await?;
        let mut cursor: Option<String> = None;
        let mut seen_cursors = std::collections::HashSet::new();
        let mut imported = 0_usize;
        for _ in 0..100 {
            let mut params = json!({"limit":100,"archived":archived,"cursor":cursor});
            if let Some(path) = cwd {
                params["cwd"] = json!(path);
            }
            let response = self.app.call("thread/list", params).await?;
            let threads = response
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for app_thread in threads {
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
                let was_missing = self.store.local_thread_id(app_id).await?.is_none();
                let local_thread = self
                    .store
                    .import_app_thread(&project_id, &app_thread)
                    .await?;
                let fingerprint = thread_fingerprint(&app_thread)?;
                let key = thread_fingerprint_key(app_id);
                let active = app_thread_status(&app_thread) == "active";
                if was_missing
                    || active
                    || self.store.state_get(&key).await?.as_deref() != Some(&fingerprint)
                {
                    match self
                        .import_and_sync_thread(&local_thread, &app_thread)
                        .await
                    {
                        Ok(()) => {
                            self.store.state_set(&key, &fingerprint).await?;
                            imported += 1;
                        }
                        Err(error) => {
                            tracing::warn!(%app_id,error=?error,"historical Codex thread import failed; continuing discovery");
                        }
                    }
                }
            }
            cursor = response
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(next) = &cursor
                && !seen_cursors.insert(next.clone())
            {
                bail!("thread/list returned a repeated cursor")
            }
            if cursor.is_none() {
                break;
            }
        }
        if cursor.is_some() {
            bail!("thread/list pagination exceeded the 10,000-thread safety limit")
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

    async fn import_and_sync_thread(&self, local_thread: &str, app_thread: &Value) -> Result<()> {
        let _guard = self.history_import_lock.lock().await;
        let app_id = app_thread
            .get("id")
            .and_then(Value::as_str)
            .context("listed thread has no id")?;
        let detail = self
            .app
            .call_with_timeout(
                "thread/read",
                json!({"threadId":app_id,"includeTurns":true}),
                std::time::Duration::from_secs(180),
            )
            .await
            .with_context(|| format!("cannot read historical thread {app_id}"))?;
        let detail = detail.get("thread").unwrap_or(&detail);
        self.store.import_app_history(local_thread, detail).await?;
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
    let mut receiver = executor.app.subscribe();
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
        executor.store.local_thread_id(id).await?
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
            .save_app_request(&approval_id, request_id, method, &event_params)
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
    if method == "turn/completed"
        && let Some(local_thread) = thread_id.as_deref()
    {
        let app_turn = find_string(&params, &["turnId", "turn/id"]);
        let status =
            find_string(&params, &["turn/status", "status"]).unwrap_or_else(|| "completed".into());
        executor
            .store
            .complete_app_turn(local_thread, app_turn.as_deref(), &status)
            .await?;
        executor.refresh_thread_history(local_thread).await?;
    }
    Ok(())
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
struct ResumedThreadState {
    status: String,
    active_turn_id: Option<String>,
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
fn thread_fingerprint_key(app_thread_id: &str) -> String {
    format!("app_thread_fingerprint:{app_thread_id}")
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
