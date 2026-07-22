import { compareThreadStatusCreation, type ThreadSummary } from "@nuntius/shared";

type PreferenceStorage = Pick<Storage, "getItem" | "setItem">;
type RecentThreadCandidate = Pick<ThreadSummary, "id" | "status" | "createdAt" | "archived">;

const STORAGE_KEY = "nuntius:last-recent-thread:v1";
const MAX_THREAD_ID_LENGTH = 512;

export function loadLastRecentThreadId(
  storage: PreferenceStorage = localStorage,
): string | null {
  try {
    const threadId = storage.getItem(STORAGE_KEY);
    return threadId && threadId.length <= MAX_THREAD_ID_LENGTH ? threadId : null;
  } catch {
    return null;
  }
}

export function saveLastRecentThreadId(
  threadId: string,
  storage: PreferenceStorage = localStorage,
) {
  if (!threadId || threadId.length > MAX_THREAD_ID_LENGTH) return;
  try {
    storage.setItem(STORAGE_KEY, threadId);
  } catch {
    // The workspace still falls back to the first sorted thread when storage is unavailable.
  }
}

export function selectRecentWorkspaceThread<T extends RecentThreadCandidate>(
  threads: readonly T[],
  lastThreadId: string | null,
  excludedIds: ReadonlySet<string> = new Set(),
): T | null {
  const available = threads.filter(
    (thread) => !thread.archived && !excludedIds.has(thread.id),
  );
  const remembered = lastThreadId
    ? available.find((thread) => thread.id === lastThreadId)
    : undefined;
  return remembered ?? [...available].sort(compareThreadStatusCreation)[0] ?? null;
}
