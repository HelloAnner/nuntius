/* Thread page (local console): conversation with the on-device Codex. */
import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IconArchive,
  IconRefresh,
  Spinner,
  SwipeActionRow,
  ThreadView,
  newIdemKey,
  useConfirmAction,
  useToast,
  type ApprovalView,
  type HistoryGroup,
  type HistoryRecord,
} from "@nuntius/shared";
import { api } from "../api";
import { useArchiveThreadAction, useMedia, useNavigate } from "../hooks";
import { liveStore, useApprovals, useRoute, useThreadLive } from "../stores";
import { ConnIndicator, ThreadRow, TopBar } from "../components";

function groupHistory(records: HistoryRecord[]): HistoryGroup[] {
  const groups: HistoryGroup[] = [];
  for (const r of records) {
    if (r.turn) {
      groups.push({
        turn: {
          id: r.turn.id,
          ordinal: r.turn.ordinal,
          status: r.turn.status,
          startedAt: r.turn.startedAt,
          completedAt: r.turn.completedAt,
        },
        items: [],
      });
    } else if (r.item && groups.length > 0) {
      groups[groups.length - 1].items.push({
        id: r.item.id,
        kind: r.item.kind,
        text:
          r.item.contentText ??
          (r.item.structuredDetail ? JSON.stringify(r.item.structuredDetail, null, 2) : ""),
        status: r.item.status,
        truncated: r.item.isTruncated,
      });
    }
  }
  return groups;
}

export function ThreadPage({ projectId, threadId }: { projectId: string; threadId: string }) {
  const toast = useToast();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const { archive: archiveThread, busyIds } = useArchiveThreadAction();
  const back = useRoute((s) => s.back);
  const wide = useMedia("(min-width: 768px)");
  const { confirm, node: confirmNode } = useConfirmAction();
  const [actionBusy, setActionBusy] = useState(false);

  const info = useQuery({ queryKey: ["info"], queryFn: api.info });
  const projects = useQuery({ queryKey: ["projects"], queryFn: api.projects });
  const projectThreads = useQuery({
    queryKey: ["projectThreads", projectId],
    queryFn: () => api.projectThreads(projectId),
  });
  const allThreads = useQuery({ queryKey: ["threads"], queryFn: api.threads });
  const history = useQuery({
    queryKey: ["history", threadId],
    queryFn: () => api.history(threadId),
  });

  const project = projects.data?.find((p) => p.id === projectId);
  const thread =
    projectThreads.data?.find((t) => t.id === threadId) ??
    allThreads.data?.find((t) => t.id === threadId);

  useEffect(() => {
    const missingProject = projects.isSuccess && !project;
    const missingThread =
      projectThreads.isSuccess &&
      allThreads.isSuccess &&
      !projectThreads.isFetching &&
      !allThreads.isFetching &&
      !thread;
    if (projects.isError || missingProject || missingThread) {
      navigate({ name: "overview" }, { replace: true });
    }
  }, [
    allThreads.isFetching,
    allThreads.isSuccess,
    navigate,
    project,
    projectThreads.isFetching,
    projectThreads.isSuccess,
    projects.isError,
    projects.isSuccess,
    thread,
  ]);

  const live = useThreadLive(threadId);
  const approvals = useApprovals((s) => s.items);
  const threadApprovals: ApprovalView[] = useMemo(
    () =>
      Object.values(approvals)
        .filter((a) => a.threadId === threadId)
        .map((a) => ({ ...a, projectName: project?.displayName, threadTitle: thread?.title }))
        .sort((a, b) => Date.parse(a.occurredAt) - Date.parse(b.occurredAt)),
    [approvals, threadId, project, thread],
  );

  const appRunning = info.data?.appServerRunning ?? false;
  const archived = thread?.archived ?? false;
  const latestAuthoritativeTurn = [...live.turns]
    .reverse()
    .find((turn) => !turn.id.startsWith("local:"));
  const running = live.turns.some((turn) => !liveStore.isTerminal(turn)) ||
    (latestAuthoritativeTurn ? false : thread?.status === "active");

  const canSend = Boolean(appRunning && !archived && thread);
  const lockedReason = !thread
    ? "会话加载中…"
    : archived
      ? "已归档"
      : !appRunning
        ? "Codex App Server 未运行"
        : null;

  const send = async (text: string) => {
    const provisionalId = `pending:${newIdemKey()}`;
    liveStore.addOptimistic(threadId, provisionalId, text);
    try {
      await api.startTurn(threadId, text);
      liveStore.applyCommandStatus(provisionalId, "completed");
    } catch (e) {
      const message = e instanceof Error ? e.message : "发送失败";
      liveStore.applyCommandStatus(provisionalId, "failed", "request_failed", message);
    }
  };

  const retry = (turnId: string, text: string) => {
    liveStore.removeOptimistic(threadId, turnId);
    void send(text);
  };

  const interrupt = async () => {
    try {
      await api.interruptTurn(threadId);
      toast("中断请求已发送");
    } catch (e) {
      toast(e instanceof Error ? e.message : "中断失败", { error: true });
    }
  };

  const decide = async (approvalId: string, decision: string) => {
    const store = useApprovals.getState();
    store.setState(approvalId, "responding");
    try {
      await api.decideApproval(approvalId, decision);
      store.setState(
        approvalId,
        decision === "decline" || decision === "cancel" ? "denied" : "approved",
        decision,
      );
    } catch (e) {
      store.setState(approvalId, "pending");
      toast(e instanceof Error ? e.message : "提交失败，请重试", { error: true });
    }
  };

  const setArchived = (next: boolean) =>
    confirm({
      title: next ? "归档这个会话？" : "取消归档？",
      body: next
        ? "归档后会话变为只读，历史记录保留。"
        : "会话将恢复为可继续对话的状态。",
      confirmLabel: next ? "归档" : "取消归档",
      action: async () => {
        setActionBusy(true);
        try {
          await api.archiveThread(threadId, next);
          await qc.invalidateQueries({ queryKey: ["projectThreads", projectId] });
          await qc.invalidateQueries({ queryKey: ["threads"] });
          toast(next ? "已归档" : "已恢复");
        } catch (e) {
          toast(e instanceof Error ? e.message : "操作失败", { error: true });
        } finally {
          setActionBusy(false);
        }
      },
    });

  const groups = useMemo(() => groupHistory(history.data ?? []), [history.data]);

  const headerOverlay = (
    <>
      {!appRunning && !info.isLoading ? (
        <div className="notice-banner warn">Codex App Server 未运行</div>
      ) : null}
      {archived ? <div className="notice-banner">已归档</div> : null}
    </>
  );

  const threadView = history.isLoading ? (
    <div style={{ flex: 1, display: "grid", placeItems: "center" }}>
      <Spinner />
    </div>
  ) : (
    <ThreadView
      history={groups}
      live={live}
      approvals={threadApprovals}
      onDecide={decide}
      approvalsLocked={!appRunning}
      headerOverlay={headerOverlay}
      draftKey={threadId}
      canSend={canSend}
      lockedReason={lockedReason}
      running={running}
      busy={actionBusy}
      onSend={send}
      onRetry={retry}
      onInterrupt={interrupt}
    />
  );

  const topbar = (
    <TopBar
      title={thread?.title ?? "会话"}
      subtitle={project?.displayName}
      onBack={() => back({ name: "project", projectId })}
      trailing={
        <>
          <button
            className="icon-btn"
            onClick={() => setArchived(!archived)}
            aria-label={archived ? "取消归档" : "归档会话"}
            title={archived ? "取消归档" : "归档会话"}
          >
            {archived ? <IconRefresh size={18} /> : <IconArchive size={18} />}
          </button>
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
            onBack={() => back({ name: "projects" })}
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
                    onAction={() => archiveThread(t.id, !t.archived)}
                  >
                    <ThreadRow
                      thread={t}
                      onClick={() => navigate({ name: "thread", projectId, threadId: t.id })}
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
      {confirmNode}
    </div>
  );
}
