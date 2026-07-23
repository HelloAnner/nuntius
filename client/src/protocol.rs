#![allow(dead_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DEVICE_PROTOCOL_VERSION: u16 = 1;
pub const DEVICE_SUBPROTOCOL: &str = "nuntius.device.v1";
pub const DEVICE_DISPLAY_NAME_SYNC_CAPABILITY: &str = "device-display-name-sync.v1";
pub const CLIENT_UPDATE_CAPABILITY: &str = "client-update.v1";
pub const STRICT_VERSION_CAPABILITY: &str = "strict-product-version.v1";
pub const PROVIDER_USAGE_CAPABILITY: &str = "provider-usage.v1";
pub const THREAD_RENAME_CAPABILITY: &str = "thread-rename.v1";
pub const THREAD_VIEW_STATE_CAPABILITY: &str = "thread-view-state.v1";

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
pub enum ProjectKind {
    Workspace,
    SystemUnassigned,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentProvider {
    #[default]
    Codex,
    Kimi,
    Pi,
}
impl AgentProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Kimi => "kimi",
            Self::Pi => "pi",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationAccessMode {
    #[default]
    Full,
    Ask,
}
impl ConversationAccessMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Ask => "ask",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderStatus {
    pub provider: AgentProvider,
    pub label: String,
    pub available: bool,
    pub status: String,
    pub version: Option<String>,
    #[serde(default)]
    pub models: Vec<AgentModelOption>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageAccount {
    pub external_account_id: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub scope: Option<String>,
    pub subscription_started_at: Option<String>,
    pub subscription_expires_at: Option<String>,
    pub subscription_last_checked_at: Option<String>,
    pub credential_expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuotaWindow {
    pub window_seconds: i64,
    pub used_percent: f64,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageWindows {
    pub five_hour: Option<ProviderQuotaWindow>,
    pub seven_day: Option<ProviderQuotaWindow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageCredits {
    pub balance: Option<f64>,
    pub reset_credits_available: Option<i64>,
    pub next_reset_credit_expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageReport {
    pub schema_version: u16,
    pub report_id: String,
    pub provider: AgentProvider,
    pub sampled_at: String,
    pub source: String,
    pub status: String,
    pub account: Option<ProviderUsageAccount>,
    pub entitlement_plan: Option<String>,
    #[serde(default)]
    pub windows: ProviderUsageWindows,
    pub credits: Option<ProviderUsageCredits>,
    pub warning_code: Option<String>,
    pub error_code: Option<String>,
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
pub struct ThreadSummary {
    pub id: String,
    pub device_id: String,
    pub project_id: String,
    #[serde(default)]
    pub provider: AgentProvider,
    pub app_server_thread_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub display_title_override: Option<String>,
    #[serde(default)]
    pub title_revision: i64,
    pub status: String,
    #[serde(default)]
    pub needs_review: bool,
    pub archived: bool,
    pub history_completeness: HistoryCompleteness,
    #[serde(default)]
    pub created_at: Option<String>,
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
    #[serde(default)]
    pub attachments: Vec<AttachmentView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub id: String,
    pub original_name: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRef {
    pub id: String,
    pub original_name: String,
    pub mime_type: String,
    pub extension: String,
    pub byte_size: i64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
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
    pub devices: Vec<Value>,
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
    pub provider: AgentProvider,
    #[serde(default)]
    pub access_mode: ConversationAccessMode,
    #[serde(default)]
    pub options: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartTurnRequest {
    pub text: String,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
    #[serde(default)]
    pub access_mode: ConversationAccessMode,
    #[serde(default)]
    pub options: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextInputRequest {
    pub text: String,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
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
    ProviderUsageRefresh,
    ProjectCreate(CreateProjectRequest),
    ProjectDelete {
        project_id: String,
    },
    ThreadCreate {
        project_id: String,
        request: CreateThreadRequest,
    },
    ThreadRename {
        thread_id: String,
        title: Option<String>,
    },
    ThreadArchive {
        thread_id: String,
        archived: bool,
    },
    ThreadMarkViewed {
        thread_id: String,
    },
    TurnStart {
        thread_id: String,
        request: StartTurnRequest,
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
    },
    TurnSteer {
        thread_id: String,
        request: TextInputRequest,
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
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
    #[serde(default)]
    pub providers: Vec<AgentProviderStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientRelease {
    pub release_id: String,
    pub product_version: String,
    pub commit_sha: String,
    pub release_sequence: u64,
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
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
        server_version: String,
        connection_id: String,
        connection_epoch: i64,
        #[serde(default = "legacy_queue_epoch")]
        command_queue_epoch: String,
        server_time: String,
        transport_security: TransportSecurity,
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    VersionMismatch {
        client_version: String,
        server_version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release: Option<ClientRelease>,
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
    DeviceConfig {
        display_name: String,
    },
    ClientUpdate {
        release: ClientRelease,
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
    fn provider_usage_refresh_wire_tag_is_stable() {
        let value = serde_json::to_value(DeviceCommandKind::ProviderUsageRefresh).unwrap();
        assert_eq!(value["kind"], "provider_usage_refresh");
        assert!(value.get("payload").is_none());
    }

    #[test]
    fn thread_mark_viewed_wire_tag_is_stable() {
        let value = serde_json::to_value(DeviceCommandKind::ThreadMarkViewed {
            thread_id: "thr_test".into(),
        })
        .unwrap();
        assert_eq!(value["kind"], "thread_mark_viewed");
        assert_eq!(value["payload"]["threadId"], "thr_test");
    }

    #[test]
    fn event_ack_wire_tag_is_stable() {
        let value = serde_json::to_value(TunnelFrame::EventAck {
            event_id: "evt_test".into(),
        })
        .unwrap();
        assert_eq!(value["type"], "event_ack");
        assert_eq!(value["payload"]["eventId"], "evt_test");
    }

    #[test]
    fn welcome_without_display_name_remains_compatible() {
        let frame: TunnelFrame = serde_json::from_value(serde_json::json!({
            "type": "welcome",
            "payload": {
                "protocolVersion": 1,
                "connectionId": "conn_test",
                "connectionEpoch": 1,
                "commandQueueEpoch": "epoch_test",
                "serverTime": "2026-01-01T00:00:00Z",
                "transportSecurity": "insecure",
                "capabilities": []
            }
        }))
        .unwrap();

        assert!(matches!(
            frame,
            TunnelFrame::Welcome {
                display_name: None,
                ..
            }
        ));
    }
}
