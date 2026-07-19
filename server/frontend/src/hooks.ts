/* misc hooks */
import { useCallback, useSyncExternalStore } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useToast, type ThreadSummary } from "@nuntius/shared";
import {
  queueArchive,
  submitArchive,
  usePendingArchiveIds,
} from "./archiveOutbox";
import { useRoute, useSession, type Route } from "./stores";

export function useNavigate() {
  const navigate = useRoute((s) => s.navigate);
  return useCallback(
    (r: Route, opts?: { replace?: boolean }) => navigate(r, opts),
    [navigate],
  );
}

export function useMedia(query: string): boolean {
  const subscribe = useCallback(
    (fn: () => void) => {
      const mql = window.matchMedia(query);
      mql.addEventListener("change", fn);
      return () => mql.removeEventListener("change", fn);
    },
    [query],
  );
  return useSyncExternalStore(subscribe, () => window.matchMedia(query).matches);
}

export function useArchiveThreadAction() {
  const qc = useQueryClient();
  const toast = useToast();
  const userId = useSession((state) => state.session?.userId);
  const busyIds = usePendingArchiveIds(userId);

  const archive = useCallback(
    async (threadId: string) => {
      if (!userId || busyIds.has(threadId)) return false;
      queueArchive(userId, threadId);
      const cachedThread = [
        ...qc
          .getQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] })
          .flatMap(([, threads]) => threads ?? []),
        ...(qc.getQueryData<ThreadSummary[]>(["allThreads"]) ?? []),
      ].find((thread) => thread.id === threadId);
      if (cachedThread) {
        qc.setQueryData<ThreadSummary>(["threadSnapshot", threadId], {
          ...cachedThread,
          archived: true,
        });
      }
      const removeArchived = (old: ThreadSummary[] | undefined) =>
        old?.filter((thread) => thread.id !== threadId);
      qc.setQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] }, removeArchived);
      qc.setQueryData<ThreadSummary[]>(["allThreads"], removeArchived);
      return (await submitArchive(userId, threadId, qc, toast, true)) !== "failed";
    },
    [busyIds, qc, toast, userId],
  );

  return { archive, busyIds };
}
