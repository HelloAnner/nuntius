/* Local SSE manager: streams CLI events into caches and the live store. */
import { create } from "zustand";
import type { QueryClient } from "@tanstack/react-query";
import type { ApprovalRequestedPayload, NuntiusEvent } from "@nuntius/shared";
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

export function startEvents(qc: QueryClient): () => void {
  let es: EventSource | null = null;
  let closed = false;
  let everLive = false;
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

  const resync = () => {
    useSse.getState().set("syncing");
    void qc.invalidateQueries().then(() => {
      if (useSse.getState().status === "syncing") useSse.getState().set("live");
    });
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
    if (type === "turn.started") {
      markThreadDirty(event.threadId);
      return;
    }
    if (type.startsWith("app_server.")) {
      const m = type.slice("app_server.".length).toLowerCase();
      if (m === "turn.completed" || m === "turn.failed" || m.startsWith("turn.interrupt")) {
        if (event.threadId) useApprovals.getState().cancelForThread(event.threadId);
        markThreadDirty(event.threadId);
      }
    }
  };

  const connect = () => {
    if (closed) return;
    es = new EventSource("/api/v1/events");
    es.onopen = () => {
      useSse.getState().set("live");
      if (everLive) resync();
      everLive = true;
    };
    es.addEventListener("nuntius", (e) => {
      try {
        dispatch(JSON.parse((e as MessageEvent).data) as NuntiusEvent);
      } catch {
        /* malformed */
      }
    });
    es.addEventListener("resync_required", () => resync());
    es.onerror = () => {
      if (useSse.getState().status !== "syncing") useSse.getState().set("reconnecting");
    };
  };

  const onVisible = () => {
    if (document.visibilityState === "visible" && everLive) resync();
  };
  document.addEventListener("visibilitychange", onVisible);

  connect();
  return () => {
    closed = true;
    es?.close();
    document.removeEventListener("visibilitychange", onVisible);
    if (flushTimer) clearTimeout(flushTimer);
  };
}
