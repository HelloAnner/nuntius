use crate::{directory, executor::CommandExecutor, pairing, protocol::*, store::InboxRecord};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header},
};

pub async fn run_forever(executor: CommandExecutor) {
    let mut backoff = 1_u64;
    loop {
        let attempt_started = tokio::time::Instant::now();
        match run_connection(executor.clone()).await {
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

async fn run_connection(executor: CommandExecutor) -> Result<()> {
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
    send(
        &mut socket,
        &TunnelFrame::Hello {
            protocol_version: DEVICE_PROTOCOL_VERSION,
            device_id: executor.device_id.clone(),
            instance_id,
            agent_version: env!("CARGO_PKG_VERSION").into(),
            transport_security: executor.config.transport_security(),
            last_server_command_seq: executor.store.last_server_sequence().await?,
            event_acks: BTreeMap::new(),
            history_cursors: BTreeMap::new(),
            capabilities: vec![
                "command-ack.v1".into(),
                "event-ack.v1".into(),
                "history.v1".into(),
                "directory-browser.v1".into(),
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
    match serde_json::from_str::<TunnelFrame>(&text)? {
        TunnelFrame::Welcome {
            protocol_version,
            transport_security,
            ..
        } if protocol_version == DEVICE_PROTOCOL_VERSION
            && transport_security == executor.config.transport_security() => {}
        other => bail!("invalid server welcome: {other:?}"),
    }
    tracing::info!(device_id=%executor.device_id,security=?executor.config.transport_security(),"device tunnel connected");
    let (out_tx, mut out_rx) = mpsc::channel::<TunnelFrame>(512);
    let (command_tx, mut command_rx) = mpsc::channel::<(i64, DeviceCommand)>(128);
    let command_executor = executor.clone();
    let command_out = out_tx.clone();
    let command_worker = tokio::spawn(async move {
        while let Some((sequence, command)) = command_rx.recv().await {
            if let Err(error) = execute_reliable(
                command_executor.clone(),
                command_out.clone(),
                sequence,
                command,
            )
            .await
            {
                tracing::error!(error=?error,"command execution pipeline failed");
            }
        }
    });
    let mut events = executor.events.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // History is already durable in the local outbox. Flush it quickly so a
    // browser watching the public server sees external terminal activity with
    // sub-second-to-low-second latency, without coupling delivery to browser state.
    let mut flush = tokio::time::interval(Duration::from_secs(1));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_server_activity = tokio::time::Instant::now();
    send_pending(&executor, &mut socket).await?;
    executor.emit_inventory().await?;
    loop {
        let watchdog_deadline = last_server_activity + Duration::from_secs(45);
        tokio::select! {
            incoming=socket.next()=>match incoming{Some(Ok(Message::Text(text)))=>{last_server_activity=tokio::time::Instant::now();let frame: TunnelFrame=serde_json::from_str(&text)?;handle_server_frame(&executor,&out_tx,&command_tx,frame).await?},Some(Ok(Message::Ping(payload)))=>{last_server_activity=tokio::time::Instant::now();socket.send(Message::Pong(payload)).await?;},Some(Ok(Message::Close(_)))|None=>break,Some(Err(error))=>{command_worker.abort();return Err(error.into())},_=>{}},
            Some(frame)=out_rx.recv()=>send(&mut socket,&frame).await?,
            event=events.recv()=>match event{Ok(event)=>send(&mut socket,&TunnelFrame::Event{event}).await?,Err(broadcast::error::RecvError::Lagged(_))=>send_pending(&executor,&mut socket).await?,Err(broadcast::error::RecvError::Closed)=>break},
            _=heartbeat.tick()=>{let(project_count,inbox_depth,outbox_depth,active)=executor.store.counts().await?;let health=DeviceHealth{app_server_status:if executor.app.is_running().await{"online".into()}else{"stopped".into()},storage_status:"ok".into(),inbox_depth,outbox_depth,history_backfill_depth:executor.store.pending_history(1000).await?.len() as i64,active_turn_count:active,pending_approval_count:executor.store.pending_approval_count().await?,project_count,codex_version:None};send(&mut socket,&TunnelFrame::Heartbeat{sent_at:now(),health}).await?;},
            _=flush.tick()=>send_pending(&executor,&mut socket).await?,
            _=tokio::time::sleep_until(watchdog_deadline)=>{command_worker.abort();return Err(anyhow!("server heartbeat acknowledgement timed out"))},
        }
    }
    command_worker.abort();
    Ok(())
}

async fn handle_server_frame(
    executor: &CommandExecutor,
    out: &mpsc::Sender<TunnelFrame>,
    commands: &mpsc::Sender<(i64, DeviceCommand)>,
    frame: TunnelFrame,
) -> Result<()> {
    match frame {
        TunnelFrame::Command {
            server_sequence,
            command,
        } => {
            if command.device_id != executor.device_id {
                bail!("command targets a different device")
            }
            commands
                .send((server_sequence, command))
                .await
                .map_err(|_| anyhow!("command worker stopped"))?;
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
        TunnelFrame::ServerNotice { code, message } => {
            tracing::warn!(%code,%message,"server notice")
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

async fn execute_reliable(
    executor: CommandExecutor,
    out: mpsc::Sender<TunnelFrame>,
    sequence: i64,
    command: DeviceCommand,
) -> Result<()> {
    let inbox = executor.store.receive_command(sequence, &command).await?;
    match inbox.status.as_str() {
        "completed" | "failed" | "unknown" | "expired" => {
            return ack_terminal(&out, &inbox).await;
        }
        "applying" => {
            executor
                .store
                .finish_command_as(
                    &command.command_id,
                    sequence,
                    "unknown",
                    None,
                    Some("execution_state_unknown_after_restart"),
                )
                .await?;
            return out
                .send(TunnelFrame::CommandAck {
                    command_id: command.command_id,
                    stage: "unknown".into(),
                    result: None,
                    error_code: Some("execution_state_unknown_after_restart".into()),
                })
                .await
                .map_err(|_| anyhow!("tunnel sender closed"));
        }
        _ => {}
    }
    let expired = time::OffsetDateTime::parse(
        &command.expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|expires_at| expires_at <= time::OffsetDateTime::now_utc())
    .unwrap_or(true);
    if expired {
        executor
            .store
            .finish_command_as(
                &command.command_id,
                sequence,
                "expired",
                None,
                Some("expired"),
            )
            .await?;
        return out
            .send(TunnelFrame::CommandAck {
                command_id: command.command_id,
                stage: "expired".into(),
                result: None,
                error_code: Some("expired".into()),
            })
            .await
            .map_err(|_| anyhow!("tunnel sender closed"));
    }
    out.send(TunnelFrame::CommandAck {
        command_id: command.command_id.clone(),
        stage: "persisted".into(),
        result: None,
        error_code: None,
    })
    .await
    .map_err(|_| anyhow!("tunnel sender closed"))?;
    if !executor.store.start_command(&command.command_id).await? {
        return Ok(());
    };
    out.send(TunnelFrame::CommandAck {
        command_id: command.command_id.clone(),
        stage: "applying".into(),
        result: None,
        error_code: None,
    })
    .await
    .map_err(|_| anyhow!("tunnel sender closed"))?;
    match executor.execute(&command).await {
        Ok(result) => {
            executor
                .store
                .finish_command(&command.command_id, sequence, Some(&result), None)
                .await?;
            out.send(TunnelFrame::CommandAck {
                command_id: command.command_id,
                stage: "completed".into(),
                result: Some(result),
                error_code: None,
            })
            .await
            .map_err(|_| anyhow!("tunnel sender closed"))?
        }
        Err(error) => {
            let code = classify_error(&error);
            let status = if code == "outcome_unknown" {
                "unknown"
            } else {
                "failed"
            };
            executor
                .store
                .finish_command_as(&command.command_id, sequence, status, None, Some(&code))
                .await?;
            out.send(TunnelFrame::CommandAck {
                command_id: command.command_id,
                stage: status.into(),
                result: None,
                error_code: Some(code),
            })
            .await
            .map_err(|_| anyhow!("tunnel sender closed"))?
        }
    }
    Ok(())
}
async fn ack_terminal(out: &mpsc::Sender<TunnelFrame>, inbox: &InboxRecord) -> Result<()> {
    out.send(TunnelFrame::CommandAck {
        command_id: inbox.command.command_id.clone(),
        stage: inbox.status.clone(),
        result: inbox.result.clone(),
        error_code: inbox.error_code.clone(),
    })
    .await
    .map_err(|_| anyhow!("tunnel sender closed"))
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
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    for event in executor.store.pending_events(500).await? {
        send(socket, &TunnelFrame::Event { event }).await?;
    }
    for batch in executor.store.pending_history(100).await? {
        send(socket, &TunnelFrame::HistoryBatch { batch }).await?;
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
fn classify_error(error: &anyhow::Error) -> String {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("timed out") || message.contains("outcome is unknown") {
        "outcome_unknown".into()
    } else if message.contains("not found") {
        "not_found".into()
    } else if message.contains("expired") {
        "expired".into()
    } else if message.contains("outside allowed") || message.contains("invalid") {
        "invalid_request".into()
    } else if message.contains("app server") || message.contains("codex") {
        "app_server_unavailable".into()
    } else {
        "execution_failed".into()
    }
}
