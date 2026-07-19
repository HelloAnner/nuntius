use crate::{command_queue, directory, executor::CommandExecutor, pairing, protocol::*};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header},
};

const EVENT_IN_FLIGHT_LIMIT: usize = 64;
const HISTORY_IN_FLIGHT_LIMIT: usize = 16;

#[derive(Default)]
struct PendingWindow {
    event_ids: HashSet<String>,
    history_ids: HashSet<String>,
}

impl PendingWindow {
    fn track_event(&mut self, event_id: &str) -> bool {
        if self.event_ids.len() >= EVENT_IN_FLIGHT_LIMIT {
            return false;
        }
        self.event_ids.insert(event_id.into())
    }

    fn track_history(&mut self, batch_id: &str) -> bool {
        if self.history_ids.len() >= HISTORY_IN_FLIGHT_LIMIT {
            return false;
        }
        self.history_ids.insert(batch_id.into())
    }

    fn acknowledge(&mut self, frame: &TunnelFrame) {
        match frame {
            TunnelFrame::EventAck { event_id } => {
                self.event_ids.remove(event_id);
            }
            TunnelFrame::HistoryAck { batch_id, .. } => {
                self.history_ids.remove(batch_id);
            }
            _ => {}
        }
    }
}

pub async fn run_forever(
    executor: CommandExecutor,
    update_trigger: nuntius_updater::UpdateTrigger,
) {
    let mut backoff = 1_u64;
    loop {
        let attempt_started = tokio::time::Instant::now();
        match run_connection(executor.clone(), &update_trigger).await {
            Ok(()) => tracing::warn!("device tunnel disconnected"),
            Err(error) => tracing::warn!(error=?error,"device tunnel connection failed"),
        };
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
    update_trigger: &nuntius_updater::UpdateTrigger,
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
                "image-input.v1".into(),
                "agent-provider.v1".into(),
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
    let (server_queue_epoch, display_name) = match serde_json::from_str::<TunnelFrame>(&text)? {
        TunnelFrame::Welcome {
            protocol_version,
            transport_security,
            command_queue_epoch,
            display_name,
            ..
        } if protocol_version == DEVICE_PROTOCOL_VERSION
            && transport_security == executor.config.transport_security() =>
        {
            (command_queue_epoch, display_name)
        }
        other => bail!("invalid server welcome: {other:?}"),
    };
    if let Some(display_name) = display_name {
        executor.apply_device_display_name(&display_name).await?;
    }
    executor
        .store
        .state_set("active_command_queue_epoch", &server_queue_epoch)
        .await?;
    tracing::info!(device_id=%executor.device_id,security=?executor.config.transport_security(),"device tunnel connected");
    let (out_tx, mut out_rx) = mpsc::channel::<TunnelFrame>(512);
    let mut events = executor.events.subscribe();
    let mut command_acks = executor.command_acks.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // History is already durable in the local outbox. Flush it quickly so a
    // browser watching the public server sees external terminal activity with
    // sub-second-to-low-second latency, without coupling delivery to browser state.
    let mut flush = tokio::time::interval(Duration::from_secs(1));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_server_activity = tokio::time::Instant::now();
    let mut pending_window = PendingWindow::default();
    send_pending(&executor, &mut socket, &mut pending_window).await?;
    executor.emit_inventory().await?;
    loop {
        let watchdog_deadline = last_server_activity + Duration::from_secs(45);
        tokio::select! {
            incoming=socket.next()=>match incoming{Some(Ok(Message::Text(text)))=>{last_server_activity=tokio::time::Instant::now();let frame: TunnelFrame=serde_json::from_str(&text)?;pending_window.acknowledge(&frame);handle_server_frame(&executor,&out_tx,frame,update_trigger).await?},Some(Ok(Message::Ping(payload)))=>{last_server_activity=tokio::time::Instant::now();socket.send(Message::Pong(payload)).await?;},Some(Ok(Message::Close(_)))|None=>break,Some(Err(error))=>return Err(error.into()),_=>{}},
            Some(frame)=out_rx.recv()=>send(&mut socket,&frame).await?,
            event=events.recv()=>match event{Ok(event)=>{if pending_window.track_event(&event.event_id){send(&mut socket,&TunnelFrame::Event{event}).await?}},Err(broadcast::error::RecvError::Lagged(_))=>send_pending(&executor,&mut socket,&mut pending_window).await?,Err(broadcast::error::RecvError::Closed)=>break},
            ack=command_acks.recv()=>match ack{
                Ok(frame) => {
                    let command_id = match &frame {
                        TunnelFrame::CommandAck { command_id, .. } => command_id,
                        _ => continue,
                    };
                    if executor.store.inbox(command_id).await?.is_some_and(|record| record.queue_epoch != "local") {
                        send(&mut socket,&frame).await?;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _=heartbeat.tick()=>{let(project_count,inbox_depth,outbox_depth,active)=executor.store.counts().await?;let providers=executor.agents.statuses().await;let codex=providers.iter().find(|status|status.provider==AgentProvider::Codex);let health=DeviceHealth{app_server_status:codex.map(|status|status.status.clone()).unwrap_or_else(||"unavailable".into()),storage_status:"ok".into(),inbox_depth,outbox_depth,history_backfill_depth:executor.store.pending_history(1000).await?.len() as i64,active_turn_count:active,pending_approval_count:executor.store.pending_approval_count().await?,project_count,codex_version:codex.and_then(|status|status.version.clone()),providers};send(&mut socket,&TunnelFrame::Heartbeat{sent_at:now(),health}).await?;},
            _=flush.tick()=>send_pending(&executor,&mut socket,&mut pending_window).await?,
            _=tokio::time::sleep_until(watchdog_deadline)=>return Err(anyhow!("server heartbeat acknowledgement timed out")),
        }
    }
    Ok(())
}

async fn handle_server_frame(
    executor: &CommandExecutor,
    out: &mpsc::Sender<TunnelFrame>,
    frame: TunnelFrame,
    update_trigger: &nuntius_updater::UpdateTrigger,
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
            out.send(acknowledgement)
                .await
                .map_err(|_| anyhow!("tunnel sender closed"))?;
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
                let _ = out
                    .send(TunnelFrame::QueryResponse {
                        correlation_id,
                        result,
                        error_code,
                    })
                    .await;
            });
        }
        TunnelFrame::EventAck { event_id } => executor.store.ack_event(&event_id).await?,
        TunnelFrame::HistoryAck { batch_id, .. } => {
            executor.store.ack_history(&batch_id).await?;
            executor.maybe_emit_inventory_complete().await?;
        }
        TunnelFrame::HeartbeatAck { .. } => {}
        TunnelFrame::DeviceConfig { display_name } => {
            executor.apply_device_display_name(&display_name).await?
        }
        TunnelFrame::ServerNotice { code, message } => {
            if code == "update_available" {
                tracing::info!(%message, "release notification received from server");
                update_trigger.notify();
            } else {
                tracing::warn!(%code,%message,"server notice")
            }
        }
        TunnelFrame::Welcome { .. }
        | TunnelFrame::Hello { .. }
        | TunnelFrame::CommandAck { .. }
        | TunnelFrame::Event { .. }
        | TunnelFrame::HistoryBatch { .. }
        | TunnelFrame::QueryResponse { .. }
        | TunnelFrame::Heartbeat { .. } => bail!("frame not allowed from server"),
    }
    Ok(())
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
async fn send_pending<S>(
    executor: &CommandExecutor,
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    pending_window: &mut PendingWindow,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    for event in executor
        .store
        .pending_events(EVENT_IN_FLIGHT_LIMIT as i64)
        .await?
    {
        if pending_window.track_event(&event.event_id) {
            send(socket, &TunnelFrame::Event { event }).await?;
        }
    }
    for batch in executor
        .store
        .pending_history(HISTORY_IN_FLIGHT_LIMIT as i64)
        .await?
    {
        if pending_window.track_history(&batch.batch_id) {
            send(socket, &TunnelFrame::HistoryBatch { batch }).await?;
        }
    }
    Ok(())
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
        window.acknowledge(&TunnelFrame::EventAck {
            event_id: "evt_test".into(),
        });
        assert!(window.track_event("evt_test"));

        assert!(window.track_history("hbatch_test"));
        assert!(!window.track_history("hbatch_test"));
        window.acknowledge(&TunnelFrame::HistoryAck {
            batch_id: "hbatch_test".into(),
            thread_id: "thr_test".into(),
            acked_cursor: "hist_test".into(),
        });
        assert!(window.track_history("hbatch_test"));
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
}
