/* misc hooks */
import { useCallback, useSyncExternalStore } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import { useToast, type ThreadSummary } from "@nuntius/shared";
import { api } from "./api";
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

export function useProjectNameMap(deviceIds: string[]): Map<string, string> {
  const uniqueDeviceIds = [...new Set(deviceIds)];
  const projectQueries = useQueries({
    queries: uniqueDeviceIds.map((deviceId) => ({
      queryKey: ["projects", deviceId],
      queryFn: () => api.projects(deviceId),
    })),
  });
  const names = new Map<string, string>();
  projectQueries.forEach((query, index) => {
    for (const project of query.data ?? []) {
      names.set(`${uniqueDeviceIds[index]}:${project.id}`, project.displayName);
    }
  });
  return names;
}

export function projectNameFrom(
  names: Map<string, string>,
  deviceId: string,
  projectId: string,
): string {
  return names.get(`${deviceId}:${projectId}`) ?? "项目";
}

export function useArchiveThreadAction() {
  const qc = useQueryClient();
  const toast = useToast();
  const userId = useSession((state) => state.session?.userId);
  const busyIds = usePendingArchiveIds(userId);

  const archive = useCallback(
    (threadId: string) => {
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
      void submitArchive(userId, threadId, qc, toast);
      return true;
    },
    [busyIds, qc, toast, userId],
  );

  return { archive, busyIds };
}
