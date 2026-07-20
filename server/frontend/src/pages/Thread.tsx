/* Thread page: the focused conversation surface. */
import { useEffect, useMemo, useRef, useState } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconClock,
  IconList,
  IconMore,
  IconPlus,
  Spinner,
  ThreadView,
  compareThreadCreation,
  newIdemKey,
  providerLabel,
  statusLabel,
  truncateEnd,
  useConfirmAction,
  useToast,
  type ApprovalView,
  type AttachmentView,
  type HistoryGroup,
  type HistoryItemView,
  type ThreadSummary,
} from "@nuntius/shared";
import { api, ApiError } from "../api";
import { trackCommand } from "../events";
import { useArchiveThreadAction, useMedia, useNavigate } from "../hooks";
import { liveStore, useAccessMode, useApprovals, useThreadLive } from "../stores";
import { ProviderBadge, StatusDot, ThreadListItem, TopBar, threadTone } from "../components";
import { NewThreadSheet } from "../sheets/NewThreadSheet";
import { ThreadSwitcher } from "../sheets/ThreadSwitcher";

function mapHistoryItem(item: HistoryItemView) {
  return {
    id: item.id,
    ordinal: item.ordinal,
    kind: item.kind,
    text:
      item.contentText ??
      (item.structuredDetail ? JSON.stringify(item.structuredDetail, null, 2) : ""),
    status: item.status,
    occurredAt: item.occurredAt,
    truncated: item.isTruncated,
    attachments: item.attachments ?? [],
  };
}

export function ThreadPage({
  deviceId,
  projectId,
  threadId,
  navigationContext = "project",
}: {
  deviceId: string;
  projectId: string;
  threadId: string;
  navigationContext?: "project" | "recents";
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const accessMode = useAccessMode((state) => state.mode);
  const navigate = useNavigate();
  const { archive: archiveThread, busyIds } = useArchiveThreadAction();
  const wide = useMedia("(min-width: 900px)");
  const fromRecents = navigationContext === "recents";
  const [switcherOpen, setSwitcherOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [turnCount, setTurnCount] = useState(12);
  const [routeGraceElapsed, setRouteGraceElapsed] = useState(false);
  const { confirm, node: confirmNode } = useConfirmAction();
  const [sendBusy, setSendBusy] = useState(false);
  const [interruptBusy, setInterruptBusy] = useState(false);
  const sendPendingRef = useRef(false);
  const interruptPendingRef = useRef(false);

  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projects = useQuery({
    queryKey: ["projects", deviceId],
    queryFn: () => api.projects(deviceId),
  });
  const projectThreads = useQuery({
    queryKey: ["projectThreads", deviceId, projectId],
    queryFn: () => api.projectThreads(deviceId, projectId),
    refetchInterval: (query) => {
      if (qc.getQueryData<ThreadSummary>(["threadSnapshot", threadId])?.archived) return false;
      return (query.state.data as { id: string }[] | undefined)?.some(
        (item) => item.id === threadId,
      )
        ? false
        : 700;
    },
  });
  const allThreads = useQuery({
    queryKey: ["allThreads"],
    queryFn: () => api.allThreads(),
    refetchInterval: (query) => {
      if (qc.getQueryData<ThreadSummary>(["threadSnapshot", threadId])?.archived) return false;
      return (query.state.data as { id: string }[] | undefined)?.some(
        (item) => item.id === threadId,
      )
        ? false
        : 700;
    },
  });
  const threadSnapshot = useQuery<ThreadSummary | undefined>({
    queryKey: ["threadSnapshot", threadId],
    queryFn: async () => undefined,
    enabled: false,
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
    allThreads.data?.find((t) => t.id === threadId) ??
    threadSnapshot.data;
  const fullThreadTitle = thread?.title || "会话";

  useEffect(() => {
    setTurnCount(12);
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
      navigate(fromRecents ? { name: "recents" } : { name: "devices" }, { replace: true });
    }
  }, [
    allThreads.isFetching,
    allThreads.isSuccess,
    device,
    devices.isError,
    devices.isSuccess,
    fromRecents,
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
  const providerAvailable =
    device?.providers.find((status) => status.provider === thread?.provider)?.available ??
    thread?.provider === "codex";
  const providerConnected =
    device?.providers.find((status) => status.provider === thread?.provider)?.status === "online" ||
    (thread?.provider === "codex" && device?.providers.length === 0 && online);
  const unassigned = project?.kind === "system_unassigned";
  const archived = thread?.archived ?? false;
  // The server SQLite projection is authoritative. Browser memory is only a
  // transient rendering layer for streamed output.
  const running = thread?.status === "active";
  const recovering = thread?.status === "recovering";

  const canSend = Boolean(
    online && providerAvailable && !unassigned && !archived && !recovering && thread,
  );
  const lockedReason = !thread
    ? "会话加载中…"
    : archived
      ? "已归档"
      : unassigned
        ? "未归类，只读"
        : !online
          ? `设备${statusLabel(device?.status ?? "offline")}`
          : !providerAvailable
            ? `${providerLabel(thread.provider)} 未安装或不可用`
          : recovering
            ? "正在恢复运行连接…"
            : null;

  const send = async (text: string, attachments: AttachmentView[] = [], clientMessageId = newIdemKey()) => {
    if (sendPendingRef.current) return;
    sendPendingRef.current = true;
    setSendBusy(true);
    const idemKey = newIdemKey();
    const provisionalId = `pending:${idemKey}`;
    liveStore.addOptimistic(threadId, provisionalId, text, attachments, clientMessageId);
    try {
      const receipt = await api.startTurn(
        threadId,
        text,
        attachments.map((attachment) => attachment.id),
        clientMessageId,
        accessMode,
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
    } finally {
      sendPendingRef.current = false;
      setSendBusy(false);
    }
  };

  const retry = (turnId: string, text: string, attachments: AttachmentView[]) => {
    liveStore.removeOptimistic(threadId, turnId);
    void send(text, attachments);
  };

  const interrupt = async () => {
    if (interruptPendingRef.current) return;
    interruptPendingRef.current = true;
    setInterruptBusy(true);
    try {
      const receipt = await api.interruptTurn(threadId);
      trackCommand(qc, receipt.commandId, threadId, "turn.interrupt");
      toast("中断请求已发送");
    } catch {
      toast("中断失败，设备可能已离线", { error: true });
    } finally {
      interruptPendingRef.current = false;
      setInterruptBusy(false);
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

  const setArchived = () =>
    confirm({
      title: "归档这个会话？",
      body: "归档后会从所有会话页面隐藏，历史记录仍保留在服务器数据库中。",
      confirmLabel: "归档",
      action: async () => {
        await archiveThread(threadId);
      },
    });

  const threadView = (
    <ThreadView
      loading={history.isLoading}
      history={history.data?.groups ?? []}
      live={live}
      approvals={threadApprovals}
      onDecide={decide}
      approvalsLocked={!online || !providerConnected}
      hasMoreHistory={history.data?.hasMore}
      loadingMore={history.isFetching && !history.isLoading}
      onLoadOlder={() => setTurnCount((n) => n + 12)}
      draftKey={threadId}
      canSend={canSend}
      lockedReason={lockedReason}
      running={running}
      runtimeStatus={thread?.status ?? null}
      runtimeConnected={online && providerAvailable}
      busy={busyIds.has(threadId) || sendBusy || interruptBusy}
      onSend={send}
      onUpload={(file, onProgress) => api.uploadAttachment(threadId, file, onProgress)}
      onDeleteAttachment={api.deleteAttachment}
      onRetry={retry}
      onInterrupt={interrupt}
    />
  );

  const pendingApproval = threadApprovals.some((approval) => approval.state === "pending");
  const currentTone = thread ? (pendingApproval ? "warning" : threadTone(thread)) : "offline";
  const currentStateLabel = pendingApproval ? "等待审批" : statusLabel(thread?.status ?? "offline");

  const mobileTopbar = (
    <TopBar
      title={truncateEnd(fullThreadTitle)}
      titleHint={fullThreadTitle}
      subtitle={device && project ? (
        <span className="thread-mobile-context">
          <StatusDot tone={online ? "success" : "offline"} />
          {device.displayName} / {project.displayName}{thread ? ` · ${providerLabel(thread.provider)}` : ""} · {online ? "在线" : statusLabel(device.status)}
        </span>
      ) : undefined}
      onBack={() =>
        fromRecents
          ? navigate({ name: "recents" }, { replace: true })
          : navigate({ name: "project", deviceId, projectId }, { replace: true })
      }
      trailing={
        <>
          <button className="icon-btn" onClick={() => setSwitcherOpen(true)} aria-label="切换会话">
            <IconList size={20} />
          </button>
          <button className="icon-btn" onClick={() => setMenuOpen((value) => !value)} aria-label="更多会话操作" aria-expanded={menuOpen}>
            <IconMore size={20} />
          </button>
        </>
      }
    />
  );

  const desktopTopbar = (
    <TopBar
      title={truncateEnd(fullThreadTitle)}
      titleHint={fullThreadTitle}
      subtitle={device && project ? `${device.displayName} / ${project.displayName}` : undefined}
      trailing={
        <>
          {thread ? <ProviderBadge provider={thread.provider} /> : null}
          <span className={`status-pill ${currentTone}`}><span className="status-dot" />{currentStateLabel}</span>
          {!unassigned && !archived ? (
            <button className="icon-btn" onClick={setArchived} aria-label="归档会话"><IconArchive size={18} /></button>
          ) : null}
          <button className="icon-btn" onClick={() => setMenuOpen((value) => !value)} aria-label="更多会话操作" aria-expanded={menuOpen}>
            <IconMore size={19} />
          </button>
        </>
      }
    />
  );

  const newThreadSheet = project && !unassigned ? (
    <NewThreadSheet
      deviceId={deviceId}
      projectId={projectId}
      open={creating}
      onClose={() => setCreating(false)}
      onCreated={(createdThreadId) =>
        navigate({ name: "thread", deviceId, projectId, threadId: createdThreadId })
      }
    />
  ) : null;

  const threadMenu = menuOpen ? (
    <div className="thread-actions-menu" role="menu">
      {project && !unassigned ? (
        <button role="menuitem" onClick={() => { setMenuOpen(false); setCreating(true); }} disabled={!online}>
          <IconPlus size={15} />新建会话
        </button>
      ) : null}
      {!unassigned && !archived ? (
        <button role="menuitem" onClick={() => { setMenuOpen(false); setArchived(); }}>
          <IconArchive size={15} />归档会话
        </button>
      ) : null}
    </div>
  ) : null;

  if (!wide) {
    return (
      <div className="page thread-page">
        {mobileTopbar}
        {threadView}
        {threadMenu}
        <ThreadSwitcher
          open={switcherOpen}
          onClose={() => setSwitcherOpen(false)}
          onNewThread={project && !unassigned && online ? () => setCreating(true) : undefined}
          currentThreadId={threadId}
          navigationContext={navigationContext}
        />
        {newThreadSheet}
        {confirmNode}
      </div>
    );
  }

  const sidebarThreads = [...(projectThreads.data ?? [])].sort(compareThreadCreation);
  const pendingThreadIds = new Set(
    Object.values(approvals)
      .flatMap((approval) => approval.state === "pending" && approval.threadId ? [approval.threadId] : []),
  );
  const ongoingThreads = sidebarThreads.filter(
    (item) => item.status === "active" || pendingThreadIds.has(item.id),
  );
  const recentThreads = sidebarThreads.filter(
    (item) => item.status !== "active" && !pendingThreadIds.has(item.id),
  );
  const selectSidebarThread = (item: ThreadSummary) =>
    navigate(
      fromRecents
        ? { name: "recentThread", threadId: item.id }
        : { name: "thread", deviceId, projectId, threadId: item.id },
    );

  return (
    <div className="page thread-page">
      <div className="detail-grid">
        <aside className="detail-side">
          <div className="thread-sidebar-scroll">
            <button className="thread-sidebar-context" onClick={() => navigate({ name: "project", deviceId, projectId })}>
              <strong>{project?.displayName ?? "项目"}</strong>
              <span>{device?.displayName ?? "设备"}{project?.pathHint ? ` · ${project.pathHint}` : ""}</span>
            </button>
            {project && !unassigned ? (
              <button className="quick-new-thread" onClick={() => setCreating(true)} disabled={!online}>
                <IconPlus size={15} />
                <strong>新建会话</strong>
                <small>同设备 · 同项目</small>
              </button>
            ) : null}
            {projectThreads.isLoading ? (
              <div className="detail-list-state"><Spinner /></div>
            ) : sidebarThreads.length === 0 ? (
              <Empty icon={<IconClock size={22} />} headline="还没有会话" />
            ) : (
              <>
                {ongoingThreads.length ? (
                  <ThreadSidebarGroup
                    label="进行中"
                    threads={ongoingThreads}
                    pendingThreadIds={pendingThreadIds}
                    currentThreadId={threadId}
                    onSelect={selectSidebarThread}
                  />
                ) : null}
                {recentThreads.length ? (
                  <ThreadSidebarGroup
                    label="最近"
                    threads={recentThreads}
                    pendingThreadIds={pendingThreadIds}
                    currentThreadId={threadId}
                    onSelect={selectSidebarThread}
                  />
                ) : null}
              </>
            )}
          </div>
        </aside>
        <div className="detail-main">
          {desktopTopbar}
          {threadView}
          {threadMenu}
        </div>
      </div>
      <ThreadSwitcher
        open={switcherOpen}
        onClose={() => setSwitcherOpen(false)}
        onNewThread={project && !unassigned && online ? () => setCreating(true) : undefined}
        currentThreadId={threadId}
        navigationContext={navigationContext}
      />
      {newThreadSheet}
      {confirmNode}
    </div>
  );
}

function ThreadSidebarGroup({
  label,
  threads,
  pendingThreadIds,
  currentThreadId,
  onSelect,
}: {
  label: string;
  threads: ThreadSummary[];
  pendingThreadIds: Set<string>;
  currentThreadId: string;
  onSelect: (thread: ThreadSummary) => void;
}) {
  return (
    <section className="thread-sidebar-group">
      <div className="thread-sidebar-label">{label}</div>
      {threads.map((thread) => (
        <ThreadListItem
          key={thread.id}
          thread={thread}
          pendingApproval={pendingThreadIds.has(thread.id)}
          selected={thread.id === currentThreadId}
          onClick={() => onSelect(thread)}
        />
      ))}
    </section>
  );
}
