/* Protocol types shared by the remote (server) and local (client) consoles.
 * Field names match the camelCase JSON produced by the Rust backends. */

export type TransportSecurity = "secure" | "insecure" | "local";
export type AgentProvider = "codex" | "kimi";

export interface AgentProviderStatus {
  provider: AgentProvider;
  label: string;
  available: boolean;
  status: string;
  version: string | null;
}

export interface ServerInfo {
  apiVersion: string;
  serverVersion: string;
  buildSha: string;
  transportSecurity: TransportSecurity;
  initialized: boolean;
  capabilities: string[];
}

export interface ClientInfo {
  apiVersion: string;
  clientVersion: string;
  buildSha: string;
  deviceId: string;
  paired: boolean;
  localBind: string;
  appServerRunning: boolean;
  providers: AgentProviderStatus[];
  projects: number;
  pendingCommands: number;
  pendingEvents: number;
  activeTurns: number;
  capabilities: string[];
}

export interface WebSessionView {
  userId: string;
  loginName: string;
  csrfToken: string;
  expiresAt: string;
  transportSecurity: TransportSecurity;
}

export interface PairingCodeView {
  id: string;
  code: string;
  expiresAt: string;
}

export type DeviceStatus =
  | "pairing"
  | "syncing"
  | "online"
  | "degraded"
  | "offline"
  | "revoked";

export type HistoryCompleteness =
  | "not_started"
  | "backfilling"
  | "complete"
  | "partial"
  | "error";

export interface DeviceSummary {
  id: string;
  displayName: string;
  status: DeviceStatus;
  lastSeenAt: string | null;
  agentVersion: string | null;
  codexVersion: string | null;
  osFamily: string | null;
  architecture: string | null;
  projectCount: number;
  activeTurnCount: number;
  pendingApprovalCount: number;
  historyCompleteness: HistoryCompleteness;
  historyLastSyncedAt: string | null;
  transportSecurity: TransportSecurity | null;
  providers: AgentProviderStatus[];
}

export type ProjectKind = "workspace" | "system_unassigned";

export interface ProjectSummary {
  id: string;
  deviceId: string;
  kind: ProjectKind;
  displayName: string;
  pathHint: string | null;
  status: string;
  repoName: string | null;
  branch: string | null;
  isDirty: boolean | null;
  threadCount: number;
  lastActivityAt: string | null;
}

export interface ThreadSummary {
  id: string;
  deviceId: string;
  projectId: string;
  provider: AgentProvider;
  appServerThreadId: string | null;
  title: string;
  status: string;
  archived: boolean;
  historyCompleteness: HistoryCompleteness;
  lastSyncedAt: string | null;
  lastActivityAt: string | null;
}

export interface HistoryTurnView {
  id: string;
  threadId: string;
  ordinal: number;
  status: string;
  startedAt: string | null;
  completedAt: string | null;
}

export interface HistoryItemView {
  id: string;
  turnId: string;
  ordinal: number;
  kind: string;
  status: string;
  revision: number;
  contentText: string | null;
  structuredDetail: unknown;
  isTruncated: boolean;
  occurredAt: string;
  completedAt: string | null;
  attachments: AttachmentView[];
}

export interface AttachmentView {
  id: string;
  originalName: string;
  mimeType: string;
  byteSize: number;
  sha256: string;
  width: number;
  height: number;
}

export interface HistoryRecord {
  thread: ThreadSummary | null;
  turn: HistoryTurnView | null;
  item: HistoryItemView | null;
}

export type CommandStatus =
  | "accepted"
  | "waiting_device"
  | "device_accepted"
  | "applying"
  | "completed"
  | "failed"
  | "rejected"
  | "unknown"
  | "expired";

export interface CommandReceipt {
  commandId: string;
  status: CommandStatus;
  acceptedAt: string;
  statusUrl: string;
}

export interface CommandView {
  id: string;
  deviceId: string;
  status: CommandStatus;
  kind: string;
  acceptedAt: string;
  completedAt: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  result: unknown;
}

/** Durable approval projection returned by both local and remote snapshots. */
export interface ApprovalSnapshot {
  id: string;
  deviceId: string;
  projectId: string | null;
  threadId: string | null;
  method: string;
  params: unknown;
  status: string;
  requestedAt: string;
  decidedAt: string | null;
  decision: string | null;
}

/** Database-backed baseline paired with an SSE replay cursor. */
export interface SyncSnapshot {
  cursor: number;
  generatedAt: string;
  devices: DeviceSummary[];
  projects: ProjectSummary[];
  threads: ThreadSummary[];
  approvals: ApprovalSnapshot[];
}

export interface DirectoryEntry {
  name: string;
  directoryRef: string;
  breadcrumb: string[];
  hasChildren: boolean;
  gitKind: string | null;
  projectId: string | null;
  selectable: boolean;
  symlink: boolean;
}

export interface DirectoryListResponse {
  deviceId: string;
  parentName: string | null;
  breadcrumb: string[];
  entries: DirectoryEntry[];
  nextCursor: string | null;
  expiresAt: string;
}

export interface NuntiusEvent<T = unknown> {
  eventId: string;
  userId: string | null;
  deviceId: string;
  projectId: string | null;
  threadId: string | null;
  turnId: string | null;
  streamId: string;
  seq: number;
  eventType: string;
  durability: string;
  occurredAt: string;
  payload: T;
}

export interface ApiErrorBody {
  error: {
    code: string;
    message: string;
    requestId: string;
    retryable: boolean;
    details: unknown;
  };
}

/* payload shapes for well-known events */
export interface TurnStartedPayload {
  text: string;
  attachments?: AttachmentView[];
  clientMessageId?: string | null;
}
export interface ApprovalRequestedPayload {
  approvalId: string;
  method: string;
  params: unknown;
}
export interface CommandStatusPayload {
  commandId: string;
  status: CommandStatus;
  kind?: string;
  threadId?: string | null;
  errorCode?: string | null;
  errorMessage?: string | null;
}
export interface HistoryProgressPayload {
  threadId: string;
  cursor: string;
}
