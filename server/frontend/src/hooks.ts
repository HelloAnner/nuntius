/* misc hooks */
import { useCallback, useState, useSyncExternalStore } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useToast, type ThreadSummary } from "@nuntius/shared";
import { api } from "./api";
import { trackCommand, waitForCommand } from "./events";
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
    async (threadId: string, archived = true) => {
      if (busyIds.has(threadId)) return false;
      setBusyIds((old) => new Set(old).add(threadId));
      try {
        const receipt = await api.archiveThread(threadId, archived);
        const update = (old: ThreadSummary[] | undefined) =>
          old?.map((thread) => (thread.id === threadId ? { ...thread, archived } : thread));
        qc.setQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] }, update);
        qc.setQueryData<ThreadSummary[]>(["allThreads"], update);
        trackCommand(qc, receipt.commandId, threadId, archived ? "thread.archive" : "thread.unarchive");
        await waitForCommand(receipt.commandId);
        await Promise.all([
          qc.invalidateQueries({ queryKey: ["projectThreads"] }),
          qc.invalidateQueries({ queryKey: ["allThreads"] }),
          qc.invalidateQueries({ queryKey: ["devices"] }),
        ]);
        toast(archived ? "会话已归档" : "会话已恢复");
        return true;
      } catch (error) {
        await Promise.all([
          qc.invalidateQueries({ queryKey: ["projectThreads"] }),
          qc.invalidateQueries({ queryKey: ["allThreads"] }),
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
