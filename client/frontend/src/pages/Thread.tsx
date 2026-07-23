/* Thread page (local console): conversation with the selected on-device provider. */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IconArchive,
  IconEdit,
  IconMore,
  IconPlus,
  RenameThreadSheet,
  SwipeActionRow,
  ThreadView,
  compareThreadStatusCreation,
  newIdemKey,
  providerLabel,
  threadNeedsReview,
  truncateEnd,
  useConfirmAction,
  useToast,
  type ApprovalView,
  type AttachmentView,
  type HistoryGroup,
  type HistoryRecord,
  type ThreadSummary,
} from "@nuntius/shared";
import { api } from "../api";
import { useArchiveThreadAction, useMedia, useNavigate, useRenameThreadAction } from "../hooks";
import { liveStore, useApprovals, useRoute, useThreadLive } from "../stores";
import { ConnIndicator, ThreadRow, TopBar } from "../components";
import { NewThreadSheet } from "../sheets/NewThreadSheet";

function groupHistory(records: HistoryRecord[]): HistoryGroup[] {
  const groups: HistoryGroup[] = [];
  const byTurn = new Map<string, HistoryGroup>();
  for (const r of records) {
    if (r.turn) {
      const group: HistoryGroup = {
        turn: {
          id: r.turn.id,
          ordinal: r.turn.ordinal,
          status: r.turn.status,
          startedAt: r.turn.startedAt,
          completedAt: r.turn.completedAt,
        },
        items: [],
      };
      groups.push(group);
      byTurn.set(r.turn.id, group);
    }
  }
  for (const r of records) {
    if (r.item) {
      byTurn.get(r.item.turnId)?.items.push({
        id: r.item.id,
        ordinal: r.item.ordinal,
        kind: r.item.kind,
        text:
          r.item.contentText ??
          (r.item.structuredDetail ? JSON.stringify(r.item.structuredDetail, null, 2) : ""),
        status: r.item.status,
        occurredAt: r.item.occurredAt,
        truncated: r.item.isTruncated,
        attachments: r.item.attachments ?? [],
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
  const renameThread = useRenameThreadAction();
  const back = useRoute((s) => s.back);
  const wide = useMedia("(min-width: 768px)");
  const { confirm, node: confirmNode } = useConfirmAction();
  const [creating, setCreating] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [renameOpen, setRenameOpen] = useState(false);
  const [sendBusy, setSendBusy] = useState(false);
  const [interruptBusy, setInterruptBusy] = useState(false);
  const sendPendingRef = useRef(false);
  const interruptPendingRef = useRef(false);
  const viewedPendingRef = useRef<string | null>(null);

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
  const threadSnapshot = useQuery<ThreadSummary | undefined>({
    queryKey: ["threadSnapshot", threadId],
    queryFn: async () => undefined,
    enabled: false,
  });

  const project = projects.data?.find((p) => p.id === projectId);
  const historyThread = history.data?.find((record) => record.thread)?.thread ?? undefined;
  const thread =
    projectThreads.data?.find((t) => t.id === threadId) ??
    allThreads.data?.find((t) => t.id === threadId) ??
    threadSnapshot.data ??
    historyThread;
  const fullThreadTitle = thread?.title || "会话";

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

  const providerStatus = info.data?.providers.find((status) => status.provider === thread?.provider);
  const providerAvailable = providerStatus?.available ?? thread?.provider === "codex";
  const providerConnected = providerStatus?.status === "online";
  const unassigned = project?.kind === "system_unassigned";
  const canCreate = Boolean(
    project && !unassigned && info.data?.providers.some((status) => status.available),
  );
  const createLabel = canCreate
    ? "在当前项目新建会话"
    : info.isLoading
      ? "正在检查可用的编码代理"
      : "没有可用的编码代理";
  const archived = thread?.archived ?? false;
  // SQLite is authoritative. Live events render output but never manufacture
  // the execution state shown to the user.
  const running = thread?.status === "active";
  const recovering = thread?.status === "recovering";

  const canSend = Boolean(providerAvailable && !archived && !recovering && thread);
  const lockedReason = !thread
    ? "会话加载中…"
    : archived
      ? "已归档"
      : !providerAvailable
        ? `${providerLabel(thread.provider)} 未安装或不可用`
        : recovering
          ? "正在恢复运行连接…"
          : null;

  useEffect(() => {
    if (!thread || !threadNeedsReview(thread)) viewedPendingRef.current = null;
  }, [thread]);

  const markLatestViewed = useCallback(() => {
    if (
      !thread
      || !threadNeedsReview(thread)
      || history.isFetching
      || history.isError
      || viewedPendingRef.current === threadId
    ) return;
    viewedPendingRef.current = threadId;
    void api.markThreadViewed(threadId).then(({ thread: updated }) => {
      const replace = (items: ThreadSummary[] | undefined) => items?.map((item) =>
        item.id === threadId ? updated : item,
      );
      qc.setQueriesData<ThreadSummary[]>({ queryKey: ["threads"] }, replace);
      qc.setQueriesData<ThreadSummary[]>({ queryKey: ["projectThreads"] }, replace);
      qc.setQueryData(["threadSnapshot", threadId], updated);
    }).catch(() => {
      viewedPendingRef.current = null;
    });
  }, [history.isError, history.isFetching, qc, thread, threadId]);

  const send = async (text: string, attachments: AttachmentView[] = [], clientMessageId = newIdemKey()) => {
    if (sendPendingRef.current) return;
    sendPendingRef.current = true;
    setSendBusy(true);
    const provisionalId = `pending:${newIdemKey()}`;
    liveStore.addOptimistic(threadId, provisionalId, text, attachments, clientMessageId);
    try {
      await api.startTurn(threadId, text, clientMessageId);
      liveStore.applyCommandStatus(provisionalId, "completed");
    } catch (e) {
      const message = e instanceof Error ? e.message : "发送失败";
      liveStore.applyCommandStatus(provisionalId, "failed", "request_failed", message);
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
      await api.interruptTurn(threadId);
      toast("中断请求已发送");
    } catch (e) {
      toast(e instanceof Error ? e.message : "中断失败", { error: true });
    } finally {
      interruptPendingRef.current = false;
      setInterruptBusy(false);
    }
  };

  const decide = async (approvalId: string, decision: string, response?: unknown) => {
    const store = useApprovals.getState();
    store.setState(approvalId, "responding");
    try {
      await api.decideApproval(approvalId, decision, response);
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

  const setArchived = () =>
    confirm({
      title: "归档这个会话？",
      body: "归档后会从所有会话页面隐藏，历史记录仍保留在本机和服务器数据库中。",
      confirmLabel: "归档",
      action: () => {
        if (archiveThread(threadId)) {
          navigate({ name: "project", projectId }, { replace: true });
        }
      },
    });

  const groups = useMemo(() => groupHistory(history.data ?? []), [history.data]);

  const threadView = (
    <ThreadView
      loading={history.isLoading}
      history={groups}
      live={live}
      approvals={threadApprovals}
      onDecide={decide}
      approvalsLocked={!providerConnected}
      draftKey={threadId}
      canSend={canSend}
      lockedReason={lockedReason}
      running={running}
      runtimeStatus={thread?.status ?? null}
      runtimeConnected={providerConnected || providerAvailable}
      busy={busyIds.has(threadId) || sendBusy || interruptBusy}
      onSend={send}
      onRetry={retry}
      onLatestVisible={markLatestViewed}
      onInterrupt={interrupt}
    />
  );

  const topbar = (
    <TopBar
      title={
        <span className="renameable-thread-title">
          <span>{truncateEnd(fullThreadTitle)}</span>
          <IconEdit size={13} />
        </span>
      }
      titleHint={`重命名会话：${fullThreadTitle}`}
      onTitleClick={thread ? () => setRenameOpen(true) : undefined}
      subtitle={thread ? `${project?.displayName ?? "项目"} · ${providerLabel(thread.provider)}` : project?.displayName}
      onBack={() => back({ name: "project", projectId })}
      trailing={
        <>
          {project && !unassigned ? (
            <button
              className="icon-btn"
              onClick={() => setCreating(true)}
              disabled={!canCreate}
              aria-label={createLabel}
              title={createLabel}
            >
              <IconPlus size={19} />
            </button>
          ) : null}
          {wide && !archived ? (
            <button
              className="icon-btn"
              onClick={setArchived}
              aria-label="归档会话"
              title="归档会话"
            >
              <IconArchive size={18} />
            </button>
          ) : null}
          <button
            className="icon-btn"
            onClick={() => setMenuOpen((value) => !value)}
            aria-label="更多会话操作"
            aria-expanded={menuOpen}
          >
            <IconMore size={19} />
          </button>
          <ConnIndicator />
        </>
      }
    />
  );

  const threadMenu = menuOpen ? (
    <div className="thread-actions-menu" role="menu">
      {thread ? (
        <button role="menuitem" onClick={() => { setMenuOpen(false); setRenameOpen(true); }}>
          <IconEdit size={15} />重命名会话
        </button>
      ) : null}
      {!archived ? (
        <button role="menuitem" onClick={() => { setMenuOpen(false); setArchived(); }}>
          <IconArchive size={15} />归档会话
        </button>
      ) : null}
    </div>
  ) : null;

  const renameSheet = (
    <RenameThreadSheet
      thread={thread ?? null}
      open={renameOpen}
      onClose={() => setRenameOpen(false)}
      onRename={(title) => thread ? renameThread(thread, title) : Promise.resolve()}
    />
  );

  const newThreadSheet = project && !unassigned ? (
    <NewThreadSheet
      projectId={projectId}
      open={creating}
      onClose={() => setCreating(false)}
      onCreated={(createdThreadId) =>
        navigate({ name: "thread", projectId, threadId: createdThreadId })
      }
    />
  ) : null;

  if (!wide) {
    return (
      <div className="page thread-page">
        {topbar}
        {threadView}
        {threadMenu}
        {renameSheet}
        {newThreadSheet}
        {confirmNode}
      </div>
    );
  }

  const sortedThreads = [...(projectThreads.data ?? [])]
    .filter((thread) => !busyIds.has(thread.id))
    .sort(compareThreadStatusCreation);

  return (
    <div className="page thread-page">
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
                    icon={<IconArchive size={18} />}
                    label="归档"
                    busy={busyIds.has(t.id)}
                    onAction={() => archiveThread(t.id)}
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
          {threadMenu}
        </div>
      </div>
      {newThreadSheet}
      {renameSheet}
      {confirmNode}
    </div>
  );
}
