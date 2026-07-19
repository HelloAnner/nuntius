/* Local SSE manager: streams CLI events into caches and the live store. */
import { create } from "zustand";
import type { QueryClient } from "@tanstack/react-query";
import type { ApprovalRequestedPayload, NuntiusEvent, SyncSnapshot } from "@nuntius/shared";
import { api } from "./api";
import { liveStore, useApprovals } from "./stores";

export type SseStatus = "connecting" | "live" | "reconnecting" | "syncing";
interface SseState {
  status: SseStatus;
  set: (status: SseStatus) => void;
}
export const useSse = create<SseState>((set) => ({
  status: "connecting",
  set: (status) => set({ status }),
}));

function applySnapshot(qc: QueryClient, snapshot: SyncSnapshot) {
  liveStore.reset();
  useApprovals.getState().replaceFromSnapshot(snapshot.approvals);
  qc.setQueryData(["projects"], snapshot.projects);
  qc.setQueryData(["threads"], snapshot.threads);
  const projectIds = new Set(snapshot.projects.map((project) => project.id));
  for (const project of snapshot.projects) {
    qc.setQueryData(
      ["projectThreads", project.id],
      snapshot.threads.filter((thread) => thread.projectId === project.id),
    );
  }
  for (const [key] of qc.getQueriesData({ queryKey: ["projectThreads"] })) {
    const projectId = typeof key[1] === "string" ? key[1] : null;
    if (projectId && !projectIds.has(projectId)) qc.setQueryData(key, []);
  }
  void qc.invalidateQueries({ queryKey: ["history"] });
  void qc.invalidateQueries({ queryKey: ["info"] });
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
          void qc.invalidateQueries({ queryKey: ["history", id] });
        }
        void qc.invalidateQueries({ queryKey: ["projectThreads"] });
        void qc.invalidateQueries({ queryKey: ["threads"] });
      }, 500);
    }
  };

  const dispatch = (event: NuntiusEvent) => {
    const type = event.eventType;
    liveStore.apply(event);
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
      return;
    }
    if (type === "project.summary") {
      void qc.invalidateQueries({ queryKey: ["projects"] });
      return;
    }
    if (type === "thread.summary") {
      markThreadDirty(event.threadId);
      return;
    }
    if (type === "project.removed") {
      void qc.invalidateQueries({ queryKey: ["projects"] });
      void qc.invalidateQueries({ queryKey: ["projectThreads"] });
      void qc.invalidateQueries({ queryKey: ["threads"] });
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
      // Do not resync here: resync() creates this stream. The snapshot cursor
      // was captured before its reads, so replaying from it already closes the
      // snapshot/subscription race without starting a connect/resync loop.
    };
    es.addEventListener("nuntius", (e) => {
      try {
        dispatch(JSON.parse((e as MessageEvent).data) as NuntiusEvent);
      } catch {
        /* malformed */
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
