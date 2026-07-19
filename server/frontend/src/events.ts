/* User-level SSE manager: reconnect-aware event dispatch into the query
 * caches, the live thread store, approvals and command tracking. */
import { create } from "zustand";
import type { QueryClient } from "@tanstack/react-query";
import {
  type ApprovalRequestedPayload,
  type CommandStatusPayload,
  type DeviceSummary,
  type HistoryProgressPayload,
  type NuntiusEvent,
  type ProjectSummary,
  type SyncSnapshot,
  type ThreadSummary,
} from "@nuntius/shared";
import { api, ApiError } from "./api";
import { liveStore, useApprovals, useCommands } from "./stores";

export type SseStatus = "connecting" | "live" | "reconnecting" | "syncing";
interface SseState {
  status: SseStatus;
  set: (status: SseStatus) => void;
}
export const useSse = create<SseState>((set) => ({
  status: "connecting",
  set: (status) => set({ status }),
}));

const TERMINAL_CMD = new Set(["completed", "failed", "rejected", "unknown", "expired"]);

function applySnapshot(qc: QueryClient, snapshot: SyncSnapshot) {
  liveStore.reset();
  useApprovals.getState().replaceFromSnapshot(snapshot.approvals);
  qc.setQueryData(["devices"], snapshot.devices);
  qc.setQueryData(["allThreads"], snapshot.threads);

  for (const device of snapshot.devices) {
    qc.setQueryData(
      ["projects", device.id],
      snapshot.projects.filter((project) => project.deviceId === device.id),
    );
  }
  const projectIds = new Set(snapshot.projects.map((project) => project.id));
  for (const project of snapshot.projects) {
    qc.setQueryData(
      ["projectThreads", project.deviceId, project.id],
      snapshot.threads.filter((thread) => thread.projectId === project.id),
    );
  }
  for (const [key] of qc.getQueriesData({ queryKey: ["projectThreads"] })) {
    const projectId = typeof key[2] === "string" ? key[2] : null;
    if (projectId && !projectIds.has(projectId)) qc.setQueryData(key, []);
  }
  void qc.invalidateQueries({ queryKey: ["threadHistory"] });
}

export async function waitForCommand(commandId: string, timeoutMs = 90_000) {
  const deadline = Date.now() + timeoutMs;
  let delay = 180;
  while (Date.now() < deadline) {
    try {
      const command = await api.command(commandId);
      if (command.status === "completed") return command;
      if (TERMINAL_CMD.has(command.status)) {
        throw new Error(command.errorMessage || "操作失败，请重试");
      }
    } catch (error) {
      if (!(error instanceof ApiError && (error.retryable || error.code === "not_found"))) {
        throw error;
      }
    }
    await new Promise((resolve) => window.setTimeout(resolve, delay));
    delay = Math.min(1_000, Math.round(delay * 1.5));
  }
  throw new Error("操作超时，请检查设备连接后重试");
}

/** register a command receipt; falls back to polling if the SSE update is lost */
export function trackCommand(qc: QueryClient, commandId: string, threadId?: string, kind?: string) {
  useCommands.getState().track(commandId, threadId, kind);
  const poll = async (attempt: number) => {
    const cur = useCommands.getState().byId[commandId];
    if (!cur || TERMINAL_CMD.has(cur.status)) return;
    try {
      const view = await api.command(commandId);
      applyCommandStatus(
        qc,
        commandId,
        view.status,
        view.kind,
        view.errorCode,
        view.errorMessage,
      );
    } catch (e) {
      if (!(e instanceof ApiError && e.code === "not_found") && attempt < 4) {
        setTimeout(() => void poll(attempt + 1), 3000 * attempt);
      }
      return;
    }
    if (!TERMINAL_CMD.has(useCommands.getState().byId[commandId]?.status ?? "") && attempt < 4) {
      setTimeout(() => void poll(attempt + 1), 3000 * attempt);
    }
  };
  setTimeout(() => void poll(1), 4000);
}

function applyCommandStatus(
  qc: QueryClient,
  commandId: string,
  status: string,
  kind?: string,
  errorCode?: string | null,
  errorMessage?: string | null,
) {
  const cmd = useCommands.getState().byId[commandId];
  useCommands.getState().apply(commandId, status as never);
  liveStore.applyCommandStatus(commandId, status as never, errorCode, errorMessage);
  const resolvedKind = kind ?? cmd?.kind;
  if (TERMINAL_CMD.has(status) && resolvedKind) {
    if (resolvedKind.startsWith("thread.")) {
      void qc.invalidateQueries({ queryKey: ["projectThreads"] });
      void qc.invalidateQueries({ queryKey: ["allThreads"] });
      if (cmd?.threadId) void qc.invalidateQueries({ queryKey: ["threadHistory", cmd.threadId] });
    }
    if (resolvedKind.startsWith("project.")) {
      void qc.invalidateQueries({ queryKey: ["projects"] });
      void qc.invalidateQueries({ queryKey: ["devices"] });
      if (resolvedKind === "project.delete") {
        void qc.invalidateQueries({ queryKey: ["projectThreads"] });
        void qc.invalidateQueries({ queryKey: ["allThreads"] });
      }
    }
  }
}

export function startEvents(qc: QueryClient): () => void {
  let es: EventSource | null = null;
  let closed = false;
  let ready = false;
  let syncGeneration = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let retryDelay = 1_000;
  const dirtyThreads = new Set<string>();
  let flushTimer: ReturnType<typeof setTimeout> | null = null;

  const markThreadDirty = (threadId: string | null) => {
    if (!threadId) return;
    dirtyThreads.add(threadId);
    if (!flushTimer) {
      flushTimer = setTimeout(() => {
        flushTimer = null;
        const ids = [...dirtyThreads];
        dirtyThreads.clear();
        for (const id of ids) {
          void qc.invalidateQueries({ queryKey: ["threadHistory", id] });
        }
        void qc.invalidateQueries({ queryKey: ["projectThreads"] });
        void qc.invalidateQueries({ queryKey: ["allThreads"] });
      }, 600);
    }
  };

  const dispatch = (event: NuntiusEvent) => {
    const type = event.eventType;
    liveStore.apply(event);

    if (type === "device.renamed") {
      const displayName = (event.payload as { displayName?: unknown }).displayName;
      if (typeof displayName === "string") {
        qc.setQueryData<DeviceSummary[]>(["devices"], (old) =>
          old?.map((device) =>
            device.id === event.deviceId ? { ...device, displayName } : device,
          ),
        );
      } else {
        void qc.invalidateQueries({ queryKey: ["devices"] });
      }
      return;
    }
    if (type === "device.online" || type === "device.offline") {
      qc.setQueryData<DeviceSummary[]>(["devices"], (old) =>
        old?.map((d) =>
          d.id === event.deviceId
            ? { ...d, status: type === "device.online" ? "online" : "offline" }
            : d,
        ),
      );
      void qc.invalidateQueries({ queryKey: ["devices"] });
      return;
    }
    if (type === "project.summary") {
      const project = event.payload as ProjectSummary;
      qc.setQueryData<ProjectSummary[]>(["projects", event.deviceId], (old) => {
        if (!old) return old;
        const idx = old.findIndex((p) => p.id === project.id);
        if (idx < 0) return [project, ...old];
        const next = [...old];
        next[idx] = project;
        return next;
      });
      return;
    }
    if (type === "thread.summary") {
      markThreadDirty(event.threadId);
      void qc.invalidateQueries({ queryKey: ["devices"] });
      return;
    }
    if (type === "project.removed") {
      const projectId =
        typeof (event.payload as { projectId?: unknown }).projectId === "string"
          ? (event.payload as { projectId: string }).projectId
          : event.projectId;
      if (projectId) {
        qc.setQueryData<ProjectSummary[]>(["projects", event.deviceId], (old) =>
          old?.filter((project) => project.id !== projectId),
        );
      }
      void qc.invalidateQueries({ queryKey: ["projects", event.deviceId] });
      void qc.invalidateQueries({ queryKey: ["projectThreads"] });
      void qc.invalidateQueries({ queryKey: ["allThreads"] });
      void qc.invalidateQueries({ queryKey: ["devices"] });
      return;
    }
    if (type === "approval.requested") {
      const p = event.payload as ApprovalRequestedPayload;
      useApprovals.getState().add({
        id: p.approvalId,
        method: p.method,
        params: p.params,
        state: "pending",
        occurredAt: event.occurredAt,
        threadId: event.threadId,
        deviceId: event.deviceId,
      });
      void qc.invalidateQueries({ queryKey: ["devices"] });
      return;
    }
    if (type === "command.status_changed") {
      const p = event.payload as CommandStatusPayload;
      applyCommandStatus(
        qc,
        p.commandId,
        p.status,
        p.kind,
        p.errorCode,
        p.errorMessage,
      );
      return;
    }
    if (type === "history.sync_progress") {
      const p = event.payload as HistoryProgressPayload;
      markThreadDirty(p.threadId);
      return;
    }
    if (type === "turn.started") {
      markThreadDirty(event.threadId);
      return;
    }
    if (
      type === "agent.turn.started" ||
      type === "agent.turn.ended" ||
      type === "agent.event.session.work_changed"
    ) {
      if (type === "agent.turn.ended" && event.threadId) {
        useApprovals.getState().cancelForThread(event.threadId);
      }
      markThreadDirty(event.threadId);
      void qc.invalidateQueries({ queryKey: ["devices"] });
      return;
    }
    if (type.startsWith("app_server.")) {
      const m = type.slice("app_server.".length).toLowerCase();
      if (
        m === "turn.started" ||
        m === "turn.completed" ||
        m === "turn.failed" ||
        m === "turn.error" ||
        m === "thread.status.changed" ||
        m.startsWith("turn.interrupt")
      ) {
        if (
          event.threadId &&
          (m === "turn.completed" ||
            m === "turn.failed" ||
            m === "turn.error" ||
            m.startsWith("turn.interrupt"))
        ) {
          useApprovals.getState().cancelForThread(event.threadId);
        }
        markThreadDirty(event.threadId);
        void qc.invalidateQueries({ queryKey: ["devices"] });
      }
    }
  };

  const connect = (after: number) => {
    if (closed) return;
    es?.close();
    es = new EventSource(`/api/v1/events?after=${encodeURIComponent(after)}`);
    es.onopen = () => {
      ready = true;
      retryDelay = 1_000;
      useSse.getState().set("live");
      everLive = true;
      // The server starts a fresh browser at the event-log head. Refreshing
      // after the subscription opens makes the REST snapshot and subsequent
      // event stream a gap-free pair.
      resync();
    };
    es.addEventListener("nuntius", (e) => {
      try {
        dispatch(JSON.parse((e as MessageEvent).data) as NuntiusEvent);
      } catch {
        /* malformed event */
      }
    });
    es.addEventListener("resync_required", () => void resync());
    es.onerror = () => {
      if (useSse.getState().status !== "syncing") useSse.getState().set("reconnecting");
    };
  };

  const resync = async () => {
    const generation = ++syncGeneration;
    es?.close();
    es = null;
    useSse.getState().set("syncing");
    try {
      const snapshot = await api.sync();
      if (closed || generation !== syncGeneration) return;
      applySnapshot(qc, snapshot);
      connect(snapshot.cursor);
    } catch {
      if (closed || generation !== syncGeneration) return;
      useSse.getState().set("reconnecting");
      if (retryTimer) clearTimeout(retryTimer);
      retryTimer = setTimeout(() => void resync(), retryDelay);
      retryDelay = Math.min(30_000, retryDelay * 2);
    }
  };

  const onVisible = () => {
    if (document.visibilityState === "visible" && ready) void resync();
  };
  document.addEventListener("visibilitychange", onVisible);

  void resync();
  return () => {
    closed = true;
    syncGeneration += 1;
    es?.close();
    document.removeEventListener("visibilitychange", onVisible);
    if (flushTimer) clearTimeout(flushTimer);
    if (retryTimer) clearTimeout(retryTimer);
  };
}
