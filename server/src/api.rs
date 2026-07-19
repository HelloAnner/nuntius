use crate::{
    AppState, assets, auth,
    config::random_secret,
    error::{ApiError, json_message},
    event_hub::PublishedEvent,
    protocol::*,
    store::{SessionRecord, unix_to_rfc3339},
    tunnel,
};
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State, WebSocketUpgrade, ws::WebSocket},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{delete, get, post},
};
use base64::Engine;
use futures_util::Stream;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{convert::Infallible, time::Duration as StdDuration};
use time::{Duration, OffsetDateTime};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/v1/info", get(info))
        .route("/api/v1/update-notices", post(update_notice))
        .route("/api/v1/sync", get(sync_snapshot))
        .route("/api/v1/openapi.yaml", get(openapi))
        .route("/api/v1/auth/bootstrap", post(bootstrap))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/session", get(current_session))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/pairing-codes", post(create_pairing_code))
        .route("/api/v1/device-auth/pair", post(pair_device))
        .route("/api/v1/device-auth/challenge", post(device_challenge))
        .route("/api/v1/device-auth/token", post(device_token))
        .route("/api/v1/devices", get(list_devices))
        .route(
            "/api/v1/devices/{device_id}",
            get(get_device).patch(rename_device).delete(revoke_device),
        )
        .route("/api/v1/devices/{device_id}/refresh", post(refresh_device))
        .route(
            "/api/v1/devices/{device_id}/history-sync",
            post(sync_device_history),
        )
        .route(
            "/api/v1/devices/{device_id}/directories/roots",
            get(directory_roots),
        )
        .route(
            "/api/v1/devices/{device_id}/directories",
            get(directory_list),
        )
        .route(
            "/api/v1/devices/{device_id}/projects",
            get(list_projects).post(create_project),
        )
        .route(
            "/api/v1/devices/{device_id}/projects/{project_id}",
            delete(delete_project),
        )
        .route(
            "/api/v1/devices/{device_id}/projects/{project_id}/threads",
            get(list_project_threads).post(create_thread),
        )
        .route("/api/v1/threads", get(list_all_threads))
        .route(
            "/api/v1/threads/{thread_id}/turns",
            get(history_turns).post(start_turn),
        )
        .route("/api/v1/threads/{thread_id}/steer", post(steer_turn))
        .route(
            "/api/v1/threads/{thread_id}/interrupt",
            post(interrupt_turn),
        )
        .route("/api/v1/threads/{thread_id}/archive", post(archive_thread))
        .route(
            "/api/v1/threads/{thread_id}/history-sync",
            post(sync_thread_history),
        )
        .route("/api/v1/turns/{turn_id}/items", get(history_items))
        .route(
            "/api/v1/approvals/{approval_id}/decision",
            post(decide_approval),
        )
        .route("/api/v1/approvals", get(list_approvals))
        .route("/api/v1/commands/{command_id}", get(command_status))
        .route("/api/v1/events", get(events))
        .route("/api/v1/device-tunnel", get(device_tunnel))
        .route("/", get(assets::serve_root))
        .route("/{*path}", get(assets::serve_path))
        .layer(middleware::from_fn(api_response_headers))
        .with_state(state)
}

async fn api_response_headers(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let is_event_stream = request.uri().path() == "/api/v1/events";
    let mut response = next.run(request).await;
    if is_api {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if is_event_stream {
                "no-cache, no-transform"
            } else {
                "no-store"
            }),
        );
        if is_event_stream {
            response
                .headers_mut()
                .insert("x-accel-buffering", HeaderValue::from_static("no"));
        }
        response.headers_mut().insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
    }
    response
}

async fn healthz() -> Json<Value> {
    json_message("ok")
}

async fn readyz(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    state.store.ready().await.map_err(ApiError::internal)?;
    Ok(json_message("ready"))
}

async fn info(State(state): State<AppState>) -> Result<Json<ServerInfo>, ApiError> {
    Ok(Json(ServerInfo {
        api_version: "v1".into(),
        server_version: env!("CARGO_PKG_VERSION").into(),
        build_sha: nuntius_updater::build_sha().into(),
        release_sequence: nuntius_updater::build_sequence(),
        transport_security: state.transport_security(),
        initialized: state.store.initialized().await?,
        capabilities: vec![
            "sse.v1".into(),
            "device-tunnel.v1".into(),
            "history.v1".into(),
            "directory-browser.v1".into(),
            "agent-provider.v1".into(),
            "project-delete.v1".into(),
            "sync-snapshot.v1".into(),
            "approvals.v1".into(),
            "server-update-relay.ssh.v1".into(),
            DEVICE_DISPLAY_NAME_SYNC_CAPABILITY.into(),
        ],
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateNoticeRequest {
    commit_sha: String,
    release_sequence: u64,
}

async fn update_notice(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateNoticeRequest>,
) -> Result<Json<Value>, ApiError> {
    let configured = tokio::fs::read_to_string(state.data_dir.join("secrets/update-notice-token"))
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ApiError::Unavailable("update notice receiver is not configured".into())
            } else {
                ApiError::internal(error)
            }
        })?;
    let supplied = auth::bearer_token(&headers).ok_or(ApiError::Forbidden)?;
    if auth::hash_secret(configured.trim()) != auth::hash_secret(supplied) {
        return Err(ApiError::Forbidden);
    }
    if request.release_sequence == 0
        || request.commit_sha.len() != 40
        || !request
            .commit_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::BadRequest("invalid release notice".into()));
    }
    let delivered = state
        .tunnels
        .broadcast(TunnelFrame::ServerNotice {
            code: "update_available".into(),
            message: format!("{}:{}", request.commit_sha, request.release_sequence),
        })
        .await;
    tracing::info!(
        commit_sha = %request.commit_sha,
        release_sequence = request.release_sequence,
        delivered,
        "release notice delivered to connected devices"
    );
    Ok(Json(json!({"accepted": true, "delivered": delivered})))
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

async fn sync_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SyncSnapshot>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    // Capture the cursor before reading the snapshot. Mutations racing with the reads then have
    // a greater cursor and are replayed by SSE instead of being silently skipped.
    let (_, maximum) = state.store.event_bounds(&session.user_id).await?;
    let mut devices = state.store.list_devices(&session.user_id).await?;
    let mut projects = Vec::new();
    for device in &mut devices {
        if device.status != DeviceStatus::Revoked && state.tunnels.is_online(&device.id).await {
            device.status = DeviceStatus::Online;
        }
        projects.extend(
            state
                .store
                .list_projects(&session.user_id, &device.id)
                .await?,
        );
    }
    let threads = state
        .store
        .list_threads(&session.user_id, None, None, 500, 0)
        .await?;
    let approvals = state.store.list_approvals(&session.user_id, true).await?;
    Ok(Json(SyncSnapshot {
        cursor: maximum.unwrap_or(0),
        generated_at: now(),
        devices,
        projects,
        threads,
        approvals,
    }))
}

async fn bootstrap(
    State(state): State<AppState>,
    Json(request): Json<BootstrapRequest>,
) -> Result<Response, ApiError> {
    let login_name = bounded_nonempty("loginName", &request.login_name, 128)?;
    bounded_nonempty("bootstrapToken", &request.bootstrap_token, 256)?;
    if request.password.len() > 1024 {
        return Err(ApiError::BadRequest("password is too long".into()));
    }
    let path = state.data_dir.join("secrets/bootstrap-token");
    let expected = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| ApiError::Forbidden)?;
    if auth::hash_secret(expected.trim()) != auth::hash_secret(&request.bootstrap_token) {
        return Err(ApiError::Forbidden);
    }
    let password_hash = auth::hash_password(&request.password)?;
    let user = state
        .store
        .create_owner(login_name, &password_hash)
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let (token, csrf, expires_at) =
        auth::create_session(&state.store, &user, state.config.session_ttl_hours, None).await?;
    let _ = tokio::fs::remove_file(path).await;
    session_response(&state, &user.id, &user.login_name, token, csrf, expires_at)
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let login_name = bounded_nonempty("loginName", &request.login_name, 128)?;
    if request.password.len() > 1024 {
        return Err(ApiError::Unauthorized);
    }
    let user = state
        .store
        .user_by_login(login_name)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if !auth::verify_password(&user.password_hash, &request.password) {
        return Err(ApiError::Unauthorized);
    }
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let (token, csrf, expires_at) = auth::create_session(
        &state.store,
        &user,
        state.config.session_ttl_hours,
        user_agent,
    )
    .await?;
    session_response(&state, &user.id, &user.login_name, token, csrf, expires_at)
}

fn session_response(
    state: &AppState,
    user_id: &str,
    login_name: &str,
    token: String,
    csrf: String,
    expires_at: String,
) -> Result<Response, ApiError> {
    let cookie = auth::session_cookie(
        &token,
        state.config.is_secure(),
        state.config.session_ttl_hours * 3600,
    );
    let mut response = Json(WebSessionView {
        user_id: user_id.into(),
        login_name: login_name.into(),
        csrf_token: csrf,
        expires_at,
        transport_security: state.transport_security(),
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn current_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<WebSessionView>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    let csrf = random_secret(24);
    state
        .store
        .rotate_csrf(&session.id, &auth::hash_secret(&csrf))
        .await?;
    Ok(Json(WebSessionView {
        user_id: session.user_id,
        login_name: session.login_name,
        csrf_token: csrf,
        expires_at: unix_to_rfc3339(session.expires_at).map_err(ApiError::internal)?,
        transport_security: state.transport_security(),
    }))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let session = web_mutation(&state, &headers).await?;
    state.store.revoke_session(&session.id).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::clear_session_cookie(state.config.is_secure()))
            .map_err(ApiError::internal)?,
    );
    Ok(response)
}

async fn create_pairing_code(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PairingCodeView>, ApiError> {
    let session = web_mutation(&state, &headers).await?;
    let code = random_pairing_code();
    let (id, expires) = state
        .store
        .create_pairing_code(
            &session.user_id,
            &auth::hash_secret(&code),
            state.config.pairing_code_ttl_minutes,
        )
        .await?;
    Ok(Json(PairingCodeView {
        id,
        code,
        expires_at: unix_to_rfc3339(expires).map_err(ApiError::internal)?,
    }))
}

async fn pair_device(
    State(state): State<AppState>,
    Json(request): Json<PairDeviceRequest>,
) -> Result<(StatusCode, Json<PairDeviceResponse>), ApiError> {
    bounded_nonempty("code", &request.code, 128)?;
    bounded_nonempty("displayName", &request.display_name, 128)?;
    bounded_nonempty("agentVersion", &request.agent_version, 128)?;
    bounded_nonempty("osFamily", &request.os_family, 64)?;
    bounded_nonempty("architecture", &request.architecture, 64)?;
    if request.public_key.len() > 64 {
        return Err(ApiError::BadRequest("publicKey is too long".into()));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&request.public_key)
        .map_err(|_| ApiError::BadRequest("invalid public key".into()))?;
    if decoded.len() != 32 {
        return Err(ApiError::BadRequest("public key must be 32 bytes".into()));
    }
    let device_id = state
        .store
        .pair_device(&request, &auth::hash_secret(&request.code))
        .await
        .map_err(|_| ApiError::BadRequest("invalid or expired pairing code".into()))?;
    Ok((StatusCode::CREATED, Json(PairDeviceResponse { device_id })))
}

async fn device_challenge(
    State(state): State<AppState>,
    Json(request): Json<ChallengeRequest>,
) -> Result<Json<ChallengeResponse>, ApiError> {
    bounded_nonempty("deviceId", &request.device_id, 128)?;
    let nonce = random_secret(32);
    let (challenge_id, expires) = state
        .store
        .create_challenge(&request.device_id, &nonce)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    Ok(Json(ChallengeResponse {
        challenge_id,
        nonce,
        expires_at: unix_to_rfc3339(expires).map_err(ApiError::internal)?,
    }))
}

async fn device_token(
    State(state): State<AppState>,
    Json(request): Json<DeviceTokenRequest>,
) -> Result<Json<DeviceTokenResponse>, ApiError> {
    bounded_nonempty("deviceId", &request.device_id, 128)?;
    bounded_nonempty("challengeId", &request.challenge_id, 128)?;
    bounded_nonempty("signature", &request.signature, 128)?;
    let (nonce, device) = state
        .store
        .challenge_for_token(&request.challenge_id, &request.device_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    auth::verify_device_signature(&device.public_key, &nonce, &request.signature)?;
    let access_token = random_secret(32);
    let expires = state
        .store
        .consume_challenge_and_create_token(
            &request.challenge_id,
            &device,
            &auth::hash_secret(&access_token),
            state.config.device_token_ttl_minutes,
        )
        .await?;
    Ok(Json(DeviceTokenResponse {
        access_token,
        expires_at: unix_to_rfc3339(expires).map_err(ApiError::internal)?,
    }))
}

async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceSummary>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    let mut devices = state.store.list_devices(&session.user_id).await?;
    for device in &mut devices {
        if device.status != DeviceStatus::Revoked && state.tunnels.is_online(&device.id).await {
            device.status = DeviceStatus::Online;
        }
    }
    Ok(Json(devices))
}

async fn get_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<Json<DeviceSummary>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    let mut device = state
        .store
        .list_devices(&session.user_id)
        .await?
        .into_iter()
        .find(|device| device.id == device_id)
        .ok_or(ApiError::NotFound)?;
    if device.status != DeviceStatus::Revoked && state.tunnels.is_online(&device.id).await {
        device.status = DeviceStatus::Online;
    }
    Ok(Json(device))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameDeviceRequest {
    display_name: String,
}

async fn rename_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(request): Json<RenameDeviceRequest>,
) -> Result<Json<DeviceSummary>, ApiError> {
    let session = web_mutation(&state, &headers).await?;
    let display_name = bounded_nonempty("displayName", &request.display_name, 128)?.to_owned();
    if !state
        .store
        .rename_device(&session.user_id, &device_id, &display_name)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    state
        .store
        .append_audit(
            Some(&session.user_id),
            "device.renamed",
            Some(&device_id),
            &serde_json::json!({"displayName": &display_name}),
        )
        .await?;
    tunnel::publish_device_event(
        &state,
        &session.user_id,
        &device_id,
        "device.renamed",
        serde_json::json!({"displayName": &display_name}),
    )
    .await?;
    // The database is authoritative. Updated clients apply this snapshot immediately;
    // offline and older clients reconcile from Welcome after their next upgrade/connect.
    if let Err(error) = state
        .tunnels
        .sync_display_name(&device_id, &display_name)
        .await
    {
        tracing::warn!(%device_id, error = ?error, "device name push failed; reconnecting for reconciliation");
    }
    get_device(State(state), headers, Path(device_id)).await
}

async fn refresh_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    require_device_owner(&state, &session, &device_id).await?;
    enqueue(
        &state,
        &headers,
        &session,
        device_id,
        None,
        None,
        DeviceCommandKind::Refresh,
    )
    .await
}

async fn sync_device_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    require_device_owner(&state, &session, &device_id).await?;
    enqueue(
        &state,
        &headers,
        &session,
        device_id,
        None,
        None,
        DeviceCommandKind::HistorySync { thread_id: None },
    )
    .await
}

async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let session = web_mutation(&state, &headers).await?;
    if !state
        .store
        .revoke_device(&session.user_id, &device_id)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    state.tunnels.disconnect(&device_id).await;
    state
        .store
        .append_audit(
            Some(&session.user_id),
            "device.revoked",
            Some(&device_id),
            &serde_json::json!({}),
        )
        .await?;
    let _ = tunnel::publish_device_event(
        &state,
        &session.user_id,
        &device_id,
        "device.revoked",
        serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn directory_roots(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    require_device_owner(&state, &session, &device_id).await?;
    let result = state
        .tunnels
        .query(&device_id, DeviceQuery::DirectoryRoots)
        .await
        .map_err(query_error)?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryQueryParams {
    parent_ref: String,
    cursor: Option<String>,
}
async fn directory_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Query(query): Query<DirectoryQueryParams>,
) -> Result<Json<Value>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    require_device_owner(&state, &session, &device_id).await?;
    bounded_nonempty("parentRef", &query.parent_ref, 256)?;
    if query
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > 32)
    {
        return Err(ApiError::BadRequest("cursor is too long".into()));
    }
    let result = state
        .tunnels
        .query(
            &device_id,
            DeviceQuery::DirectoryList {
                parent_ref: query.parent_ref,
                cursor: query.cursor,
            },
        )
        .await
        .map_err(query_error)?;
    Ok(Json(result))
}

async fn list_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<Json<Vec<ProjectSummary>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    require_device_owner(&state, &session, &device_id).await?;
    Ok(Json(
        state
            .store
            .list_projects(&session.user_id, &device_id)
            .await?,
    ))
}

async fn create_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    require_device_owner(&state, &session, &device_id).await?;
    enqueue(
        &state,
        &headers,
        &session,
        device_id,
        None,
        None,
        DeviceCommandKind::ProjectCreate(request),
    )
    .await
}

async fn delete_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((device_id, project_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    if !state
        .store
        .project_accepts_commands(&session.user_id, &device_id, &project_id)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    enqueue(
        &state,
        &headers,
        &session,
        device_id,
        Some(project_id.clone()),
        None,
        DeviceCommandKind::ProjectDelete { project_id },
    )
    .await
}

async fn list_project_threads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((device_id, project_id)): Path<(String, String)>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    if !state
        .store
        .project_belongs_to_user(&session.user_id, &device_id, &project_id)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    Ok(Json(
        state
            .store
            .list_threads(
                &session.user_id,
                Some(&device_id),
                Some(&project_id),
                query.limit.unwrap_or(100).clamp(1, 500),
                query.offset.unwrap_or(0).clamp(0, 1_000_000),
            )
            .await?,
    ))
}

async fn create_thread(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((device_id, project_id)): Path<(String, String)>,
    Json(request): Json<CreateThreadRequest>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    if !state
        .store
        .project_accepts_commands(&session.user_id, &device_id, &project_id)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    enqueue(
        &state,
        &headers,
        &session,
        device_id,
        Some(project_id.clone()),
        None,
        DeviceCommandKind::ThreadCreate {
            project_id,
            request,
        },
    )
    .await
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}
async fn list_all_threads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    Ok(Json(
        state
            .store
            .list_threads(
                &session.user_id,
                None,
                None,
                query.limit.unwrap_or(100).clamp(1, 500),
                query.offset.unwrap_or(0).clamp(0, 1_000_000),
            )
            .await?,
    ))
}

async fn history_turns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(thread_id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<HistoryTurnView>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    if state
        .store
        .thread_belongs_to_user(&session.user_id, &thread_id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    Ok(Json(
        state
            .store
            .history_turns(
                &session.user_id,
                &thread_id,
                query.limit.unwrap_or(200).clamp(1, 1000),
                query.offset.unwrap_or(0).clamp(0, 1_000_000),
            )
            .await?,
    ))
}

async fn history_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(turn_id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<HistoryItemView>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    Ok(Json(
        state
            .store
            .history_items(
                &session.user_id,
                &turn_id,
                query.limit.unwrap_or(500).clamp(1, 2000),
                query.offset.unwrap_or(0).clamp(0, 1_000_000),
            )
            .await?,
    ))
}

async fn start_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(thread_id): Path<String>,
    Json(request): Json<StartTurnRequest>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    let (device, project) = state
        .store
        .thread_command_target(&session.user_id, &thread_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    enqueue(
        &state,
        &headers,
        &session,
        device,
        Some(project),
        Some(thread_id.clone()),
        DeviceCommandKind::TurnStart { thread_id, request },
    )
    .await
}

async fn steer_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(thread_id): Path<String>,
    Json(request): Json<TextInputRequest>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    command_for_thread(&state, &headers, thread_id, |thread_id| {
        DeviceCommandKind::TurnSteer { thread_id, request }
    })
    .await
}
async fn interrupt_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(thread_id): Path<String>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    command_for_thread(&state, &headers, thread_id, |thread_id| {
        DeviceCommandKind::TurnInterrupt { thread_id }
    })
    .await
}
async fn archive_thread(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(thread_id): Path<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    command_for_thread(&state, &headers, thread_id, |thread_id| {
        DeviceCommandKind::ThreadArchive {
            thread_id,
            archived: request.archived,
        }
    })
    .await
}

#[derive(Deserialize)]
struct ArchiveRequest {
    archived: bool,
}

async fn sync_thread_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(thread_id): Path<String>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    command_for_thread(&state, &headers, thread_id.clone(), move |_| {
        DeviceCommandKind::HistorySync {
            thread_id: Some(thread_id),
        }
    })
    .await
}

async fn command_for_thread<F>(
    state: &AppState,
    headers: &HeaderMap,
    thread_id: String,
    make: F,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError>
where
    F: FnOnce(String) -> DeviceCommandKind,
{
    let session = web_mutation(state, headers).await?;
    let (device, project) = state
        .store
        .thread_command_target(&session.user_id, &thread_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    enqueue(
        state,
        headers,
        &session,
        device,
        Some(project),
        Some(thread_id.clone()),
        make(thread_id),
    )
    .await
}

async fn decide_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(approval_id): Path<String>,
    Json(request): Json<ApprovalDecisionRequest>,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let session = web_mutation(&state, &headers).await?;
    let device_id = state
        .store
        .approval_device(&session.user_id, &approval_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    enqueue(
        &state,
        &headers,
        &session,
        device_id,
        None,
        None,
        DeviceCommandKind::ApprovalDecide {
            approval_id,
            request,
        },
    )
    .await
}

#[derive(Deserialize)]
struct ApprovalQuery {
    pending: Option<bool>,
}
async fn list_approvals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ApprovalQuery>,
) -> Result<Json<Vec<ApprovalView>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    Ok(Json(
        state
            .store
            .list_approvals(&session.user_id, query.pending.unwrap_or(true))
            .await?,
    ))
}

async fn command_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(command_id): Path<String>,
) -> Result<Json<CommandView>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    Ok(Json(
        state
            .store
            .command_view(&session.user_id, &command_id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<i64>,
}
async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let session = auth::authenticate_web(&state.store, &headers).await?;
    let header_after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let after = header_after.or(query.after).unwrap_or(0);
    let mut receiver = state.events.subscribe();
    let (minimum, _) = state.store.event_bounds(&session.user_id).await?;
    let mut resync_required = after > 0
        && match minimum {
            Some(minimum) => after.saturating_add(1) < minimum,
            None => true,
        };
    let mut replay = if resync_required {
        Vec::new()
    } else {
        state
            .store
            .replay_events(&session.user_id, after, 10_001)
            .await?
    };
    if replay.len() > 10_000 {
        replay.clear();
        resync_required = true;
    }
    let user_id = session.user_id;
    let output = stream! {
        if resync_required { yield Ok(Event::default().event("resync_required").data("{}")); }
        for(cursor,event)in replay{yield Ok(Event::default().id(cursor.to_string()).event("nuntius").json_data(event).expect("serializable event"));}
        loop{match receiver.recv().await{Ok(PublishedEvent{cursor,user_id:event_user,event})if event_user==user_id=>yield Ok(Event::default().id(cursor.to_string()).event("nuntius").json_data(event).expect("serializable event")),Ok(_)=>{},Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{yield Ok(Event::default().event("resync_required").data("{}"));break},Err(_)=>break}}
    };
    Ok(Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(StdDuration::from_secs(15))
            .text("keepalive"),
    ))
}

async fn device_tunnel(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let token = auth::bearer_token(&headers).ok_or(ApiError::Unauthorized)?;
    let device = state
        .store
        .device_by_access_token_hash(&auth::hash_secret(token))
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let state_for_socket = state.clone();
    let device_id = device.device_id;
    let user_id = device.user_id;
    Ok(ws
        .max_message_size(2 * 1024 * 1024)
        .max_frame_size(2 * 1024 * 1024)
        .protocols([DEVICE_SUBPROTOCOL])
        .on_upgrade(move |socket: WebSocket| {
            tunnel::serve_socket(socket, state_for_socket, device_id, user_id)
        }))
}

async fn enqueue(
    state: &AppState,
    headers: &HeaderMap,
    session: &SessionRecord,
    device_id: String,
    project_id: Option<String>,
    thread_id: Option<String>,
    kind: DeviceCommandKind,
) -> Result<(StatusCode, Json<CommandReceipt>), ApiError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or_else(|| ApiError::BadRequest("Idempotency-Key header is required".into()))?;
    validate_device_command(&kind)?;
    let issued = now();
    let expiry = OffsetDateTime::now_utc() + Duration::minutes(5);
    let command_id = new_id("cmd");
    let command = DeviceCommand {
        command_id: command_id.clone(),
        device_id: device_id.clone(),
        project_id,
        thread_id,
        issued_at: issued.clone(),
        expires_at: expiry
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(ApiError::internal)?,
        command: kind,
    };
    // The fingerprint deliberately excludes generated command metadata. A browser retry
    // receives a new command id and timestamps before deduplication, but must still map to
    // the original durable command when its business request is identical.
    let serialized = serde_json::to_vec(&serde_json::json!({
        "deviceId": &command.device_id,
        "projectId": &command.project_id,
        "threadId": &command.thread_id,
        "command": &command.command,
    }))
    .map_err(ApiError::internal)?;
    let fingerprint = hex::encode(Sha256::digest(serialized));
    let existing = state
        .store
        .command_by_idempotency(&session.user_id, &device_id, key, &fingerprint)
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let stored = if let Some(stored) = existing {
        stored
    } else {
        // Reject a new side effect when the device is already unavailable. Once the
        // durable insert commits, a racing disconnect is recovered by tunnel replay.
        if !state
            .store
            .device_is_active_for_user(&session.user_id, &device_id)
            .await?
        {
            return Err(ApiError::NotFound);
        }
        if !state.tunnels.is_online(&device_id).await {
            return Err(ApiError::DeviceOffline);
        }
        state
            .store
            .insert_command(
                &session.user_id,
                key,
                &fingerprint,
                &command,
                expiry.unix_timestamp(),
            )
            .await
            .map_err(|e| ApiError::Conflict(e.to_string()))?
    };
    if stored.newly_created {
        state
            .store
            .mark_command_waiting(&stored.command.command_id)
            .await?;
        if state.tunnels.is_online(&device_id).await {
            // The SQLite commit is the acceptance boundary. A disconnect here is recovered by
            // pending-command replay and must not turn an accepted command into an HTTP error.
            let _ = state
                .tunnels
                .send(
                    &device_id,
                    TunnelFrame::Command {
                        queue_epoch: stored.queue_epoch.clone(),
                        server_sequence: stored.sequence,
                        command: stored.command.clone(),
                    },
                )
                .await;
        }
    }
    let command_id = stored.command.command_id.clone();
    Ok((
        StatusCode::ACCEPTED,
        Json(CommandReceipt {
            command_id: command_id.clone(),
            status: stored.status,
            accepted_at: stored.command.issued_at,
            status_url: format!("/api/v1/commands/{command_id}"),
        }),
    ))
}

async fn web_mutation(state: &AppState, headers: &HeaderMap) -> Result<SessionRecord, ApiError> {
    let session = auth::authenticate_web(&state.store, headers).await?;
    auth::require_csrf(&state.store, headers, &session).await?;
    Ok(session)
}
async fn require_device_owner(
    state: &AppState,
    session: &SessionRecord,
    device_id: &str,
) -> Result<(), ApiError> {
    if state.store.user_id_for_device(device_id).await?.as_deref() != Some(&session.user_id) {
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}
fn query_error(code: String) -> ApiError {
    match code.as_str() {
        "device_offline" => ApiError::DeviceOffline,
        "query_timeout" => ApiError::Unavailable("device directory query timed out".into()),
        _ => ApiError::BadRequest(code),
    }
}
fn random_pairing_code() -> String {
    let mut bytes = [0u8; 6];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(bytes)
        .to_uppercase()
}

fn bounded_nonempty<'a>(field: &str, value: &'a str, maximum: usize) -> Result<&'a str, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > maximum {
        return Err(ApiError::BadRequest(format!(
            "{field} must contain 1 to {maximum} bytes"
        )));
    }
    Ok(value)
}

fn bounded_json(field: &str, value: &Value, maximum: usize) -> Result<(), ApiError> {
    let length = serde_json::to_vec(value).map_err(ApiError::internal)?.len();
    if length > maximum {
        return Err(ApiError::BadRequest(format!(
            "{field} must not exceed {maximum} bytes"
        )));
    }
    Ok(())
}

fn validate_device_command(kind: &DeviceCommandKind) -> Result<(), ApiError> {
    match kind {
        DeviceCommandKind::ProjectCreate(request) => {
            bounded_nonempty("directoryRef", &request.directory_ref, 256)?;
            bounded_nonempty("displayName", &request.display_name, 128)?;
            bounded_json("defaults", &request.defaults, 64 * 1024)?;
        }
        DeviceCommandKind::ProjectDelete { project_id } => {
            bounded_nonempty("projectId", project_id, 128)?;
        }
        DeviceCommandKind::ThreadCreate { request, .. } => {
            if let Some(title) = &request.title {
                bounded_nonempty("title", title, 256)?;
            }
            if let Some(message) = &request.first_message {
                bounded_nonempty("firstMessage", message, 256 * 1024)?;
            }
            bounded_json("options", &request.options, 64 * 1024)?;
        }
        DeviceCommandKind::TurnStart { request, .. } => {
            bounded_nonempty("text", &request.text, 256 * 1024)?;
            bounded_json("options", &request.options, 64 * 1024)?;
        }
        DeviceCommandKind::TurnSteer { request, .. } => {
            bounded_nonempty("text", &request.text, 256 * 1024)?;
        }
        DeviceCommandKind::ApprovalDecide { request, .. } => {
            if !matches!(
                request.decision.as_str(),
                "accept" | "accept_for_session" | "decline" | "cancel"
            ) && request.response.is_none()
            {
                return Err(ApiError::BadRequest("unsupported approval decision".into()));
            }
            if let Some(response) = &request.response {
                bounded_json("response", response, 128 * 1024)?;
            }
        }
        DeviceCommandKind::Refresh
        | DeviceCommandKind::ThreadArchive { .. }
        | DeviceCommandKind::TurnInterrupt { .. }
        | DeviceCommandKind::HistorySync { .. } => {}
    }
    Ok(())
}
