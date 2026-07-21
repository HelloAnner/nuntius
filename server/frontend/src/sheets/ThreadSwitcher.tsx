/* Global thread switcher matching the mobile conversation sheet. */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { IconPlus, Sheet, compareThreadCreation } from "@nuntius/shared";
import { api } from "../api";
import { projectNameFrom, useNavigate, useProjectNameMap } from "../hooks";
import { ThreadListItem } from "../components";
import { useApprovals } from "../stores";

export function ThreadSwitcher({
  open,
  onClose,
  onNewThread,
  currentThreadId,
  navigationContext = "project",
}: {
  open: boolean;
  onClose: () => void;
  onNewThread?: () => void;
  currentThreadId?: string;
  navigationContext?: "project" | "recents";
}) {
  const navigate = useNavigate();
  const approvals = useApprovals((state) => state.items);
  const fromRecents = navigationContext === "recents";
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projectNames = useProjectNameMap(
    (threads.data ?? []).map((thread) => thread.deviceId),
  );
  const pendingIds = useMemo(
    () => new Set(Object.values(approvals).flatMap((approval) => approval.state === "pending" && approval.threadId ? [approval.threadId] : [])),
    [approvals],
  );
  const scoped = useMemo(
    () => [...(threads.data ?? [])]
      .sort(compareThreadCreation),
    [threads.data],
  );
  const contextFor = (thread: (typeof scoped)[number]) => ({
    device: devices.data?.find((item) => item.id === thread.deviceId)?.displayName ?? "设备",
    project: projectNameFrom(projectNames, thread.deviceId, thread.projectId),
  });
  const active = scoped.filter((thread) => thread.status === "active" || pendingIds.has(thread.id));
  const recent = scoped.filter((thread) => thread.status !== "active" && !pendingIds.has(thread.id));

  const select = (thread: (typeof scoped)[number]) => {
    navigate(
      navigationContext === "recents"
        ? { name: "recentThread", threadId: thread.id }
        : { name: "thread", deviceId: thread.deviceId, projectId: thread.projectId, threadId: thread.id },
    );
    onClose();
  };

  return (
    <Sheet open={open} onClose={onClose} className="thread-switcher-sheet">
      <div className="thread-switcher-content">
        <header className="thread-switcher-head">
          <strong>切换会话</strong>
          <span>全部设备 · 全部项目</span>
        </header>
        {onNewThread ? (
          <button
            className="quick-new-thread"
            onClick={() => {
              onClose();
              onNewThread();
            }}
          >
            <IconPlus size={15} />
            <strong>新建会话</strong>
            <small>默认当前项目</small>
          </button>
        ) : null}
        {active.length ? <ThreadSwitcherGroup label="进行中" threads={active} currentThreadId={currentThreadId} pendingIds={pendingIds} contextFor={contextFor} onSelect={select} /> : null}
        {recent.length ? <ThreadSwitcherGroup label="最近" threads={recent} currentThreadId={currentThreadId} pendingIds={pendingIds} contextFor={contextFor} onSelect={select} /> : null}
        {!active.length && !recent.length ? <div className="switcher-empty">还没有会话</div> : null}
      </div>
    </Sheet>
  );
}

function ThreadSwitcherGroup({
  label,
  threads,
  currentThreadId,
  pendingIds,
  contextFor,
  onSelect,
}: {
  label: string;
  threads: Awaited<ReturnType<typeof api.allThreads>>;
  currentThreadId?: string;
  pendingIds: Set<string>;
  contextFor?: (thread: Awaited<ReturnType<typeof api.allThreads>>[number]) => { device: string; project: string };
  onSelect: (thread: Awaited<ReturnType<typeof api.allThreads>>[number]) => void;
}) {
  return (
    <section className="thread-switcher-group">
      <div className="thread-switcher-label">{label}</div>
      {threads.map((thread) => {
        const context = contextFor?.(thread);
        return (
          <ThreadListItem
            key={thread.id}
            thread={thread}
            pendingApproval={pendingIds.has(thread.id)}
            selected={thread.id === currentThreadId}
            contextDevice={context?.device}
            contextProject={context?.project}
            onClick={() => onSelect(thread)}
          />
        );
      })}
    </section>
  );
}
