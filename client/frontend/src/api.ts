/* Local console API client (loopback CLI service, /api/v1).
 * No auth, no CSRF; mutations are synchronous and return the result. */
import type {
  ClientInfo,
  AgentProvider,
  DirectoryListResponse,
  HistoryRecord,
  ProjectSummary,
  SyncSnapshot,
  ThreadSummary,
  AgentProvider,
} from "@nuntius/shared";

export class ApiError extends Error {
  code: string;
  status: number;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function parseError(res: Response): Promise<ApiError> {
  try {
    const body = await res.json();
    const e = body?.error;
    if (e) return new ApiError(res.status, e.code ?? "error", e.message ?? res.statusText);
  } catch {
    /* fall through */
  }
  if (res.status === 404) return new ApiError(404, "not_found", "资源不存在");
  return new ApiError(res.status, "error", `请求失败（${res.status}）`);
}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`/api/v1${path}`, {
      method,
      credentials: "same-origin",
      headers: body === undefined ? {} : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, "network", "无法连接本地服务，请确认 nuntius-client 正在运行");
  }
  if (!res.ok) throw await parseError(res);
  return (await res.json()) as T;
}

export const api = {
  info: () => req<ClientInfo>("GET", "/info"),
  sync: () => req<SyncSnapshot>("GET", "/sync"),
  directoryRoots: () => req<DirectoryListResponse>("GET", "/directories/roots"),
  directories: (parentRef: string, cursor?: string) =>
    req<DirectoryListResponse>(
      "GET",
      `/directories?parentRef=${encodeURIComponent(parentRef)}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`,
    ),
  projects: () => req<ProjectSummary[]>("GET", "/projects"),
  createProject: (directoryRef: string, displayName: string) =>
    req<ProjectSummary>("POST", "/projects", { directoryRef, displayName, defaults: {} }),
  deleteProject: (projectId: string) =>
    req<{ projectId: string; threadCount: number }>("DELETE", `/projects/${projectId}`),
  projectThreads: (projectId: string) =>
    req<ThreadSummary[]>("GET", `/projects/${projectId}/threads`),
  createThread: (projectId: string, title: string | null, provider: AgentProvider = "codex") =>
    req<{ threadId: string; appServerThreadId: string }>(
      "POST",
      `/projects/${projectId}/threads`,
      { title, firstMessage: null, provider, accessMode: "full", options: {} },
    ),
  threads: () => req<ThreadSummary[]>("GET", "/threads"),
  history: (threadId: string) => req<HistoryRecord[]>("GET", `/threads/${threadId}/history`),
  startTurn: (threadId: string, text: string) =>
    req<{ operation: "start" | "steer"; turnId?: string }>("POST", `/threads/${threadId}/turns`, {
      text,
      accessMode: "full",
      options: {},
    }),
  steerTurn: (threadId: string, text: string) =>
    req<unknown>("POST", `/threads/${threadId}/steer`, { text }),
  interruptTurn: (threadId: string) => req<unknown>("POST", `/threads/${threadId}/interrupt`),
  archiveThread: (threadId: string, archived: boolean) =>
    req<unknown>("POST", `/threads/${threadId}/archive`, { archived }),
  decideApproval: (approvalId: string, decision: string) =>
    req<unknown>("POST", `/approvals/${approvalId}/decision`, { decision }),
};
