#![allow(dead_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DEVICE_PROTOCOL_VERSION: u16 = 1;
pub const DEVICE_SUBPROTOCOL: &str = "nuntius.device.v1";

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7())
}

pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339")
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportSecurity {
    Secure,
    Insecure,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub api_version: String,
    pub server_version: String,
    pub build_sha: String,
    pub release_sequence: u64,
    pub transport_security: TransportSecurity,
    pub initialized: bool,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorDetail {
    pub code: String,
    pub message: String,
    pub request_id: String,
    pub retryable: bool,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapRequest {
    pub bootstrap_token: String,
    pub login_name: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub login_name: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebSessionView {
    pub user_id: String,
    pub login_name: String,
    pub csrf_token: String,
    pub expires_at: String,
    pub transport_security: TransportSecurity,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairingCodeView {
    pub id: String,
    pub code: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairDeviceRequest {
    pub code: String,
    pub display_name: String,
    pub public_key: String,
    pub agent_version: String,
    pub os_family: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairDeviceResponse {
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    pub challenge_id: String,
    pub nonce: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTokenRequest {
    pub device_id: String,
    pub challenge_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTokenResponse {
    pub access_token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    Pairing,
    Syncing,
    Online,
    Degraded,
    Offline,
    Revoked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryCompleteness {
    NotStarted,
    Backfilling,
    Complete,
    Partial,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub id: String,
    pub display_name: String,
    pub status: DeviceStatus,
    pub last_seen_at: Option<String>,
    pub agent_version: Option<String>,
    pub codex_version: Option<String>,
    pub os_family: Option<String>,
    pub architecture: Option<String>,
    pub project_count: i64,
    pub active_turn_count: i64,
    pub pending_approval_count: i64,
    pub history_completeness: HistoryCompleteness,
    pub history_last_synced_at: Option<String>,
    pub transport_security: Option<TransportSecurity>,
    pub app_server_status: Option<String>,
    pub storage_status: Option<String>,
    pub inbox_depth: i64,
    pub outbox_depth: i64,
    pub history_backfill_depth: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Workspace,
    SystemUnassigned,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub device_id: String,
    pub kind: ProjectKind,
    pub display_name: String,
    pub path_hint: Option<String>,
    pub status: String,
    pub repo_name: Option<String>,
    pub branch: Option<String>,
    pub is_dirty: Option<bool>,
    pub thread_count: i64,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: String,
    pub device_id: String,
    pub project_id: String,
    pub app_server_thread_id: Option<String>,
    pub title: String,
    pub status: String,
    pub archived: bool,
    pub history_completeness: HistoryCompleteness,
    pub last_synced_at: Option<String>,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryTurnView {
    pub id: String,
    pub thread_id: String,
    pub ordinal: i64,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItemView {
    pub id: String,
    pub turn_id: String,
    pub ordinal: i64,
    pub kind: String,
    pub status: String,
    pub revision: i64,
    pub content_text: Option<String>,
    pub structured_detail: Option<Value>,
    pub is_truncated: bool,
    pub occurred_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Accepted,
    WaitingDevice,
    DeviceAccepted,
    Applying,
    Completed,
    Failed,
    Rejected,
    Unknown,
    Expired,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::WaitingDevice => "waiting_device",
            Self::DeviceAccepted => "device_accepted",
            Self::Applying => "applying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    pub command_id: String,
    pub status: CommandStatus,
    pub accepted_at: String,
    pub status_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandView {
    pub id: String,
    pub device_id: String,
    pub status: CommandStatus,
    pub kind: String,
    pub accepted_at: String,
    pub completed_at: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalView {
    pub id: String,
    pub device_id: String,
    pub project_id: Option<String>,
    pub thread_id: Option<String>,
    pub method: String,
    pub params: Value,
    pub status: String,
    pub requested_at: String,
    pub decided_at: Option<String>,
    pub decision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshot {
    pub cursor: i64,
    pub generated_at: String,
    pub devices: Vec<DeviceSummary>,
    pub projects: Vec<ProjectSummary>,
    pub threads: Vec<ThreadSummary>,
    pub approvals: Vec<ApprovalView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectRequest {
    pub directory_ref: String,
    pub display_name: String,
    #[serde(default)]
    pub defaults: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateThreadRequest {
    pub title: Option<String>,
    pub first_message: Option<String>,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartTurnRequest {
    pub text: String,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextInputRequest {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionRequest {
    pub decision: String,
    #[serde(default)]
    pub response: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub name: String,
    pub directory_ref: String,
    pub breadcrumb: Vec<String>,
    pub has_children: bool,
    pub git_kind: Option<String>,
    pub project_id: Option<String>,
    pub selectable: bool,
    pub symlink: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListResponse {
    pub device_id: String,
    pub parent_name: Option<String>,
    pub breadcrumb: Vec<String>,
    pub entries: Vec<DirectoryEntry>,
    pub next_cursor: Option<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    pub thread: Option<ThreadSummary>,
    pub turn: Option<HistoryTurnView>,
    pub item: Option<HistoryItemView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBatch {
    pub batch_id: String,
    pub device_id: String,
    pub thread_id: String,
    pub from_cursor: Option<String>,
    pub to_cursor: String,
    pub inventory_revision: i64,
    pub payload_hash: String,
    #[serde(default)]
    pub complete: bool,
    pub records: Vec<HistoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "payload",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DeviceCommandKind {
    Refresh,
    ProjectCreate(CreateProjectRequest),
    ProjectDelete {
        project_id: String,
    },
    ThreadCreate {
        project_id: String,
        request: CreateThreadRequest,
    },
    ThreadArchive {
        thread_id: String,
        archived: bool,
    },
    TurnStart {
        thread_id: String,
        request: StartTurnRequest,
    },
    TurnSteer {
        thread_id: String,
        request: TextInputRequest,
    },
    TurnInterrupt {
        thread_id: String,
    },
    ApprovalDecide {
        approval_id: String,
        request: ApprovalDecisionRequest,
    },
    HistorySync {
        thread_id: Option<String>,
    },
}

impl DeviceCommandKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Refresh => "device.refresh",
            Self::ProjectCreate(_) => "project.create",
            Self::ProjectDelete { .. } => "project.delete",
            Self::ThreadCreate { .. } => "thread.create",
            Self::ThreadArchive { archived: true, .. } => "thread.archive",
            Self::ThreadArchive {
                archived: false, ..
            } => "thread.unarchive",
            Self::TurnStart { .. } => "turn.start",
            Self::TurnSteer { .. } => "turn.steer",
            Self::TurnInterrupt { .. } => "turn.interrupt",
            Self::ApprovalDecide { .. } => "approval.decide",
            Self::HistorySync { .. } => "history.sync",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCommand {
    pub command_id: String,
    pub device_id: String,
    pub project_id: Option<String>,
    pub thread_id: Option<String>,
    pub issued_at: String,
    pub expires_at: String,
    pub command: DeviceCommandKind,
}

fn legacy_queue_epoch() -> String {
    "legacy".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "payload",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DeviceQuery {
    DirectoryRoots,
    DirectoryList {
        parent_ref: String,
        cursor: Option<String>,
    },
    Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NuntiusEvent {
    pub event_id: String,
    pub user_id: Option<String>,
    pub device_id: String,
    pub project_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub stream_id: String,
    pub seq: i64,
    pub event_type: String,
    pub durability: String,
    pub occurred_at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceHealth {
    pub app_server_status: String,
    pub storage_status: String,
    pub inbox_depth: i64,
    pub outbox_depth: i64,
    pub history_backfill_depth: i64,
    pub active_turn_count: i64,
    pub pending_approval_count: i64,
    pub project_count: i64,
    pub codex_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TunnelFrame {
    Hello {
        protocol_version: u16,
        device_id: String,
        instance_id: String,
        agent_version: String,
        transport_security: TransportSecurity,
        last_server_command_seq: i64,
        #[serde(default)]
        command_queue_epoch: Option<String>,
        event_acks: BTreeMap<String, i64>,
        history_cursors: BTreeMap<String, String>,
        capabilities: Vec<String>,
    },
    Welcome {
        protocol_version: u16,
        connection_id: String,
        connection_epoch: i64,
        #[serde(default = "legacy_queue_epoch")]
        command_queue_epoch: String,
        server_time: String,
        transport_security: TransportSecurity,
        capabilities: Vec<String>,
    },
    Command {
        #[serde(default = "legacy_queue_epoch")]
        queue_epoch: String,
        server_sequence: i64,
        command: DeviceCommand,
    },
    CommandAck {
        command_id: String,
        stage: String,
        result: Option<Value>,
        error_code: Option<String>,
        #[serde(default)]
        error_message: Option<String>,
    },
    Event {
        event: NuntiusEvent,
    },
    EventAck {
        event_id: String,
    },
    HistoryBatch {
        batch: HistoryBatch,
    },
    HistoryAck {
        batch_id: String,
        thread_id: String,
        acked_cursor: String,
    },
    Query {
        correlation_id: String,
        query: DeviceQuery,
    },
    QueryResponse {
        correlation_id: String,
        result: Option<Value>,
        error_code: Option<String>,
    },
    Heartbeat {
        sent_at: String,
        health: DeviceHealth,
    },
    HeartbeatAck {
        received_at: String,
    },
    ServerNotice {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_delete_wire_tag_is_stable() {
        let value = serde_json::to_value(DeviceCommandKind::ProjectDelete {
            project_id: "prj_test".into(),
        })
        .unwrap();
        assert_eq!(value["kind"], "project_delete");
        assert_eq!(value["payload"]["projectId"], "prj_test");
    }

    #[test]
    fn tunnel_tags_are_stable() {
        let value = serde_json::to_value(TunnelFrame::ServerNotice {
            code: "x".into(),
            message: "y".into(),
        })
        .unwrap();
        assert_eq!(value["type"], "server_notice");
    }
}
