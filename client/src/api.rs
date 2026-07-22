use crate::{
    assets, attachments, command_queue, directory, error::ApiError, executor::CommandExecutor,
    protocol::*,
};
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{delete, get, post},
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, time::Duration};

pub fn router(executor: CommandExecutor) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/api/v1/info", get(info))
        .route("/api/v1/sync", get(sync_snapshot))
        .route("/api/v1/openapi.yaml", get(openapi))
        .route("/api/v1/directories/roots", get(directory_roots))
        .route("/api/v1/directories", get(directory_list))
        .route("/api/v1/projects", get(projects).post(create_project))
        .route("/api/v1/projects/{project_id}", delete(delete_project))
        .route(
            "/api/v1/projects/{project_id}/threads",
            get(project_threads).post(create_thread),
        )
        .route("/api/v1/threads", get(threads))
        .route("/api/v1/threads/{thread_id}/history", get(history))
        .route(
            "/api/v1/attachments/{attachment_id}/content",
            get(attachment_content),
        )
        .route(
            "/api/v1/attachments/{attachment_id}/thumbnail",
            get(attachment_thumbnail),
        )
        .route("/api/v1/threads/{thread_id}/turns", post(start_turn))
        .route("/api/v1/threads/{thread_id}/steer", post(steer_turn))
        .route(
            "/api/v1/threads/{thread_id}/interrupt",
            post(interrupt_turn),
        )
        .route("/api/v1/threads/{thread_id}/archive", post(archive_thread))
        .route("/api/v1/approvals/{approval_id}/decision", post(approval))
        .route("/api/v1/events", get(events))
        .route("/", get(assets::serve_root))
        .route("/{*path}", get(assets::serve_path))
        .layer(middleware::from_fn(local_request_guard))
        .with_state(executor)
}

async fn local_request_guard(request: Request, next: Next) -> Result<Response, StatusCode> {
    let is_api = request.uri().path().starts_with("/api/");
    let is_event_stream = request.uri().path() == "/api/v1/events";
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !is_loopback_authority(host) {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
        let parsed = url::Url::parse(origin).map_err(|_| StatusCode::FORBIDDEN)?;
        if parsed.scheme() != "http"
            || !parsed.host_str().is_some_and(|origin_host| {
                matches!(origin_host, "localhost" | "127.0.0.1" | "::1")
                    && same_authority(host, &parsed)
            })
        {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let mut response = next.run(request).await;
    if is_api {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            if is_event_stream {
                "no-cache, no-transform"
            } else {
                "no-store"
            }
            .parse()
            .expect("static header"),
        );
        if is_event_stream {
            response
                .headers_mut()
                .insert("x-accel-buffering", "no".parse().expect("static header"));
        }
        response.headers_mut().insert(
            header::X_CONTENT_TYPE_OPTIONS,
            "nosniff".parse().expect("static header"),
        );
    }
    Ok(response)
}

fn same_authority(host_header: &str, origin: &url::Url) -> bool {
    let Ok(host_url) = url::Url::parse(&format!("http://{host_header}")) else {
        return false;
    };
    host_url.host_str() == origin.host_str()
        && host_url.port_or_known_default() == origin.port_or_known_default()
}

fn is_loopback_authority(authority: &str) -> bool {
    authority == "localhost"
        || authority.starts_with("localhost:")
        || authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:")
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ok"}))
}
async fn ready(State(executor): State<CommandExecutor>) -> Result<Json<Value>, ApiError> {
    executor.store.ready().await?;
    Ok(Json(json!({"status":"ready"})))
}
async fn info(State(executor): State<CommandExecutor>) -> Result<Json<Value>, ApiError> {
    let (projects, inbox, outbox, active) = executor.store.counts().await?;
    let display_name = executor.display_name.read().await.clone();
    let providers = executor.agents.statuses().await;
    let app_server_running = providers
        .iter()
        .any(|status| status.provider == AgentProvider::Codex && status.status == "online");
    Ok(Json(
        json!({"apiVersion":"v1","clientVersion":env!("CARGO_PKG_VERSION"),"buildSha":nuntius_updater::build_sha(),"releaseSequence":nuntius_updater::build_sequence(),"deviceId":executor.device_id,"displayName":display_name,"paired":executor.config.device_id.is_some(),"localBind":executor.config.local_bind,"appServerRunning":app_server_running,"providers":providers,"projects":projects,"pendingCommands":inbox,"pendingEvents":outbox,"activeTurns":active,"capabilities":["local-console.v1","directory-browser.v1","project-delete.v1","image-input.v1","agent-provider.v1","agent-model-config.v1","app-server.v1","sse.v1",DEVICE_DISPLAY_NAME_SYNC_CAPABILITY,PROVIDER_USAGE_CAPABILITY]}),
    ))
}
async fn sync_snapshot(
    State(executor): State<CommandExecutor>,
) -> Result<Json<SyncSnapshot>, ApiError> {
    // Capture the cursor first so mutations racing the following reads are
    // replayed by SSE instead of falling between snapshot and subscription.
    let (_, maximum) = executor.store.browser_event_bounds().await?;
    Ok(Json(SyncSnapshot {
        cursor: maximum.unwrap_or(0),
        generated_at: now(),
        devices: Vec::new(),
        projects: executor.store.list_projects(&executor.device_id).await?,
        threads: executor
            .store
            .list_threads_page(&executor.device_id, None, 500, 0)
            .await?,
        approvals: executor.store.list_approvals(&executor.device_id).await?,
    }))
}
async fn openapi() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/yaml"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../api/openapi.yaml"),
    )
        .into_response()
}

async fn directory_roots(
    State(executor): State<CommandExecutor>,
) -> Result<Json<DirectoryListResponse>, ApiError> {
    Ok(Json(
        directory::roots(&executor.config, &executor.store, &executor.device_id)
            .await
            .map_err(ApiError::Internal)?,
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirQuery {
    parent_ref: String,
    cursor: Option<String>,
}
async fn directory_list(
    State(executor): State<CommandExecutor>,
    Query(query): Query<DirQuery>,
) -> Result<Json<DirectoryListResponse>, ApiError> {
    Ok(Json(
        directory::list(
            &executor.config,
            &executor.store,
            &executor.device_id,
            &query.parent_ref,
            query.cursor.as_deref(),
        )
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?,
    ))
}
async fn projects(
    State(executor): State<CommandExecutor>,
) -> Result<Json<Vec<ProjectSummary>>, ApiError> {
    Ok(Json(
        executor.store.list_projects(&executor.device_id).await?,
    ))
}
async fn create_project(
    State(executor): State<CommandExecutor>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::ProjectCreate(request),
        None,
        None,
    )
    .await
}
async fn delete_project(
    State(executor): State<CommandExecutor>,
    Path(project_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::ProjectDelete {
            project_id: project_id.clone(),
        },
        Some(project_id),
        None,
    )
    .await
}
async fn project_threads(
    State(executor): State<CommandExecutor>,
    Path(project_id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    Ok(Json(
        executor
            .store
            .list_threads_page(
                &executor.device_id,
                Some(&project_id),
                query.limit.unwrap_or(100).clamp(1, 500),
                query.offset.unwrap_or(0).clamp(0, 1_000_000),
            )
            .await?,
    ))
}
async fn create_thread(
    State(executor): State<CommandExecutor>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateThreadRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::ThreadCreate {
            project_id: project_id.clone(),
            request,
        },
        Some(project_id),
        None,
    )
    .await
}
async fn threads(
    State(executor): State<CommandExecutor>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    Ok(Json(
        executor
            .store
            .list_threads_page(
                &executor.device_id,
                None,
                query.limit.unwrap_or(100).clamp(1, 500),
                query.offset.unwrap_or(0).clamp(0, 1_000_000),
            )
            .await?,
    ))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}
async fn history(
    State(executor): State<CommandExecutor>,
    Path(thread_id): Path<String>,
) -> Result<Json<Vec<HistoryRecord>>, ApiError> {
    if executor
        .store
        .thread(&thread_id, &executor.device_id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    Ok(Json(
        executor
            .store
            .history_records(&thread_id, &executor.device_id)
            .await?,
    ))
}

async fn attachment_content(
    State(executor): State<CommandExecutor>,
    Path(attachment_id): Path<String>,
) -> Result<Response, ApiError> {
    let (thread_id, attachment) = executor
        .store
        .attachment_ref(&attachment_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let root = crate::config::data_dir().map_err(ApiError::Internal)?;
    serve_attachment_file(
        attachments::original_path(&root, &thread_id, &attachment.id, &attachment.extension)
            .map_err(ApiError::Internal)?,
        &attachment.mime_type,
    )
    .await
}

async fn attachment_thumbnail(
    State(executor): State<CommandExecutor>,
    Path(attachment_id): Path<String>,
) -> Result<Response, ApiError> {
    let (thread_id, attachment) = executor
        .store
        .attachment_ref(&attachment_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let root = crate::config::data_dir().map_err(ApiError::Internal)?;
    serve_attachment_file(
        attachments::thumbnail_path(&root, &thread_id, &attachment.id)
            .map_err(ApiError::Internal)?,
        "image/webp",
    )
    .await
}

async fn serve_attachment_file(
    path: std::path::PathBuf,
    mime_type: &str,
) -> Result<Response, ApiError> {
    let bytes = tokio::fs::read(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound
        } else {
            ApiError::Internal(error.into())
        }
    })?;
    let content_type = HeaderValue::from_bytes(mime_type.as_bytes())
        .map_err(|error| ApiError::Internal(error.into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("inline"),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn start_turn(
    State(executor): State<CommandExecutor>,
    Path(thread_id): Path<String>,
    Json(request): Json<StartTurnRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::TurnStart {
            thread_id: thread_id.clone(),
            request,
            attachments: Vec::new(),
        },
        None,
        Some(thread_id),
    )
    .await
}
async fn steer_turn(
    State(executor): State<CommandExecutor>,
    Path(thread_id): Path<String>,
    Json(request): Json<TextInputRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::TurnSteer {
            thread_id: thread_id.clone(),
            request,
            attachments: Vec::new(),
        },
        None,
        Some(thread_id),
    )
    .await
}
async fn interrupt_turn(
    State(executor): State<CommandExecutor>,
    Path(thread_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::TurnInterrupt {
            thread_id: thread_id.clone(),
        },
        None,
        Some(thread_id),
    )
    .await
}
#[derive(Deserialize)]
struct ArchiveRequest {
    archived: bool,
}
async fn archive_thread(
    State(executor): State<CommandExecutor>,
    Path(thread_id): Path<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::ThreadArchive {
            thread_id: thread_id.clone(),
            archived: request.archived,
        },
        None,
        Some(thread_id),
    )
    .await
}
async fn approval(
    State(executor): State<CommandExecutor>,
    Path(approval_id): Path<String>,
    Json(request): Json<ApprovalDecisionRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    run(
        &executor,
        DeviceCommandKind::ApprovalDecide {
            approval_id,
            request,
        },
        None,
        None,
    )
    .await
}

async fn run(
    executor: &CommandExecutor,
    kind: DeviceCommandKind,
    project_id: Option<String>,
    thread_id: Option<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let expires_at = (time::OffsetDateTime::now_utc() + time::Duration::minutes(10))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let command = DeviceCommand {
        command_id: new_id("local"),
        device_id: executor.device_id.clone(),
        project_id,
        thread_id,
        issued_at: now(),
        expires_at,
        command: kind,
    };
    let target = command_queue::target_key(&command);
    let priority = command_queue::priority(&command);
    let mut feedback = executor.command_acks.subscribe();
    executor
        .store
        .enqueue_local_command(&target, priority, &command)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    executor.command_notify.notify_one();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let record = executor
            .store
            .inbox(&command.command_id)
            .await
            .map_err(|error| ApiError::BadRequest(error.to_string()))?
            .ok_or_else(|| ApiError::BadRequest("命令入队失败".into()))?;
        match record.status.as_str() {
            "completed" => {
                return Ok((StatusCode::OK, Json(record.result.unwrap_or(Value::Null))));
            }
            "failed" | "unknown" | "expired" => {
                return Err(ApiError::BadRequest(
                    record
                        .error_message
                        .or(record.error_code)
                        .unwrap_or_else(|| "命令执行失败，请重试".into()),
                ));
            }
            _ => {}
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                return Err(ApiError::BadRequest("命令仍在执行，可稍后在会话中查看结果".into()));
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            ack = feedback.recv() => match ack {
                Ok(TunnelFrame::CommandAck { command_id, .. }) if command_id == command.command_id => {}
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
}
#[derive(Deserialize)]
struct EventsQuery {
    after: Option<i64>,
}
async fn events(
    State(executor): State<CommandExecutor>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let header_after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let after = header_after.or(query.after).unwrap_or(0);
    let mut receiver = executor.events.subscribe();
    let (minimum, _) = executor.store.browser_event_bounds().await?;
    let mut resync_required = after > 0
        && match minimum {
            Some(minimum) => after.saturating_add(1) < minimum,
            None => true,
        };
    let mut replay = if resync_required {
        Vec::new()
    } else {
        executor.store.replay_browser_events(after, 10_001).await?
    };
    if replay.len() > 10_000 {
        replay.clear();
        resync_required = true;
    }
    let replay_executor = executor.clone();
    let output = stream! {
        if resync_required {
            yield Ok(Event::default().event("resync_required").data("{}"));
        }
        for (cursor, event) in replay {
            yield Ok(Event::default().id(cursor.to_string()).event("nuntius").json_data(event).expect("serializable event"));
        }
        loop {
            match receiver.recv().await {
                Ok(event) => match replay_executor.store.browser_event_cursor(&event.event_id).await {
                    Ok(Some(cursor)) => yield Ok(Event::default().id(cursor.to_string()).event("nuntius").json_data(event).expect("serializable event")),
                    Ok(None) | Err(_) => {
                        yield Ok(Event::default().event("resync_required").data("{}"));
                        break;
                    }
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    yield Ok(Event::default().event("resync_required").data("{}"));
                    break;
                }
                Err(_) => break,
            }
        }
    };
    Ok(Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}
