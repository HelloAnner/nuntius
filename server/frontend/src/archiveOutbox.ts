import { useEffect, useSyncExternalStore } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { newIdemKey, useToast, type ThreadSummary } from "@nuntius/shared";
import { api, ApiError } from "./api";
import { trackCommand } from "./events";
import { useSession } from "./stores";

interface ArchiveIntent {
  userId: string;
  threadId: string;
  idempotencyKey: string;
  commandId?: string;
  createdAt: number;
  attempts: number;
  nextAttemptAt: number;
}

type Toast = (text: string, opts?: { error?: boolean }) => void;
type IntentResult = "pending" | "completed" | "failed";

const STORAGE_KEY = "nuntius:archive-outbox:v1";
const TERMINAL = new Set(["completed", "failed", "rejected", "unknown", "expired"]);
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
          intent &&
          typeof intent.threadId === "string" &&
          typeof intent.userId === "string" &&
          intent.createdAt >= cutoff,
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
    // The in-memory queue still covers the current page when storage is unavailable.
  }
  version += 1;
  listeners.forEach((listener) => listener());
}

function intentKey(userId: string, threadId: string) {
  return `${userId}:${threadId}`;
}

function updateIntent(key: string, update: Partial<ArchiveIntent>) {
  const current = intents[key];
  if (!current) return;
  publish({ ...intents, [key]: { ...current, ...update } });
}

function removeIntent(key: string) {
  if (!intents[key]) return;
  const next = { ...intents };
  delete next[key];
  publish(next);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getVersion() {
  return version;
}

export function usePendingArchiveIds(userId: string | undefined) {
  useSyncExternalStore(subscribe, getVersion);
  return new Set(
    Object.values(intents)
      .filter((intent) => intent.userId === userId)
      .map((intent) => intent.threadId),
  );
}

export function queueArchive(userId: string, threadId: string) {
  const key = intentKey(userId, threadId);
  if (intents[key]) return;
  publish({
    ...intents,
    [key]: {
      userId,
      threadId,
      idempotencyKey: newIdemKey(),
      createdAt: Date.now(),
      attempts: 0,
      nextAttemptAt: 0,
    },
  });
}

async function invalidateThreads(qc: QueryClient) {
  await Promise.all([
    qc.invalidateQueries({ queryKey: ["projectThreads"] }),
    qc.invalidateQueries({ queryKey: ["allThreads"] }),
    qc.invalidateQueries({ queryKey: ["devices"] }),
  ]);
}

function restoreOptimisticArchive(qc: QueryClient, threadId: string) {
  qc.setQueryData<ThreadSummary>(
    ["threadSnapshot", threadId],
    (thread) => thread ? { ...thread, archived: false } : thread,
  );
}

function archiveFailureMessage(error: unknown) {
  const detail = typeof error === "string"
    ? error
    : error instanceof Error
      ? error.message
      : "请重试";
  return `归档失败：${detail}`;
}

function retryLater(key: string, intent: ArchiveIntent) {
  const attempts = intent.attempts + 1;
  const delay = Math.min(30_000, 1_000 * 2 ** Math.min(attempts - 1, 5));
  updateIntent(key, { attempts, nextAttemptAt: Date.now() + delay });
}

function isRecoverable(error: unknown) {
  return (
    error instanceof ApiError &&
    (error.retryable ||
      error.status === 0 ||
      error.status === 401 ||
      error.code === "network" ||
      error.code === "device_offline" ||
      error.code === "unavailable")
  );
}

export async function submitArchive(
  userId: string,
  threadId: string,
  qc: QueryClient,
  toast: Toast,
): Promise<IntentResult> {
  const key = intentKey(userId, threadId);
  const intent = intents[key];
  if (!intent) return "completed";
  if (inFlight.has(key)) return "pending";
  if (intent.nextAttemptAt > Date.now()) return "pending";

  inFlight.add(key);
  try {
    if (intent.commandId) {
      const command = await api.command(intent.commandId);
      if (command.status === "completed") {
        removeIntent(key);
        await invalidateThreads(qc);
        return "completed";
      }
      if (TERMINAL.has(command.status)) {
        removeIntent(key);
        restoreOptimisticArchive(qc, threadId);
        await invalidateThreads(qc);
        toast(archiveFailureMessage(command.errorMessage), {
          error: true,
        });
        return "failed";
      }
      updateIntent(key, { attempts: 0, nextAttemptAt: Date.now() + 2_000 });
      return "pending";
    }

    const receipt = await api.archiveThread(
      intent.threadId,
      true,
      intent.idempotencyKey,
    );
    updateIntent(key, {
      commandId: receipt.commandId,
      attempts: 0,
      nextAttemptAt: Date.now() + 1_000,
    });
    trackCommand(qc, receipt.commandId, intent.threadId, "thread.archive");
    return "pending";
  } catch (error) {
    if (error instanceof ApiError && error.code === "not_found" && !intent.commandId) {
      removeIntent(key);
      await invalidateThreads(qc);
      return "completed";
    }
    if (isRecoverable(error)) {
      retryLater(key, intent);
      return "pending";
    }
    removeIntent(key);
    restoreOptimisticArchive(qc, threadId);
    await invalidateThreads(qc);
    toast(archiveFailureMessage(error), { error: true });
    return "failed";
  } finally {
    inFlight.delete(key);
  }
}

async function flush(userId: string, qc: QueryClient, toast: Toast) {
  const pending = Object.values(intents).filter((intent) => intent.userId === userId);
  await Promise.all(
    pending.map((intent) => submitArchive(userId, intent.threadId, qc, toast)),
  );
}

export function useArchiveOutboxRunner(enabled: boolean) {
  const qc = useQueryClient();
  const toast = useToast();
  const userId = useSession((state) => state.session?.userId);

  useEffect(() => {
    if (!enabled || !userId) return;
    const run = () => void flush(userId, qc, toast);
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
  }, [enabled, qc, toast, userId]);
}
