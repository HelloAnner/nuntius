/* Remote console API client (public server, /api/v1). */
import {
  newIdemKey,
  type AgentProvider,
  type ConversationAccessMode,
  type CommandReceipt,
  type CommandView,
  type DeviceSummary,
  type DirectoryListResponse,
  type HistoryItemView,
  type HistoryTurnView,
  type PairingCodeView,
  type ProjectSummary,
  type ServerInfo,
  type SyncSnapshot,
  type ThreadSummary,
  type WebSessionView,
} from "@nuntius/shared";

export class ApiError extends Error {
  code: string;
  status: number;
  retryable: boolean;
  constructor(status: number, code: string, message: string, retryable = false) {
    super(message);
    this.status = status;
    this.code = code;
    this.retryable = retryable;
  }
}

async function parseError(res: Response): Promise<ApiError> {
  try {
    const body = await res.json();
    const e = body?.error;
    if (e) return new ApiError(res.status, e.code ?? "error", e.message ?? res.statusText, e.retryable);
  } catch {
    /* fall through */
  }
  if (res.status === 401) return new ApiError(401, "unauthorized", "登录已过期");
  if (res.status === 403) return new ApiError(403, "forbidden", "没有权限执行此操作");
  if (res.status === 404) return new ApiError(404, "not_found", "资源不存在");
  if (res.status === 409) return new ApiError(409, "conflict", "当前状态冲突，请刷新后重试");
  if (res.status === 429) return new ApiError(429, "rate_limited", "请求过于频繁，请稍候", true);
  if (res.status === 503) return new ApiError(503, "unavailable", "服务暂不可用", true);
  return new ApiError(res.status, "error", `请求失败（${res.status}）`);
}

type CsrfProvider = () => string | null;
let csrfProvider: CsrfProvider = () => null;
export function setCsrfProvider(fn: CsrfProvider) {
  csrfProvider = fn;
}

function uploadAttachment(
  threadId: string,
  file: File,
  onProgress: (progress: number) => void,
): Promise<AttachmentView> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/api/v1/threads/${encodeURIComponent(threadId)}/attachments`);
    xhr.withCredentials = true;
    xhr.timeout = 90_000;
    const csrf = csrfProvider();
    if (csrf) xhr.setRequestHeader("x-csrf-token", csrf);
    xhr.setRequestHeader("idempotency-key", newIdemKey());
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) onProgress(Math.round((event.loaded / event.total) * 100));
    };
    xhr.onerror = () => reject(new ApiError(0, "network", "图片上传失败，请检查网络", true));
    xhr.ontimeout = () => reject(new ApiError(0, "timeout", "图片上传超时，请重试", true));
    xhr.onload = () => {
      let body: unknown = null;
      try { body = JSON.parse(xhr.responseText); } catch { /* handled below */ }
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(body as AttachmentView);
        return;
      }
      const detail = body as { error?: { code?: string; message?: string; retryable?: boolean } } | null;
      reject(new ApiError(
        xhr.status,
        detail?.error?.code ?? "upload_failed",
        detail?.error?.message ?? `图片上传失败（${xhr.status}）`,
        detail?.error?.retryable ?? false,
      ));
    };
    const form = new FormData();
    form.append("file", file, file.name);
    xhr.send(form);
  });
}

async function req<T>(
  method: string,
  path: string,
  body?: unknown,
  opts?: { idemKey?: string; deviceId?: string },
): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET") {
    const csrf = csrfProvider();
    if (csrf) headers["x-csrf-token"] = csrf;
    headers["idempotency-key"] = opts?.idemKey ?? newIdemKey();
  }
  if (opts?.deviceId) headers["x-nuntius-device-id"] = opts.deviceId;
  let res: Response;
  try {
    res = await fetch(`/api/v1${path}`, {
      method,
      headers,
      credentials: "same-origin",
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, "network", "网络连接失败", true);
  }
  if (!res.ok) throw await parseError(res);
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export const api = {
  info: () => req<ServerInfo>("GET", "/info"),
  sync: () => req<SyncSnapshot>("GET", "/sync"),
  session: () => req<WebSessionView>("GET", "/auth/session"),
  login: (loginName: string, password: string) =>
    req<WebSessionView>("POST", "/auth/login", { loginName, password }),
  bootstrap: (bootstrapToken: string, loginName: string, password: string) =>
    req<WebSessionView>("POST", "/auth/bootstrap", { bootstrapToken, loginName, password }),
  logout: () => req<void>("POST", "/auth/logout"),
  createPairingCode: () => req<PairingCodeView>("POST", "/pairing-codes"),

  devices: () => req<DeviceSummary[]>("GET", "/devices"),
  renameDevice: (deviceId: string, displayName: string) =>
    req<DeviceSummary>("PATCH", `/devices/${deviceId}`, { displayName }),
  revokeDevice: (deviceId: string) => req<void>("DELETE", `/devices/${deviceId}`),

  projects: (deviceId: string) =>
    req<ProjectSummary[]>("GET", `/devices/${deviceId}/projects`),
  createProject: (deviceId: string, directoryRef: string, displayName: string, idemKey: string) =>
    req<CommandReceipt>("POST", `/devices/${deviceId}/projects`, { directoryRef, displayName, defaults: {} }, { idemKey }),
  deleteProject: (deviceId: string, projectId: string, idemKey: string) =>
    req<CommandReceipt>("DELETE", `/devices/${deviceId}/projects/${projectId}`, undefined, { idemKey }),

  directoryRoots: (deviceId: string) =>
    req<DirectoryListResponse>("GET", `/devices/${deviceId}/directories/roots`),
  directories: (deviceId: string, parentRef: string, cursor?: string) =>
    req<DirectoryListResponse>(
      "GET",
      `/devices/${deviceId}/directories?parentRef=${encodeURIComponent(parentRef)}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`,
    ),

  projectThreads: (deviceId: string, projectId: string) =>
    req<ThreadSummary[]>("GET", `/devices/${deviceId}/projects/${projectId}/threads`),
  createThread: (deviceId: string, projectId: string, title: string | null, firstMessage: string | null, provider: AgentProvider, accessMode: ConversationAccessMode, idemKey: string) =>
    req<CommandReceipt>(
      "POST",
      `/devices/${deviceId}/projects/${projectId}/threads`,
      { title, firstMessage, provider, accessMode, options: {} },
      { idemKey },
    ),

  allThreads: (limit = 200) => req<ThreadSummary[]>("GET", `/threads?limit=${limit}`),
  historyTurns: (threadId: string, limit = 200) =>
    req<HistoryTurnView[]>("GET", `/threads/${threadId}/turns?limit=${limit}`),
  historyItems: (turnId: string, limit = 500) =>
    req<HistoryItemView[]>("GET", `/turns/${turnId}/items?limit=${limit}`),

  startTurn: (threadId: string, text: string, accessMode: ConversationAccessMode, idemKey: string) =>
    req<CommandReceipt>("POST", `/threads/${threadId}/turns`, { text, accessMode, options: {} }, { idemKey }),
  steerTurn: (threadId: string, text: string, idemKey: string) =>
    req<CommandReceipt>("POST", `/threads/${threadId}/steer`, { text }, { idemKey }),
  interruptTurn: (threadId: string) =>
    req<CommandReceipt>("POST", `/threads/${threadId}/interrupt`),
  archiveThread: (threadId: string, archived = true, idemKey?: string) =>
    req<CommandReceipt>("POST", `/threads/${threadId}/archive`, { archived }, { idemKey }),

  decideApproval: (deviceId: string, approvalId: string, decision: string, idemKey: string) =>
    req<CommandReceipt>(
      "POST",
      `/approvals/${approvalId}/decision`,
      { decision },
      { idemKey, deviceId },
    ),

  command: (commandId: string) => req<CommandView>("GET", `/commands/${commandId}`),
};
