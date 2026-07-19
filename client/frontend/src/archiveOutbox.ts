import { useEffect, useSyncExternalStore } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useToast } from "@nuntius/shared";
import { api, ApiError } from "./api";

interface ArchiveIntent {
  threadId: string;
  createdAt: number;
  attempts: number;
  nextAttemptAt: number;
}

type Toast = (text: string, opts?: { error?: boolean }) => void;

const STORAGE_KEY = "nuntius:local-archive-outbox:v1";
const listeners = new Set<() => void>();
const inFlight = new Set<string>();
let version = 0;

function load(): Record<string, ArchiveIntent> {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Record<
      string,
      ArchiveIntent
    >;
    const cutoff = Date.now() - 7 * 24 * 3600_000;
    return Object.fromEntries(
      Object.entries(parsed).filter(
        ([, intent]) =>
          intent && typeof intent.threadId === "string" && intent.createdAt >= cutoff,
      ),
    );
  } catch {
    return {};
  }
}

let intents = load();

function publish(next: Record<string, ArchiveIntent>) {
  intents = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(intents));
  } catch {
    // Keep the current-page queue alive even if browser storage is unavailable.
  }
  version += 1;
  listeners.forEach((listener) => listener());
}

function remove(threadId: string) {
  if (!intents[threadId]) return;
  const next = { ...intents };
  delete next[threadId];
  publish(next);
}

function retryLater(intent: ArchiveIntent) {
  const attempts = intent.attempts + 1;
  const delay = Math.min(30_000, 1_000 * 2 ** Math.min(attempts - 1, 5));
  publish({
    ...intents,
    [intent.threadId]: {
      ...intent,
      attempts,
      nextAttemptAt: Date.now() + delay,
    },
  });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getVersion() {
  return version;
}

export function usePendingArchiveIds() {
  useSyncExternalStore(subscribe, getVersion);
  return new Set(Object.keys(intents));
}

export function queueArchive(threadId: string) {
  if (intents[threadId]) return;
  publish({
    ...intents,
    [threadId]: {
      threadId,
      createdAt: Date.now(),
      attempts: 0,
      nextAttemptAt: 0,
    },
  });
}

async function invalidateThreads(qc: QueryClient) {
  await Promise.all([
    qc.invalidateQueries({ queryKey: ["projectThreads"] }),
    qc.invalidateQueries({ queryKey: ["threads"] }),
  ]);
}

export async function submitArchive(
  threadId: string,
  qc: QueryClient,
  toast: Toast,
) {
  const intent = intents[threadId];
  if (!intent || inFlight.has(threadId) || intent.nextAttemptAt > Date.now()) return;
  inFlight.add(threadId);
  try {
    await api.archiveThread(threadId, true);
    remove(threadId);
    await invalidateThreads(qc);
    toast("会话已自动归档");
  } catch (error) {
    if (error instanceof ApiError && error.code === "not_found") {
      remove(threadId);
      await invalidateThreads(qc);
      return;
    }
    if (error instanceof ApiError && (error.status === 0 || error.status >= 500)) {
      retryLater(intent);
      return;
    }
    remove(threadId);
    await invalidateThreads(qc);
    toast(error instanceof Error ? error.message : "归档失败，请重试", { error: true });
  } finally {
    inFlight.delete(threadId);
  }
}

async function flush(qc: QueryClient, toast: Toast) {
  await Promise.all(Object.keys(intents).map((threadId) => submitArchive(threadId, qc, toast)));
}

export function useArchiveOutboxRunner() {
  const qc = useQueryClient();
  const toast = useToast();
  useEffect(() => {
    const run = () => void flush(qc, toast);
    const visible = () => {
      if (document.visibilityState === "visible") run();
    };
    run();
    const timer = window.setInterval(run, 2_000);
    window.addEventListener("online", run);
    document.addEventListener("visibilitychange", visible);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("online", run);
      document.removeEventListener("visibilitychange", visible);
    };
  }, [qc, toast]);
}
