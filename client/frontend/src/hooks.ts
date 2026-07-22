/* misc hooks */
import { useCallback, useSyncExternalStore } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useToast, type ThreadSummary } from "@nuntius/shared";
import { api } from "./api";
import {
  queueArchive,
  submitArchive,
  usePendingArchiveIds,
} from "./archiveOutbox";
import { useRoute, type Route } from "./stores";

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

function replaceThreadInCaches(qc: QueryClient, thread: ThreadSummary) {
  const replace = (items: ThreadSummary[] | undefined) =>
    items?.map((item) => (item.id === thread.id ? thread : item));
  qc.setQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] }, replace);
  qc.setQueryData<ThreadSummary[]>(["threads"], replace);
  qc.setQueryData<ThreadSummary>(["threadSnapshot", thread.id], thread);
}

export function useRenameThreadAction() {
  const qc = useQueryClient();
  const toast = useToast();

  return useCallback(
    async (thread: ThreadSummary, title: string | null) => {
      const optimistic: ThreadSummary = {
        ...thread,
        title: title ?? thread.title,
        displayTitleOverride: title,
        titleRevision: thread.titleRevision + 1,
      };
      replaceThreadInCaches(qc, optimistic);
      try {
        const result = await api.renameThread(thread.id, title);
        replaceThreadInCaches(qc, result.thread);
        toast(title === null ? "已恢复自动标题" : "会话名称已更新");
      } catch (error) {
        replaceThreadInCaches(qc, thread);
        void qc.invalidateQueries({ queryKey: ["projectThreads"] });
        void qc.invalidateQueries({ queryKey: ["threads"] });
        toast(error instanceof Error ? error.message : "重命名失败，请重试", { error: true });
        throw error;
      }
    },
    [qc, toast],
  );
}

export function useArchiveThreadAction() {
  const qc = useQueryClient();
  const toast = useToast();
  const busyIds = usePendingArchiveIds();

  const archive = useCallback(
    (threadId: string) => {
      if (busyIds.has(threadId)) return false;
      queueArchive(threadId);
      const cachedThread = [
        ...qc
          .getQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] })
          .flatMap(([, threads]) => threads ?? []),
        ...(qc.getQueryData<ThreadSummary[]>(["threads"]) ?? []),
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
      qc.setQueryData<ThreadSummary[]>(["threads"], removeArchived);
      void submitArchive(threadId, qc, toast);
      return true;
    },
    [busyIds, qc, toast],
  );

  return { archive, busyIds };
}
