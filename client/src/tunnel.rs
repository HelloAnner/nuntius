use crate::{command_queue, directory, executor::CommandExecutor, pairing, protocol::*};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header},
};

const EVENT_IN_FLIGHT_LIMIT: usize = 64;
const HISTORY_IN_FLIGHT_LIMIT: usize = 2;
const OUTBOX_ACK_TIMEOUT: Duration = Duration::from_secs(30);
const TUNNEL_WRITER_CAPACITY: usize = 512;

#[derive(Clone)]
enum OutboxAck {
    Event(String),
    History(String),
}

#[derive(Default)]
struct PendingWindow {
    event_ids: HashMap<String, std::time::Instant>,
    history_ids: HashMap<String, std::time::Instant>,
}

impl PendingWindow {
    fn track_event(&mut self, event_id: &str) -> bool {
        if self.event_ids.len() >= EVENT_IN_FLIGHT_LIMIT || self.event_ids.contains_key(event_id) {
            return false;
        }
        self.event_ids
            .insert(event_id.into(), std::time::Instant::now());
        true
    }

    fn track_history(&mut self, batch_id: &str) -> bool {
        if self.history_ids.len() >= HISTORY_IN_FLIGHT_LIMIT
            || self.history_ids.contains_key(batch_id)
        {
            return false;
        }
        self.history_ids
            .insert(batch_id.into(), std::time::Instant::now());
        true
    }

    fn acknowledge(&mut self, acknowledgement: &OutboxAck) {
        match acknowledgement {
            OutboxAck::Event(event_id) => {
                self.event_ids.remove(event_id);
            }
            OutboxAck::History(batch_id) => {
                self.history_ids.remove(batch_id);
            }
        }
    }

    fn expire_stale(&mut self, now: std::time::Instant) {
        self.event_ids
            .retain(|_, sent_at| now.duration_since(*sent_at) < OUTBOX_ACK_TIMEOUT);
        self.history_ids
            .retain(|_, sent_at| now.duration_since(*sent_at) < OUTBOX_ACK_TIMEOUT);
    }
}

pub async fn run_forever(
    executor: CommandExecutor,
    desired_release: watch::Sender<Option<nuntius_updater::ClientRelease>>,
    connectivity: watch::Sender<bool>,
) {
    let mut backoff = 1_u64;
    let inventory_replay_started = Arc::new(AtomicBool::new(false));
    loop {
        let attempt_started = tokio::time::Instant::now();
        match run_connection(
            executor.clone(),
            &desired_release,
            &connectivity,
            inventory_replay_started.clone(),
        )
        .await
        {
            Ok(()) => tracing::warn!("device tunnel disconnected"),
            Err(error) => tracing::warn!(error=?error,"device tunnel connection failed"),
        };
        connectivity.send_replace(false);
        if attempt_started.elapsed() >= Duration::from_secs(60) {
            backoff = 1;
        }
        let jitter = rand::rng().random_range(0..=1000_u64);
        tokio::time::sleep(Duration::from_millis(backoff * 1000 + jitter)).await;
        backoff = (backoff * 2).min(30)
    }
}

async fn run_connection(
    executor: CommandExecutor,
    desired_release: &watch::Sender<Option<nuntius_updater::ClientRelease>>,
    connectivity: &watch::Sender<bool>,
    inventory_replay_started: Arc<AtomicBool>,
) -> Result<()> {
    let token = pairing::access_token(&executor.config).await?;
    let mut url = pairing::endpoint(&executor.config, "api/v1/device-tunnel")?;
    match url.scheme() {
        "https" => url
            .set_scheme("wss")
            .map_err(|_| anyhow!("cannot create wss URL"))?,
        "http" => url
            .set_scheme("ws")
            .map_err(|_| anyhow!("cannot create ws URL"))?,
        _ => bail!("unsupported server scheme"),
    }
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, format!("Bearer {token}").parse()?);
    request
        .headers_mut()
        .insert(header::SEC_WEBSOCKET_PROTOCOL, DEVICE_SUBPROTOCOL.parse()?);
    let (mut socket, response) =
        tokio::time::timeout(Duration::from_secs(20), connect_async(request))
            .await
            .context("device tunnel connect timed out")??;
    if response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        != Some(DEVICE_SUBPROTOCOL)
    {
        bail!("server did not negotiate {DEVICE_SUBPROTOCOL}")
    }
    let instance_id = executor
        .store
        .state_get("instance_id")
        .await?
        .unwrap_or_else(|| new_id("inst"));
    executor
        .store
        .state_set("instance_id", &instance_id)
        .await?;
    let client_queue_epoch = executor.store.active_command_queue_epoch().await?;
    let last_server_command_seq = executor
        .store
        .last_server_sequence(client_queue_epoch.as_deref())
        .await?;
    send(
        &mut socket,
        &TunnelFrame::Hello {
            protocol_version: DEVICE_PROTOCOL_VERSION,
            device_id: executor.device_id.clone(),
            instance_id,
            agent_version: env!("CARGO_PKG_VERSION").into(),
            transport_security: executor.config.transport_security(),
            last_server_command_seq,
            command_queue_epoch: client_queue_epoch,
            event_acks: BTreeMap::new(),
            history_cursors: BTreeMap::new(),
            capabilities: vec![
                "command-ack.v1".into(),
                "event-ack.v1".into(),
                "history.v1".into(),
                "directory-browser.v1".into(),
                "project-delete.v1".into(),
                DEVICE_DISPLAY_NAME_SYNC_CAPABILITY.into(),
                CLIENT_UPDATE_CAPABILITY.into(),
                "image-input.v1".into(),
                "agent-provider.v1".into(),
                PROVIDER_USAGE_CAPABILITY.into(),
                THREAD_RENAME_CAPABILITY.into(),
                THREAD_VIEW_STATE_CAPABILITY.into(),
                STRICT_VERSION_CAPABILITY.into(),
            ],
        },
    )
    .await?;
    let welcome = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .context("tunnel welcome timed out")?
        .context("tunnel closed before welcome")??;
    let Message::Text(text) = welcome else {
        bail!("server welcome was not text")
    };
    let (server_queue_epoch, display_name, server_version) = match serde_json::from_str::<
        TunnelFrame,
    >(&text)?
    {
        TunnelFrame::Welcome {
            protocol_version,
            transport_security,
            command_queue_epoch,
            display_name,
            server_version,
            ..
        } if protocol_version == DEVICE_PROTOCOL_VERSION
            && transport_security == executor.config.transport_security()
            && product_versions_match(env!("CARGO_PKG_VERSION"), &server_version) =>
        {
            (command_queue_epoch, display_name, server_version)
        }
        TunnelFrame::VersionMismatch {
            client_version,
            server_version,
            release,
        } => {
            executor
                .store
                .state_set("paired_server_version", &server_version)
                .await?;
            executor
                .store
                .state_set("version_compatibility", "mismatch")
                .await?;
            if let Some(release) = release {
                desired_release.send_replace(Some(nuntius_updater::ClientRelease {
                    release_id: release.release_id,
                    product_version: release.product_version,
                    commit_sha: release.commit_sha,
                    release_sequence: release.release_sequence,
                    target: release.target,
                    url: release.url,
                    sha256: release.sha256,
                    size: release.size,
                }));
            }
            bail!(
                "product version mismatch: client {client_version}, server {server_version}; normal tunnel connection refused"
            )
        }
        other => bail!("invalid server welcome: {other:?}"),
    };
    executor
        .store
        .state_set("paired_server_version", &server_version)
        .await?;
    executor
        .store
        .state_set("version_compatibility", "compatible")
        .await?;
    if let Some(display_name) = display_name {
        executor.apply_device_display_name(&display_name).await?;
    }
    executor
        .store
        .state_set("active_command_queue_epoch", &server_queue_epoch)
        .await?;
    tracing::info!(device_id=%executor.device_id,security=?executor.config.transport_security(),"device tunnel connected");
    let (mut socket_sink, mut socket_stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(TUNNEL_WRITER_CAPACITY);
    let (urgent_tx, mut urgent_rx) = mpsc::channel::<Message>(16);
    let mut writer = tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                biased;
                message=urgent_rx.recv()=>message,
                message=out_rx.recv()=>message,
            };
            let Some(message) = message else {
                break;
            };
            tokio::time::timeout(Duration::from_secs(10), socket_sink.send(message))
                .await
                .context("device tunnel send timed out")??;
        }
        Ok::<(), anyhow::Error>(())
    });
    let (window_ack_tx, window_ack_rx) = mpsc::unbounded_channel();
    let (persist_ack_tx, persist_ack_rx) = mpsc::unbounded_channel();
    let outbox_task = tokio::spawn(run_outbox(executor.clone(), out_tx.clone(), window_ack_rx));
    let acknowledgement_task =
        tokio::spawn(persist_acknowledgements(executor.clone(), persist_ack_rx));
    let heartbeat_task = tokio::spawn(send_heartbeats(executor.clone(), urgent_tx.clone()));
    let command_ack_task = tokio::spawn(send_command_acknowledgements(
        executor.clone(),
        out_tx.clone(),
    ));
    let (server_frame_tx, server_frame_rx) = mpsc::unbounded_channel();
    let server_frame_task = tokio::spawn(handle_server_frames(
        executor.clone(),
        out_tx.clone(),
        server_frame_rx,
        desired_release.clone(),
    ));
    let mut last_server_activity = tokio::time::Instant::now();
    spawn_inventory_replay(executor.clone(), inventory_replay_started);
    let connection_result = async {
        loop {
            let watchdog_deadline = last_server_activity + Duration::from_secs(45);
            tokio::select! {
                incoming=socket_stream.next()=>match incoming {
                    Some(Ok(Message::Text(text))) => {
                        last_server_activity = tokio::time::Instant::now();
                        let frame: TunnelFrame = serde_json::from_str(&text)?;
                        match frame {
                            TunnelFrame::EventAck { event_id } => {
                                let acknowledgement = OutboxAck::Event(event_id);
                                window_ack_tx.send(acknowledgement.clone()).map_err(|_| anyhow!("outbox sender closed"))?;
                                persist_ack_tx.send(acknowledgement).map_err(|_| anyhow!("acknowledgement sender closed"))?;
                            }
                            TunnelFrame::HistoryAck { batch_id, .. } => {
                                let acknowledgement = OutboxAck::History(batch_id);
                                window_ack_tx.send(acknowledgement.clone()).map_err(|_| anyhow!("outbox sender closed"))?;
                                persist_ack_tx.send(acknowledgement).map_err(|_| anyhow!("acknowledgement sender closed"))?;
                            }
                            TunnelFrame::HeartbeatAck { .. } => {
                                connectivity.send_replace(true);
                            }
                            frame => {
                                server_frame_tx.send(frame).map_err(|_| anyhow!("server frame handler closed"))?;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        last_server_activity = tokio::time::Instant::now();
                        urgent_tx.try_send(Message::Pong(payload)).map_err(|_| anyhow!("tunnel writer congested while replying to ping"))?;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {}
                },
                result=&mut writer=>match result {
                    Ok(Ok(())) => return Err(anyhow!("device tunnel writer stopped")),
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(anyhow!("device tunnel writer task failed: {error}")),
                },
                _=tokio::time::sleep_until(watchdog_deadline)=>return Err(anyhow!("server heartbeat acknowledgement timed out")),
            }
        }
        Ok(())
    }
    .await;
    outbox_task.abort();
    acknowledgement_task.abort();
    heartbeat_task.abort();
    command_ack_task.abort();
    server_frame_task.abort();
    writer.abort();
    connection_result
}

fn spawn_inventory_replay(executor: CommandExecutor, started: Arc<AtomicBool>) {
    if started.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        let result = async {
            // The durable outboxes are themselves the reconnect replay. Re-emitting
            // every project/thread while they still contain work creates a feedback
            // loop: each short reconnect grows the backlog and delays the next
            // heartbeat. Only synthesize a full inventory for a clean process once.
            if !executor.store.pending_events(1).await?.is_empty()
                || !executor.store.pending_history(1).await?.is_empty()
            {
                tracing::info!("durable sync backlog present; using it as reconnect inventory");
                return Ok::<(), anyhow::Error>(());
            }
            executor.emit_inventory().await
        }
        .await;
        match result {
            Ok(()) => tracing::info!("initial inventory replay completed without blocking tunnel"),
            Err(error) => {
                started.store(false, Ordering::Release);
                tracing::warn!(error=?error,"initial inventory replay failed; will retry after reconnect");
            }
        }
    });
}

async fn handle_server_frame(
    executor: &CommandExecutor,
    out: &mpsc::Sender<Message>,
    frame: TunnelFrame,
    desired_release: &watch::Sender<Option<nuntius_updater::ClientRelease>>,
) -> Result<()> {
    match frame {
        TunnelFrame::Command {
            queue_epoch,
            server_sequence,
            command,
        } => {
            if command.device_id != executor.device_id {
                bail!("command targets a different device")
            }
            let target = command_queue::target_key(&command);
            let priority = command_queue::priority(&command);
            let inbox = executor
                .store
                .receive_command(&queue_epoch, server_sequence, &target, priority, &command)
                .await?;
            let acknowledgement = match inbox.status.as_str() {
                "completed" | "failed" | "unknown" | "expired" => terminal_ack(&inbox),
                "applying" => TunnelFrame::CommandAck {
                    command_id: command.command_id,
                    stage: "applying".into(),
                    result: None,
                    error_code: None,
                    error_message: None,
                },
                _ => {
                    executor.command_notify.notify_one();
                    TunnelFrame::CommandAck {
                        command_id: command.command_id,
                        stage: "persisted".into(),
                        result: None,
                        error_code: None,
                        error_message: None,
                    }
                }
            };
            queue_frame(out, &acknowledgement).await?;
        }
        TunnelFrame::Query {
            correlation_id,
            query,
        } => {
            let executor = executor.clone();
            let out = out.clone();
            tokio::spawn(async move {
                let response = execute_query(&executor, query).await;
                let (result, error_code) = match response {
                    Ok(value) => (Some(value), None),
                    Err(error) => (None, Some(error.to_string())),
                };
                let _ = queue_frame(
                    &out,
                    &TunnelFrame::QueryResponse {
                        correlation_id,
                        result,
                        error_code,
                    },
                )
                .await;
            });
        }
        TunnelFrame::EventAck { .. } | TunnelFrame::HistoryAck { .. } => {
            bail!("outbox acknowledgement bypassed tunnel reader")
        }
        TunnelFrame::HeartbeatAck { .. } => {}
        TunnelFrame::DeviceConfig { display_name } => {
            executor.apply_device_display_name(&display_name).await?
        }
        TunnelFrame::ClientUpdate { release } => {
            tracing::info!(release_id=%release.release_id,commit_sha=%release.commit_sha,release_sequence=release.release_sequence,"desired client release received");
            desired_release.send_replace(Some(nuntius_updater::ClientRelease {
                release_id: release.release_id,
                product_version: release.product_version,
                commit_sha: release.commit_sha,
                release_sequence: release.release_sequence,
                target: release.target,
                url: release.url,
                sha256: release.sha256,
                size: release.size,
            }));
        }
        TunnelFrame::ServerNotice { code, message } => {
            if code == "update_available" {
                tracing::info!(%message, "legacy release notification received; waiting for structured client release");
            } else {
                tracing::warn!(%code,%message,"server notice")
            }
        }
        TunnelFrame::Welcome { .. }
        | TunnelFrame::VersionMismatch { .. }
        | TunnelFrame::Hello { .. }
        | TunnelFrame::CommandAck { .. }
        | TunnelFrame::Event { .. }
        | TunnelFrame::HistoryBatch { .. }
        | TunnelFrame::QueryResponse { .. }
        | TunnelFrame::Heartbeat { .. } => bail!("frame not allowed from server"),
    }
    Ok(())
}

async fn handle_server_frames(
    executor: CommandExecutor,
    out: mpsc::Sender<Message>,
    mut frames: mpsc::UnboundedReceiver<TunnelFrame>,
    desired_release: watch::Sender<Option<nuntius_updater::ClientRelease>>,
) {
    while let Some(frame) = frames.recv().await {
        if let Err(error) = handle_server_frame(&executor, &out, frame, &desired_release).await {
            tracing::warn!(error=?error, "server frame handling failed");
        }
    }
}

async fn send_command_acknowledgements(executor: CommandExecutor, out: mpsc::Sender<Message>) {
    let mut command_acks = executor.command_acks.subscribe();
    loop {
        match command_acks.recv().await {
            Ok(frame) => {
                let command_id = match &frame {
                    TunnelFrame::CommandAck { command_id, .. } => command_id,
                    _ => continue,
                };
                match executor.store.inbox(command_id).await {
                    Ok(Some(record)) if record.queue_epoch != "local" => {
                        if queue_frame(&out, &frame).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(error=?error, %command_id, "command acknowledgement lookup failed");
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "command acknowledgement sender lagged");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn run_outbox(
    executor: CommandExecutor,
    out: mpsc::Sender<Message>,
    mut acknowledgements: mpsc::UnboundedReceiver<OutboxAck>,
) {
    let mut events = executor.events.subscribe();
    let mut pending_window = PendingWindow::default();
    // History is already durable in the local outbox. Flush it quickly so a
    // browser watching the public server sees external terminal activity with
    // sub-second-to-low-second latency, without coupling delivery to browser state.
    let mut flush = tokio::time::interval(Duration::from_secs(1));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            acknowledgement=acknowledgements.recv()=>match acknowledgement {
                Some(acknowledgement) => pending_window.acknowledge(&acknowledgement),
                None => break,
            },
            event=events.recv()=>match event {
                Ok(event) => {
                    if pending_window.track_event(&event.event_id)
                        && queue_frame(&out, &TunnelFrame::Event { event }).await.is_err()
                    {
                        break;
                    }
                }
                // The durable timer replay below recovers every lagged event.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _=flush.tick()=>{
                // The server intentionally leaves rejected durable data
                // unacknowledged. Release timed-out slots so transient SQLite
                // contention cannot freeze replay until the next reconnect.
                pending_window.expire_stale(std::time::Instant::now());
                if let Err(error) = queue_pending(&executor, &out, &mut pending_window).await {
                    tracing::warn!(error=?error, "durable tunnel outbox flush failed; will retry");
                }
            },
        }
    }
}

async fn persist_acknowledgements(
    executor: CommandExecutor,
    mut acknowledgements: mpsc::UnboundedReceiver<OutboxAck>,
) {
    while let Some(first) = acknowledgements.recv().await {
        let mut event_ids = Vec::new();
        let mut history_ids = Vec::new();
        push_acknowledgement(first, &mut event_ids, &mut history_ids);
        tokio::time::sleep(Duration::from_millis(25)).await;
        while event_ids.len() + history_ids.len() < 256 {
            match acknowledgements.try_recv() {
                Ok(acknowledgement) => {
                    push_acknowledgement(acknowledgement, &mut event_ids, &mut history_ids)
                }
                Err(_) => break,
            }
        }
        let history_changed = !history_ids.is_empty();
        if let Err(error) = executor
            .store
            .ack_outbox_batch(&event_ids, &history_ids)
            .await
        {
            // Leaving an acknowledged row durable is safe: the next flush sends
            // it again and the idempotent server acknowledges it again.
            tracing::warn!(error=?error,event_count=event_ids.len(),history_count=history_ids.len(),"durable tunnel acknowledgements failed; rows remain replayable");
            continue;
        }
        if history_changed {
            if let Err(error) = executor.maybe_emit_inventory_complete().await {
                tracing::warn!(error=?error, "history inventory completion check failed");
            }
        }
    }
}

fn push_acknowledgement(
    acknowledgement: OutboxAck,
    event_ids: &mut Vec<String>,
    history_ids: &mut Vec<String>,
) {
    match acknowledgement {
        OutboxAck::Event(event_id) => event_ids.push(event_id),
        OutboxAck::History(batch_id) => history_ids.push(batch_id),
    }
}

async fn send_heartbeats(executor: CommandExecutor, out: mpsc::Sender<Message>) {
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut health = unavailable_health();
    loop {
        heartbeat.tick().await;
        if queue_frame(
            &out,
            &TunnelFrame::Heartbeat {
                sent_at: now(),
                health: health.clone(),
            },
        )
        .await
        .is_err()
        {
            break;
        }
        match tokio::time::timeout(Duration::from_secs(5), collect_health(&executor)).await {
            Ok(Ok(current)) => health = current,
            Ok(Err(error)) => {
                health.storage_status = "degraded".into();
                tracing::warn!(error=?error, "device health refresh failed; heartbeat remains independent");
            }
            Err(_) => {
                health.storage_status = "busy".into();
                tracing::warn!("device health refresh timed out; heartbeat remains independent");
            }
        }
    }
}

fn unavailable_health() -> DeviceHealth {
    DeviceHealth {
        app_server_status: "unavailable".into(),
        storage_status: "starting".into(),
        inbox_depth: 0,
        outbox_depth: 0,
        history_backfill_depth: 0,
        active_turn_count: 0,
        pending_approval_count: 0,
        project_count: 0,
        codex_version: None,
        providers: Vec::new(),
    }
}

async fn collect_health(executor: &CommandExecutor) -> Result<DeviceHealth> {
    let (project_count, inbox_depth, outbox_depth, active_turn_count) =
        executor.store.counts().await?;
    let history_backfill_depth = executor.store.pending_history_count().await?;
    let pending_approval_count = executor.store.pending_approval_count().await?;
    let providers = executor.agents.statuses().await;
    let codex = providers
        .iter()
        .find(|status| status.provider == AgentProvider::Codex);
    Ok(DeviceHealth {
        app_server_status: codex
            .map(|status| status.status.clone())
            .unwrap_or_else(|| "unavailable".into()),
        storage_status: "ok".into(),
        inbox_depth,
        outbox_depth,
        history_backfill_depth,
        active_turn_count,
        pending_approval_count,
        project_count,
        codex_version: codex.and_then(|status| status.version.clone()),
        providers,
    })
}
fn terminal_ack(inbox: &crate::store::InboxRecord) -> TunnelFrame {
    TunnelFrame::CommandAck {
        command_id: inbox.command.command_id.clone(),
        stage: inbox.status.clone(),
        result: inbox.result.clone(),
        error_code: inbox.error_code.clone(),
        error_message: inbox.error_message.clone(),
    }
}
async fn execute_query(executor: &CommandExecutor, query: DeviceQuery) -> Result<Value> {
    match query {
        DeviceQuery::DirectoryRoots => Ok(serde_json::to_value(
            directory::roots(&executor.config, &executor.store, &executor.device_id).await?,
        )?),
        DeviceQuery::DirectoryList { parent_ref, cursor } => Ok(serde_json::to_value(
            directory::list(
                &executor.config,
                &executor.store,
                &executor.device_id,
                &parent_ref,
                cursor.as_deref(),
            )
            .await?,
        )?),
        DeviceQuery::Snapshot => Ok(
            json!({"projects":executor.store.list_projects(&executor.device_id).await?,"threads":executor.store.list_threads_page(&executor.device_id,None,500,0).await?}),
        ),
    }
}
async fn queue_pending(
    executor: &CommandExecutor,
    out: &mpsc::Sender<Message>,
    pending_window: &mut PendingWindow,
) -> Result<()> {
    for event in executor
        .store
        .pending_events(EVENT_IN_FLIGHT_LIMIT as i64)
        .await?
    {
        if pending_window.track_event(&event.event_id) {
            queue_frame(out, &TunnelFrame::Event { event }).await?;
        }
    }
    for batch in executor
        .store
        .pending_history(HISTORY_IN_FLIGHT_LIMIT as i64)
        .await?
    {
        if pending_window.track_history(&batch.batch_id) {
            queue_frame(out, &TunnelFrame::HistoryBatch { batch }).await?;
        }
    }
    Ok(())
}

fn product_versions_match(client_version: &str, server_version: &str) -> bool {
    client_version == server_version
}

async fn queue_frame(out: &mpsc::Sender<Message>, frame: &TunnelFrame) -> Result<()> {
    out.send(Message::Text(serde_json::to_string(frame)?.into()))
        .await
        .map_err(|_| anyhow!("device tunnel writer closed"))
}

async fn send<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    frame: &TunnelFrame,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(
        Duration::from_secs(10),
        socket.send(Message::Text(serde_json::to_string(frame)?.into())),
    )
    .await
    .context("device tunnel send timed out")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_window_deduplicates_until_acknowledged() {
        let mut window = PendingWindow::default();
        assert!(window.track_event("evt_test"));
        assert!(!window.track_event("evt_test"));
        window.acknowledge(&OutboxAck::Event("evt_test".into()));
        assert!(window.track_event("evt_test"));

        assert!(window.track_history("hbatch_test"));
        assert!(!window.track_history("hbatch_test"));
        window.acknowledge(&OutboxAck::History("hbatch_test".into()));
        assert!(window.track_history("hbatch_test"));
    }

    #[test]
    fn pending_window_retries_unacknowledged_data_after_timeout() {
        let mut window = PendingWindow::default();
        let now = std::time::Instant::now();
        let expired = now.checked_sub(OUTBOX_ACK_TIMEOUT).unwrap();
        window.event_ids.insert("evt_retry".into(), expired);
        window.history_ids.insert("hbatch_retry".into(), expired);

        window.expire_stale(now);

        assert!(window.track_event("evt_retry"));
        assert!(window.track_history("hbatch_retry"));
    }

    #[test]
    fn pending_window_enforces_bounded_replay() {
        let mut window = PendingWindow::default();
        for index in 0..EVENT_IN_FLIGHT_LIMIT {
            assert!(window.track_event(&format!("evt_{index}")));
        }
        assert!(!window.track_event("evt_overflow"));

        for index in 0..HISTORY_IN_FLIGHT_LIMIT {
            assert!(window.track_history(&format!("hbatch_{index}")));
        }
        assert!(!window.track_history("hbatch_overflow"));
    }

    #[test]
    fn product_versions_require_exact_match() {
        assert!(product_versions_match("0.0.1", "0.0.1"));
        assert!(!product_versions_match("0.0.1", "0.0.2"));
        assert!(!product_versions_match("0.0.1", "0.1.1"));
    }
}
