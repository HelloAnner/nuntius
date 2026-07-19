/* misc hooks */
import { useCallback, useState, useSyncExternalStore } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useToast, type ThreadSummary } from "@nuntius/shared";
import { api } from "./api";
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

export function useArchiveThreadAction() {
  const qc = useQueryClient();
  const toast = useToast();
  const [busyIds, setBusyIds] = useState<Set<string>>(() => new Set());

  const archive = useCallback(
    async (threadId: string) => {
      if (busyIds.has(threadId)) return false;
      setBusyIds((old) => new Set(old).add(threadId));
      const cachedThread = [
        ...qc
          .getQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] })
          .flatMap(([, threads]) => threads ?? []),
        ...(qc.getQueryData<ThreadSummary[]>(["threads"]) ?? []),
      ].find((thread) => thread.id === threadId);
      try {
        await api.archiveThread(threadId, true);
        if (cachedThread) {
          qc.setQueryData<ThreadSummary>(["threadSnapshot", threadId], {
            ...cachedThread,
            archived: true,
          });
        }
        const update = (old: ThreadSummary[] | undefined) =>
          old?.filter((thread) => thread.id !== threadId);
        qc.setQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] }, update);
        qc.setQueryData<ThreadSummary[]>(["threads"], update);
        await Promise.all([
          qc.invalidateQueries({ queryKey: ["projectThreads"] }),
          qc.invalidateQueries({ queryKey: ["threads"] }),
          qc.invalidateQueries({ queryKey: ["history", threadId] }),
        ]);
        toast("会话已归档");
        return true;
      } catch (error) {
        if (cachedThread) {
          qc.setQueryData<ThreadSummary>(["threadSnapshot", threadId], cachedThread);
        }
        await Promise.all([
          qc.invalidateQueries({ queryKey: ["projectThreads"] }),
          qc.invalidateQueries({ queryKey: ["threads"] }),
        ]);
        toast(error instanceof Error ? error.message : "归档失败，请重试", { error: true });
        return false;
      } finally {
        setBusyIds((old) => {
          const next = new Set(old);
          next.delete(threadId);
          return next;
        });
      }
    },
    [busyIds, qc, toast],
  );

  return { archive, busyIds };
}
