use crate::{AppState, event_hub::PublishedEvent, protocol::*};
use anyhow::{Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};

struct Connection {
    epoch: i64,
    sender: mpsc::Sender<TunnelFrame>,
    supersede: Option<oneshot::Sender<()>>,
    capabilities: HashSet<String>,
    ready: bool,
    pending_display_name: Option<String>,
}

#[derive(Default)]
pub struct TunnelRegistry {
    connections: RwLock<HashMap<String, Connection>>,
    pending_queries: Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    next_epoch: AtomicI64,
}

impl TunnelRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_epoch: AtomicI64::new(1),
            ..Default::default()
        })
    }

    async fn register(
        &self,
        device_id: &str,
        sender: mpsc::Sender<TunnelFrame>,
        supersede: oneshot::Sender<()>,
        capabilities: HashSet<String>,
    ) -> i64 {
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
        let previous = self.connections.write().await.insert(
            device_id.into(),
            Connection {
                epoch,
                sender,
                supersede: Some(supersede),
                capabilities,
                ready: false,
                pending_display_name: None,
            },
        );
        if let Some(mut previous) = previous
            && let Some(cancel) = previous.supersede.take()
        {
            let _ = cancel.send(());
        }
        epoch
    }

    async fn unregister(&self, device_id: &str, epoch: i64) -> bool {
        let mut connections = self.connections.write().await;
        if connections.get(device_id).map(|c| c.epoch) == Some(epoch) {
            connections.remove(device_id);
            true
        } else {
            false
        }
    }

    pub async fn disconnect(&self, device_id: &str) {
        if let Some(mut connection) = self.connections.write().await.remove(device_id)
            && let Some(cancel) = connection.supersede.take()
        {
            let _ = cancel.send(());
        }
    }

    async fn disconnect_epoch(&self, device_id: &str, epoch: i64) {
        let mut connections = self.connections.write().await;
        if connections
            .get(device_id)
            .map(|connection| connection.epoch)
            != Some(epoch)
        {
            return;
        }
        if let Some(mut connection) = connections.remove(device_id)
            && let Some(cancel) = connection.supersede.take()
        {
            let _ = cancel.send(());
        }
    }

    pub async fn is_online(&self, device_id: &str) -> bool {
        self.connections
            .read()
            .await
            .get(device_id)
            .is_some_and(|connection| connection.ready)
    }

    pub async fn send(&self, device_id: &str, frame: TunnelFrame) -> Result<()> {
        let sender = self
            .connections
            .read()
            .await
            .get(device_id)
            .filter(|connection| connection.ready)
            .map(|c| c.sender.clone())
            .ok_or_else(|| anyhow!("device offline"))?;
        tokio::time::timeout(std::time::Duration::from_secs(2), sender.send(frame))
            .await
            .map_err(|_| anyhow!("device connection send queue timed out"))?
            .map_err(|_| anyhow!("device connection closed"))
    }

    pub async fn broadcast(&self, frame: TunnelFrame) -> usize {
        let senders: Vec<_> = self
            .connections
            .read()
            .await
            .values()
            .map(|connection| connection.sender.clone())
            .collect();
        let mut delivered = 0;
        for sender in senders {
            if sender.try_send(frame.clone()).is_ok() {
                delivered += 1;
            }
        }
        delivered
    }

    pub async fn broadcast_client_release(&self, release: ClientRelease) -> usize {
        let frames: Vec<_> = self
            .connections
            .read()
            .await
            .values()
            .map(|connection| {
                let frame = if connection.capabilities.contains(CLIENT_UPDATE_CAPABILITY) {
                    TunnelFrame::ClientUpdate {
                        release: release.clone(),
                    }
                } else {
                    TunnelFrame::ServerNotice {
                        code: "update_available".into(),
                        message: format!("{}:{}", release.commit_sha, release.release_sequence),
                    }
                };
                (connection.sender.clone(), frame)
            })
            .collect();
        let mut delivered = 0;
        for (sender, frame) in frames {
            if sender.try_send(frame).is_ok() {
                delivered += 1;
            }
        }
        delivered
    }

    pub async fn sync_display_name(&self, device_id: &str, display_name: &str) -> Result<bool> {
        let (epoch, sender) = {
            let mut connections = self.connections.write().await;
            let Some(connection) = connections.get_mut(device_id) else {
                return Ok(false);
            };
            if !connection
                .capabilities
                .contains(DEVICE_DISPLAY_NAME_SYNC_CAPABILITY)
            {
                return Ok(false);
            }
            if !connection.ready {
                connection.pending_display_name = Some(display_name.to_owned());
                return Ok(true);
            }
            (connection.epoch, connection.sender.clone())
        };
        let sent = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sender.send(TunnelFrame::DeviceConfig {
                display_name: display_name.to_owned(),
            }),
        )
        .await
        .map_err(|_| anyhow!("device connection send queue timed out"))
        .and_then(|result| result.map_err(|_| anyhow!("device connection closed")));
        if let Err(error) = sent {
            self.disconnect_epoch(device_id, epoch).await;
            return Err(error);
        }
        Ok(true)
    }

    async fn activate(&self, device_id: &str, epoch: i64, display_name: String) -> Result<()> {
        let mut connections = self.connections.write().await;
        let connection = connections
            .get_mut(device_id)
            .filter(|connection| connection.epoch == epoch)
            .ok_or_else(|| anyhow!("device connection was superseded"))?;
        if connection
            .capabilities
            .contains(DEVICE_DISPLAY_NAME_SYNC_CAPABILITY)
        {
            let display_name = connection
                .pending_display_name
                .take()
                .unwrap_or(display_name);
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                connection
                    .sender
                    .send(TunnelFrame::DeviceConfig { display_name }),
            )
            .await
            .map_err(|_| anyhow!("device connection send queue timed out"))?
            .map_err(|_| anyhow!("device connection closed"))?;
        }
        connection.ready = true;
        Ok(())
    }

    pub async fn query(&self, device_id: &str, query: DeviceQuery) -> Result<Value, String> {
        let correlation_id = new_id("qry");
        let (tx, rx) = oneshot::channel();
        self.pending_queries
            .lock()
            .await
            .insert(correlation_id.clone(), tx);
        if self
            .send(
                device_id,
                TunnelFrame::Query {
                    correlation_id: correlation_id.clone(),
                    query,
                },
            )
            .await
            .is_err()
        {
            self.pending_queries.lock().await.remove(&correlation_id);
            return Err("device_offline".into());
        }
        match tokio::time::timeout(std::time::Duration::from_secs(8), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("query_cancelled".into()),
            Err(_) => {
                self.pending_queries.lock().await.remove(&correlation_id);
                Err("query_timeout".into())
            }
        }
    }

    async fn complete_query(
        &self,
        correlation_id: &str,
        result: Option<Value>,
        error: Option<String>,
    ) {
        if let Some(sender) = self.pending_queries.lock().await.remove(correlation_id) {
            let _ = sender.send(match error {
                Some(e) => Err(e),
                None => Ok(result.unwrap_or(Value::Null)),
            });
        }
    }
}

pub async fn serve_socket(
    socket: WebSocket,
    state: AppState,
    authenticated_device: String,
    user_id: String,
) {
    if let Err(error) = run_socket(socket, state, &authenticated_device, &user_id).await {
        tracing::warn!(device_id=%authenticated_device,error=?error,"device tunnel closed with error");
    }
}

async fn run_socket(
    socket: WebSocket,
    state: AppState,
    authenticated_device: &str,
    user_id: &str,
) -> Result<()> {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), ws_rx.next())
        .await?
        .ok_or_else(|| anyhow!("connection closed before hello"))??;
    let Message::Text(text) = first else {
        return Err(anyhow!("first frame must be text hello"));
    };
    let hello: TunnelFrame = serde_json::from_str(&text)?;
    let (
        device_id,
        agent_version,
        security,
        last_sequence,
        client_queue_epoch,
        client_capabilities,
    ) = match hello {
        TunnelFrame::Hello {
            protocol_version,
            device_id,
            agent_version,
            transport_security,
            last_server_command_seq,
            command_queue_epoch,
            capabilities,
            ..
        } if protocol_version == DEVICE_PROTOCOL_VERSION && device_id == authenticated_device => (
            device_id,
            agent_version,
            transport_security,
            last_server_command_seq,
            command_queue_epoch,
            capabilities,
        ),
        _ => return Err(anyhow!("invalid hello")),
    };
    let expected_security = if state.config.is_secure() {
        TransportSecurity::Secure
    } else {
        TransportSecurity::Insecure
    };
    if security != expected_security {
        return Err(anyhow!("transport security mismatch"));
    }
    let supports_client_update = client_capabilities
        .iter()
        .any(|capability| capability == CLIENT_UPDATE_CAPABILITY);

    let (out_tx, mut out_rx) = mpsc::channel::<TunnelFrame>(256);
    let (supersede_tx, mut superseded) = oneshot::channel();
    let epoch = state
        .tunnels
        .register(
            &device_id,
            out_tx.clone(),
            supersede_tx,
            client_capabilities.into_iter().collect(),
        )
        .await;
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let text = serde_json::to_string(&frame)?;
            ws_tx.send(Message::Text(text.into())).await?;
        }
        Result::<()>::Ok(())
    });

    let session_result = async {
        state
            .store
            .mark_device_seen(&device_id, &agent_version, security, None)
            .await?;
        let display_name = state
            .store
            .device_display_name(user_id, &device_id)
            .await?
            .ok_or_else(|| anyhow!("device is not active"))?;
        out_tx
            .send(TunnelFrame::Welcome {
                protocol_version: DEVICE_PROTOCOL_VERSION,
                connection_id: new_id("conn"),
                connection_epoch: epoch,
                command_queue_epoch: state.store.queue_epoch().into(),
                server_time: now(),
                transport_security: expected_security,
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
                ],
                display_name: Some(display_name.clone()),
            })
            .await?;
        if let Some(release) = state.releases.current().await {
            out_tx
                .send(if supports_client_update {
                    TunnelFrame::ClientUpdate { release }
                } else {
                    TunnelFrame::ServerNotice {
                        code: "update_available".into(),
                        message: format!("{}:{}", release.commit_sha, release.release_sequence),
                    }
                })
                .await?;
        }
        let replay_after = if client_queue_epoch.as_deref() == Some(state.store.queue_epoch()) {
            last_sequence
        } else {
            0
        };
        for stored in state
            .store
            .pending_commands(&device_id, replay_after, 500)
            .await?
        {
            out_tx
                .send(TunnelFrame::Command {
                    queue_epoch: stored.queue_epoch,
                    server_sequence: stored.sequence,
                    command: stored.command,
                })
                .await?;
        }
        state
            .tunnels
            .activate(&device_id, epoch, display_name)
            .await?;
        publish_device_event(
            &state,
            user_id,
            &device_id,
            "device.online",
            json!({"epoch":epoch}),
        )
        .await?;

        loop {
            let idle_deadline = tokio::time::Instant::now()
                + std::time::Duration::from_secs(45);
            tokio::select! {
                message = ws_rx.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        let frame: TunnelFrame = serde_json::from_str(&text)?;
                        handle_frame(&state, user_id, &device_id, &agent_version, frame, &out_tx).await?;
                    }
                    Some(Ok(Message::Ping(_))) => {}
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {}
                },
                _ = &mut superseded => break,
                writer_result = &mut writer => {
                    writer_result.map_err(|error| anyhow!("tunnel writer task failed: {error}"))??;
                    break;
                },
                _ = tokio::time::sleep_until(idle_deadline) => return Err(anyhow!("device heartbeat timed out")),
            }
        }
        Result::<()>::Ok(())
    }
    .await;
    let was_current = state.tunnels.unregister(&device_id, epoch).await;
    writer.abort();
    if was_current {
        let _ = publish_device_event(
            &state,
            user_id,
            &device_id,
            "device.offline",
            json!({"lastSeenAt":now()}),
        )
        .await;
    }
    session_result
}

async fn handle_frame(
    state: &AppState,
    user_id: &str,
    device_id: &str,
    agent_version: &str,
    frame: TunnelFrame,
    out: &mpsc::Sender<TunnelFrame>,
) -> Result<()> {
    if !state
        .store
        .device_is_active_for_user(user_id, device_id)
        .await?
    {
        return Err(anyhow!("device was revoked"));
    }
    match frame {
        TunnelFrame::CommandAck {
            command_id,
            stage,
            result,
            error_code,
            error_message,
        } => {
            if stage == "completed"
                && let Some(thread) = result
                    .as_ref()
                    .and_then(|value| value.get("thread"))
                    .and_then(|value| serde_json::from_value::<ThreadSummary>(value.clone()).ok())
            {
                if thread.device_id != device_id {
                    return Err(anyhow!("created thread device mismatch"));
                }
                state.store.upsert_created_thread(user_id, &thread).await?;
            }
            let status = state
                .store
                .update_command_ack(
                    device_id,
                    &command_id,
                    &stage,
                    result.as_ref(),
                    error_code.as_deref(),
                    error_message.as_deref(),
                )
                .await?;
            publish_device_event(
                state,
                user_id,
                device_id,
                "command.status_changed",
                json!({
                    "commandId": command_id,
                    "status": status,
                    "errorCode": error_code,
                    "errorMessage": error_message,
                }),
            )
            .await?;
        }
        TunnelFrame::Event { mut event } => {
            if event.device_id != device_id {
                return Err(anyhow!("event device mismatch"));
            }
            if event.event_id.is_empty()
                || event.event_id.len() > 128
                || event.stream_id.is_empty()
                || event.stream_id.len() > 256
                || event.event_type.is_empty()
                || event.event_type.len() > 128
                || event.seq < 1
                || serde_json::to_vec(&event)?.len() > 512 * 1024
            {
                return Err(anyhow!("event violates size or identity limits"));
            }
            event.user_id = Some(user_id.into());
            if event.event_type == "thread.summary" {
                let thread = serde_json::from_value::<ThreadSummary>(event.payload.clone())?;
                if thread.device_id != device_id
                    || event.thread_id.as_deref() != Some(thread.id.as_str())
                    || event.project_id.as_deref() != Some(thread.project_id.as_str())
                {
                    return Err(anyhow!("thread summary identity mismatch"));
                }
                state.store.upsert_created_thread(user_id, &thread).await?;
            }
            if event.event_type == "project.summary" {
                let project = serde_json::from_value::<ProjectSummary>(event.payload.clone())?;
                if project.device_id != device_id {
                    return Err(anyhow!("project summary device mismatch"));
                }
                state
                    .store
                    .upsert_project_summary(user_id, &project, event.seq)
                    .await?;
            }
            if event.event_type == "project.removed" {
                let project_id = event
                    .project_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("project removal event has no project id"))?;
                if event.payload.get("projectId").and_then(Value::as_str) != Some(project_id) {
                    return Err(anyhow!("project removal payload mismatch"));
                }
                state
                    .store
                    .remove_project(user_id, device_id, project_id)
                    .await?;
            }
            if event.event_type == "approval.requested" {
                state.store.upsert_approval_event(user_id, &event).await?;
            }
            if event.event_type == "history.inventory_complete" {
                state
                    .store
                    .mark_history_inventory_complete(device_id)
                    .await?;
            }
            let cursor = state.store.append_event(user_id, &event).await?;
            let event_id = event.event_id.clone();
            state.events.publish(PublishedEvent {
                cursor,
                user_id: user_id.into(),
                event,
            });
            out.send(TunnelFrame::EventAck { event_id }).await?;
        }
        TunnelFrame::HistoryBatch { batch } => {
            if batch.device_id != device_id {
                return Err(anyhow!("history batch device mismatch"));
            }
            let cursor = match state.store.ingest_history_batch(user_id, &batch).await {
                Ok(cursor) => cursor,
                Err(error) => {
                    // History is application data carried by the tunnel. A
                    // malformed or temporarily uncommittable batch must not
                    // tear down heartbeats and command delivery for the whole
                    // device. Leave it unacknowledged so a later release or
                    // transient database recovery can accept the retry.
                    tracing::warn!(
                        device_id,
                        batch_id = %batch.batch_id,
                        thread_id = %batch.thread_id,
                        error = ?error,
                        "history batch rejected without closing device tunnel"
                    );
                    return Ok(());
                }
            };
            out.send(TunnelFrame::HistoryAck {
                batch_id: batch.batch_id,
                thread_id: batch.thread_id.clone(),
                acked_cursor: cursor.clone(),
            })
            .await?;
            publish_device_event(
                state,
                user_id,
                device_id,
                "history.sync_progress",
                json!({"threadId":batch.thread_id,"cursor":cursor}),
            )
            .await?;
        }
        TunnelFrame::QueryResponse {
            correlation_id,
            result,
            error_code,
        } => {
            state
                .tunnels
                .complete_query(&correlation_id, result, error_code)
                .await
        }
        TunnelFrame::Heartbeat { health, .. } => {
            state
                .store
                .mark_device_seen(
                    device_id,
                    agent_version,
                    if state.config.is_secure() {
                        TransportSecurity::Secure
                    } else {
                        TransportSecurity::Insecure
                    },
                    Some(&health),
                )
                .await?;
            out.send(TunnelFrame::HeartbeatAck { received_at: now() })
                .await?;
        }
        TunnelFrame::Hello { .. }
        | TunnelFrame::Welcome { .. }
        | TunnelFrame::Command { .. }
        | TunnelFrame::EventAck { .. }
        | TunnelFrame::HistoryAck { .. }
        | TunnelFrame::Query { .. }
        | TunnelFrame::HeartbeatAck { .. }
        | TunnelFrame::DeviceConfig { .. }
        | TunnelFrame::ClientUpdate { .. }
        | TunnelFrame::ServerNotice { .. } => return Err(anyhow!("frame not allowed from device")),
    }
    Ok(())
}

pub async fn publish_device_event(
    state: &AppState,
    user_id: &str,
    device_id: &str,
    event_type: &str,
    payload: Value,
) -> Result<i64> {
    let event = NuntiusEvent {
        event_id: new_id("evt"),
        user_id: Some(user_id.into()),
        device_id: device_id.into(),
        project_id: None,
        thread_id: None,
        turn_id: None,
        stream_id: format!("device:{device_id}"),
        seq: (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64,
        event_type: event_type.into(),
        durability: "durable".into(),
        occurred_at: now(),
        payload,
    };
    let cursor = state.store.append_event(user_id, &event).await?;
    state.events.publish(PublishedEvent {
        cursor,
        user_id: user_id.into(),
        event,
    });
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rename_during_handshake_replaces_the_initial_snapshot() {
        let registry = TunnelRegistry::new();
        let (sender, mut receiver) = mpsc::channel(4);
        let (supersede, _superseded) = oneshot::channel();
        let epoch = registry
            .register(
                "dev_test",
                sender,
                supersede,
                HashSet::from([DEVICE_DISPLAY_NAME_SYNC_CAPABILITY.into()]),
            )
            .await;

        assert!(
            registry
                .sync_display_name("dev_test", "Newest name")
                .await
                .unwrap()
        );
        registry
            .activate("dev_test", epoch, "Stale handshake name".into())
            .await
            .unwrap();

        assert!(registry.is_online("dev_test").await);
        assert!(matches!(
            receiver.recv().await,
            Some(TunnelFrame::DeviceConfig { display_name }) if display_name == "Newest name"
        ));
    }
}
