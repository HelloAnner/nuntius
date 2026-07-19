use crate::{
    assets, command_queue, directory, error::ApiError, executor::CommandExecutor, protocol::*,
};
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
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
        .route("/api/v1/openapi.yaml", get(openapi))
        .route("/api/v1/directories/roots", get(directory_roots))
        .route("/api/v1/directories", get(directory_list))
        .route("/api/v1/projects", get(projects).post(create_project))
        .route(
            "/api/v1/projects/{project_id}/threads",
            get(project_threads).post(create_thread),
        )
        .route("/api/v1/threads", get(threads))
        .route("/api/v1/threads/{thread_id}/history", get(history))
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
    Ok(Json(
        json!({"apiVersion":"v1","clientVersion":env!("CARGO_PKG_VERSION"),"buildSha":nuntius_updater::build_sha(),"deviceId":executor.device_id,"paired":executor.config.device_id.is_some(),"localBind":executor.config.local_bind,"appServerRunning":executor.app.is_running().await,"projects":projects,"pendingCommands":inbox,"pendingEvents":outbox,"activeTurns":active,"capabilities":["local-console.v1","directory-browser.v1","app-server.v1","sse.v1"]}),
    ))
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
async fn events(
    State(executor): State<CommandExecutor>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = executor.events.subscribe();
    let output = stream! {loop{match receiver.recv().await{Ok(event)=>yield Ok(Event::default().id(event.event_id.clone()).event("nuntius").json_data(event).expect("serializable event")),Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>yield Ok(Event::default().event("resync_required").data("{}")),Err(_)=>break}}};
    Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}
