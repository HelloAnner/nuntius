/* Thread page: the focused conversation surface. */
import { useEffect, useMemo, useState } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IconArchive,
  Spinner,
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
import { useMedia, useNavigate } from "../hooks";
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
  const back = useRoute((s) => s.back);
  const wide = useMedia("(min-width: 768px)");
  const [switcherOpen, setSwitcherOpen] = useState(false);
  const [turnCount, setTurnCount] = useState(12);
  const [routeGraceElapsed, setRouteGraceElapsed] = useState(false);
  const { confirm, node: confirmNode } = useConfirmAction();
  const [actionBusy, setActionBusy] = useState(false);

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
  const running =
    live.turns.some((t) => !liveStore.isTerminal(t)) || thread?.status === "active";

  const canSend = Boolean(online && !unassigned && !archived && thread);
  const lockedReason = !thread
    ? "会话加载中…"
    : archived
      ? "会话已归档，归档的会话只读"
      : unassigned
        ? "未归类会话不能继续执行"
        : !online
          ? `设备${statusLabel(device?.status ?? "offline")}，无法发送`
          : null;

  const send = async (text: string) => {
    const idemKey = newIdemKey();
    try {
      const receipt = await api.startTurn(
        threadId,
        text,
        turnOptionsForAccess(accessMode),
        idemKey,
      );
      liveStore.addOptimistic(threadId, receipt.commandId, text);
      trackCommand(qc, receipt.commandId, threadId, "turn.start");
    } catch (e) {
      toast(
        e instanceof ApiError && e.code === "device_offline" ? "设备离线，消息未发送" : "发送失败，请重试",
        { error: true },
      );
    }
  };

  const steer = async (text: string) => {
    const idemKey = newIdemKey();
    try {
      const receipt = await api.steerTurn(threadId, text, idemKey);
      liveStore.appendSteerEcho(threadId, text);
      trackCommand(qc, receipt.commandId, threadId, "turn.steer");
      toast("指导已发送");
    } catch {
      toast("指导发送失败", { error: true });
    }
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

  const archive = () =>
    confirm({
      title: "归档这个会话？",
      body: "归档后会话变为只读，历史记录保留，随时可以在列表中找到。",
      confirmLabel: "归档",
      action: async () => {
        setActionBusy(true);
        try {
          const receipt = await api.archiveThread(threadId);
          trackCommand(qc, receipt.commandId, threadId, "thread.archive");
          await qc.invalidateQueries({ queryKey: ["projectThreads", deviceId, projectId] });
          await qc.invalidateQueries({ queryKey: ["allThreads"] });
          toast("已归档");
        } catch {
          toast("归档失败", { error: true });
        } finally {
          setActionBusy(false);
        }
      },
    });

  const headerOverlay = (
    <>
      {!online && device ? (
        <div className="notice-banner warn">
          设备{statusLabel(device.status)} · 以下为服务器已同步的历史
          {thread?.lastSyncedAt ? `（同步于 ${thread.lastSyncedAt.slice(0, 16).replace("T", " ")}）` : ""}
          ，不能继续执行。
        </div>
      ) : null}
      {archived ? <div className="notice-banner">会话已归档，内容只读。</div> : null}
      {unassigned ? (
        <div className="notice-banner info">未归类会话：历史可阅读，回到设备本地归类后才能继续对话。</div>
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
      busy={actionBusy}
      onSend={send}
      onSteer={steer}
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
          {!archived && !unassigned ? (
            <button className="icon-btn" onClick={archive} aria-label="归档会话">
              <IconArchive size={18} />
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
                  <ThreadRow
                    key={t.id}
                    thread={t}
                    onClick={() =>
                      navigate({ name: "thread", deviceId, projectId, threadId: t.id })
                    }
                  />
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
