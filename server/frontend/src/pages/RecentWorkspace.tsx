import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Empty, IconClock, IconPlus, Spinner, type ThreadSummary } from "@nuntius/shared";
import { api } from "../api";
import { usePendingArchiveIds } from "../archiveOutbox";
import { TopBar } from "../components";
import { useMedia, useNavigate } from "../hooks";
import {
  loadLastRecentThreadId,
  saveLastRecentThreadId,
  selectRecentWorkspaceThread,
} from "../recentWorkspace";
import { useSession } from "../stores";
import { NewThreadSheet } from "../sheets/NewThreadSheet";
import { RecentsPage } from "./Recents";
import { ThreadPage } from "./Thread";

export function RecentsEntryRoute() {
  const wide = useMedia("(min-width: 900px)");
  return wide ? <DesktopRecentsEntry /> : <RecentsPage />;
}

function DesktopRecentsEntry() {
  const navigate = useNavigate();
  const userId = useSession((state) => state.session?.userId);
  const pendingArchiveIds = usePendingArchiveIds(userId);
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const [lastThreadId] = useState(() => loadLastRecentThreadId());
  const target = selectRecentWorkspaceThread(
    threads.data ?? [],
    lastThreadId,
    pendingArchiveIds,
  );

  useEffect(() => {
    if (!target) return;
    navigate({ name: "recentThread", threadId: target.id }, { replace: true });
  }, [navigate, target]);

  if (threads.isLoading || target) {
    return (
      <div className="page boot-screen">
        <Spinner />
      </div>
    );
  }

  return (
    <EmptyRecentWorkspace
      failed={threads.isError}
      onRetry={() => void threads.refetch()}
    />
  );
}

export function RecentThreadRoute({ threadId }: { threadId: string }) {
  const navigate = useNavigate();
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const snapshot = useQuery<ThreadSummary | undefined>({
    queryKey: ["threadSnapshot", threadId],
    queryFn: async () => undefined,
    enabled: false,
  });
  const thread = threads.data?.find((item) => item.id === threadId) ?? snapshot.data;

  useEffect(() => {
    if ((threads.isSuccess || threads.isError) && !thread) {
      navigate({ name: "recents" }, { replace: true });
    }
  }, [navigate, thread, threads.isError, threads.isSuccess]);

  useEffect(() => {
    if (thread && !thread.archived) saveLastRecentThreadId(thread.id);
  }, [thread]);

  if (!thread) {
    return (
      <div className="page boot-screen">
        <Spinner />
      </div>
    );
  }

  return (
    <ThreadPage
      navigationContext="recents"
      deviceId={thread.deviceId}
      projectId={thread.projectId}
      threadId={thread.id}
    />
  );
}

function EmptyRecentWorkspace({
  failed,
  onRetry,
}: {
  failed: boolean;
  onRetry: () => void;
}) {
  const navigate = useNavigate();
  const [creating, setCreating] = useState(false);
  const openNewThread = () => setCreating(true);
  const action = failed ? (
    <button className="btn outline" onClick={onRetry}>重新加载</button>
  ) : (
    <button className="btn primary" onClick={openNewThread}>
      <IconPlus size={15} />新建会话
    </button>
  );

  return (
    <div className="page thread-page recent-workspace-empty">
      <div className="detail-grid">
        <aside className="detail-side">
          <div className="thread-sidebar-scroll">
            <header className="thread-sidebar-context static">
              <strong>最近会话</strong>
              <span>运行中优先 · 按创建时间排序</span>
            </header>
            <Empty
              icon={<IconClock size={21} />}
              headline={failed ? "暂时无法加载" : "还没有会话"}
              hint={failed ? "重新加载后再试" : "新建会话后会显示在这里"}
            />
          </div>
        </aside>
        <div className="detail-main">
          <TopBar
            title="最近会话"
            subtitle={failed ? "会话数据加载失败" : "从一条新会话开始"}
            trailing={failed ? null : (
              <button className="btn primary" onClick={openNewThread}>
                <IconPlus size={15} />新建会话
              </button>
            )}
          />
          <div className="recent-workspace-placeholder">
            <Empty
              icon={<IconClock size={24} />}
              headline={failed ? "无法读取最近会话" : "开始第一次对话"}
              hint={failed ? "检查连接后重新加载" : "创建会话后，可以在左侧列表中快速来回切换"}
              action={action}
            />
          </div>
        </div>
      </div>
      <NewThreadSheet
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(threadId) =>
          navigate({ name: "recentThread", threadId }, { replace: true })
        }
      />
    </div>
  );
}
