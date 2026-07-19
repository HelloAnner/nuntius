/* Thread page: the focused conversation surface. */
import { useEffect, useMemo, useState } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IconArchive,
  IconRefresh,
  Spinner,
  SwipeActionRow,
  ThreadView,
  newIdemKey,
  statusLabel,
  turnOptionsForAccess,
  useConfirmAction,
  useToast,
  type ApprovalView,
  type HistoryGroup,
  type HistoryItemView,
} from "@nuntius/shared";
import { api, ApiError } from "../api";
import { trackCommand } from "../events";
import { useArchiveThreadAction, useMedia, useNavigate } from "../hooks";
import { liveStore, useAccessMode, useApprovals, useRoute, useThreadLive } from "../stores";
import { ConnIndicator, ThreadRow, TopBar } from "../components";
import { ThreadSwitcher } from "../sheets/ThreadSwitcher";

function mapHistoryItem(item: HistoryItemView) {
  return {
    id: item.id,
    kind: item.kind,
    text:
      item.contentText ??
      (item.structuredDetail ? JSON.stringify(item.structuredDetail, null, 2) : ""),
    status: item.status,
    truncated: item.isTruncated,
  };
}

export function ThreadPage({
  deviceId,
  projectId,
  threadId,
}: {
  deviceId: string;
  projectId: string;
  threadId: string;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const accessMode = useAccessMode((state) => state.mode);
  const navigate = useNavigate();
  const { archive: archiveThread, busyIds } = useArchiveThreadAction();
  const back = useRoute((s) => s.back);
  const wide = useMedia("(min-width: 768px)");
  const [switcherOpen, setSwitcherOpen] = useState(false);
  const [turnCount, setTurnCount] = useState(12);
  const [routeGraceElapsed, setRouteGraceElapsed] = useState(false);
  const { confirm, node: confirmNode } = useConfirmAction();

  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projects = useQuery({
    queryKey: ["projects", deviceId],
    queryFn: () => api.projects(deviceId),
  });
  const projectThreads = useQuery({
    queryKey: ["projectThreads", deviceId, projectId],
    queryFn: () => api.projectThreads(deviceId, projectId),
    refetchInterval: (query) =>
      (query.state.data as { id: string }[] | undefined)?.some((item) => item.id === threadId)
        ? false
        : 700,
  });
  const allThreads = useQuery({
    queryKey: ["allThreads"],
    queryFn: () => api.allThreads(),
    refetchInterval: (query) =>
      (query.state.data as { id: string }[] | undefined)?.some((item) => item.id === threadId)
        ? false
        : 700,
  });

  const history = useQuery({
    queryKey: ["threadHistory", threadId, turnCount],
    queryFn: async (): Promise<{ groups: HistoryGroup[]; hasMore: boolean }> => {
      const turns = await api.historyTurns(threadId, 1000);
      const shown = turns.slice(-turnCount);
      const groups = await Promise.all(
        shown.map(async (turn) => ({
          turn: {
            id: turn.id,
            ordinal: turn.ordinal,
            status: turn.status,
            startedAt: turn.startedAt,
            completedAt: turn.completedAt,
          },
          items: (await api.historyItems(turn.id)).map(mapHistoryItem),
        })),
      );
      return { groups, hasMore: turns.length > shown.length };
    },
    placeholderData: keepPreviousData,
  });

  const device = devices.data?.find((d) => d.id === deviceId);
  const project = projects.data?.find((p) => p.id === projectId);
  const thread =
    projectThreads.data?.find((t) => t.id === threadId) ??
    allThreads.data?.find((t) => t.id === threadId);

  useEffect(() => {
    setRouteGraceElapsed(false);
    const timer = window.setTimeout(() => setRouteGraceElapsed(true), 10_000);
    return () => window.clearTimeout(timer);
  }, [threadId]);

  useEffect(() => {
    const missingDevice = devices.isSuccess && !device;
    const missingProject = projects.isSuccess && !project;
    const missingThread =
      routeGraceElapsed &&
      projectThreads.isSuccess &&
      allThreads.isSuccess &&
      !projectThreads.isFetching &&
      !allThreads.isFetching &&
      !thread;
    if (devices.isError || projects.isError || missingDevice || missingProject || missingThread) {
      navigate({ name: "devices" }, { replace: true });
    }
  }, [
    allThreads.isFetching,
    allThreads.isSuccess,
    device,
    devices.isError,
    devices.isSuccess,
    navigate,
    project,
    projectThreads.isFetching,
    projectThreads.isSuccess,
    projects.isError,
    projects.isSuccess,
    routeGraceElapsed,
    thread,
  ]);

  const live = useThreadLive(threadId);
  const approvals = useApprovals((s) => s.items);
  const threadApprovals: ApprovalView[] = useMemo(
    () =>
      Object.values(approvals)
        .filter((a) => a.threadId === threadId)
        .map((a) => ({
          ...a,
          deviceName: device?.displayName,
          projectName: project?.displayName,
          threadTitle: thread?.title,
        }))
        .sort((a, b) => Date.parse(a.occurredAt) - Date.parse(b.occurredAt)),
    [approvals, threadId, device, project, thread],
  );

  const online = device?.status === "online";
  const unassigned = project?.kind === "system_unassigned";
  const archived = thread?.archived ?? false;
  // The server SQLite projection is authoritative. Browser memory is only a
  // transient rendering layer for streamed output.
  const running = thread?.status === "active";

  const canSend = Boolean(online && !unassigned && !archived && thread);
  const lockedReason = !thread
    ? "会话加载中…"
    : archived
      ? "已归档"
      : unassigned
        ? "未归类，只读"
        : !online
          ? `设备${statusLabel(device?.status ?? "offline")}`
          : null;

  const send = async (text: string) => {
    const idemKey = newIdemKey();
    const provisionalId = `pending:${idemKey}`;
    liveStore.addOptimistic(threadId, provisionalId, text);
    try {
      const receipt = await api.startTurn(
        threadId,
        text,
        turnOptionsForAccess(accessMode),
        idemKey,
      );
      liveStore.bindCommand(provisionalId, receipt.commandId);
      liveStore.applyCommandStatus(receipt.commandId, receipt.status);
      trackCommand(qc, receipt.commandId, threadId, "thread.input");
    } catch (e) {
      const message =
        e instanceof ApiError && e.code === "device_offline"
          ? "设备离线，消息未发送"
          : e instanceof Error
            ? e.message
            : "发送失败";
      liveStore.applyCommandStatus(
        provisionalId,
        "failed",
        e instanceof ApiError ? e.code : "request_failed",
        message,
      );
    }
  };

  const retry = (turnId: string, text: string) => {
    liveStore.removeOptimistic(threadId, turnId);
    void send(text);
  };

  const interrupt = async () => {
    try {
      const receipt = await api.interruptTurn(threadId);
      trackCommand(qc, receipt.commandId, threadId, "turn.interrupt");
      toast("中断请求已发送");
    } catch {
      toast("中断失败，设备可能已离线", { error: true });
    }
  };

  const decide = async (approvalId: string, decision: string) => {
    const approvalsApi = useApprovals.getState();
    approvalsApi.setState(approvalId, "responding");
    const idemKey = newIdemKey();
    try {
      const receipt = await api.decideApproval(deviceId, approvalId, decision, idemKey);
      trackCommand(qc, receipt.commandId, threadId, "approval.decide");
      approvalsApi.setState(
        approvalId,
        decision === "decline" || decision === "cancel" ? "denied" : "approved",
        decision,
      );
    } catch (e) {
      approvalsApi.setState(approvalId, "pending");
      toast(
        e instanceof ApiError && e.code === "device_offline" ? "设备离线，决定未送达" : "提交失败，请重试",
        { error: true },
      );
    }
  };

  const setArchived = (next: boolean) =>
    confirm({
      title: next ? "归档这个会话？" : "取消归档？",
      body: next
        ? "归档后会话变为只读，历史记录保留，随时可以在列表中找到。"
        : "会话将恢复为可继续对话的状态。",
      confirmLabel: next ? "归档" : "取消归档",
      action: async () => {
        await archiveThread(threadId, next);
      },
    });

  const headerOverlay = (
    <>
      {!online && device ? (
        <div className="notice-banner warn">设备{statusLabel(device.status)}</div>
      ) : null}
      {archived ? <div className="notice-banner">已归档</div> : null}
      {unassigned ? (
        <div className="notice-banner info">未归类，只读</div>
      ) : null}
    </>
  );

  const threadView = history.isLoading ? (
    <div style={{ flex: 1, display: "grid", placeItems: "center" }}>
      <Spinner />
    </div>
  ) : (
    <ThreadView
      history={history.data?.groups ?? []}
      live={live}
      approvals={threadApprovals}
      onDecide={decide}
      approvalsLocked={!online}
      hasMoreHistory={history.data?.hasMore}
      loadingMore={history.isFetching && !history.isLoading}
      onLoadOlder={() => setTurnCount((n) => n + 12)}
      headerOverlay={headerOverlay}
      draftKey={threadId}
      canSend={canSend}
      lockedReason={lockedReason}
      running={running}
      runtimeStatus={thread?.status ?? null}
      runtimeConnected={online}
      busy={busyIds.has(threadId)}
      onSend={send}
      onRetry={retry}
      onInterrupt={interrupt}
    />
  );

  const topbar = (
    <TopBar
      title={thread?.title ?? "会话"}
      subtitle={device && project ? `${device.displayName} · ${project.displayName}` : undefined}
      onBack={() => back({ name: "project", deviceId, projectId })}
      onTitleClick={() => setSwitcherOpen(true)}
      trailing={
        <>
          {!unassigned ? (
            <button
              className="icon-btn"
              onClick={() => setArchived(!archived)}
              aria-label={archived ? "取消归档" : "归档会话"}
            >
              {archived ? <IconRefresh size={18} /> : <IconArchive size={18} />}
            </button>
          ) : null}
          <ConnIndicator />
        </>
      }
    />
  );

  if (!wide) {
    return (
      <div className="page">
        {topbar}
        {threadView}
        <ThreadSwitcher
          open={switcherOpen}
          onClose={() => setSwitcherOpen(false)}
          currentThreadId={threadId}
        />
        {confirmNode}
      </div>
    );
  }

  const sortedThreads = [...(projectThreads.data ?? [])].sort(
    (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
  );

  return (
    <div className="page">
      <div className="detail-grid">
        <aside className="detail-side">
          <TopBar
            title={project?.displayName ?? "项目"}
            subtitle={device?.displayName}
            onBack={() => back({ name: "device", deviceId })}
          />
          <div className="page-scroll">
            <div className="page-col">
              <div className="list-group" style={{ border: "none", background: "transparent" }}>
                {sortedThreads.map((t) => (
                  <SwipeActionRow
                    key={t.id}
                    icon={t.archived ? <IconRefresh size={18} /> : <IconArchive size={18} />}
                    label={t.archived ? "恢复" : "归档"}
                    busy={busyIds.has(t.id)}
                    disabled={!online || unassigned}
                    onAction={() => archiveThread(t.id, !t.archived)}
                  >
                    <ThreadRow
                      thread={t}
                      onClick={() =>
                        navigate({ name: "thread", deviceId, projectId, threadId: t.id })
                      }
                    />
                  </SwipeActionRow>
                ))}
              </div>
            </div>
          </div>
        </aside>
        <div className="detail-main">
          {topbar}
          {threadView}
        </div>
      </div>
      <ThreadSwitcher
        open={switcherOpen}
        onClose={() => setSwitcherOpen(false)}
        currentThreadId={threadId}
      />
      {confirmNode}
    </div>
  );
}
