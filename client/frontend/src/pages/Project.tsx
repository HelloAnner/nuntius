/* Project page: threads of one local project + new-thread entry. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconChat,
  IconPlus,
  Spinner,
  RenameThreadSheet,
  SwipeActionRow,
  compareThreadStatusCreation,
  newIdemKey,
  useConfirmAction,
  useToast,
  type ThreadSummary,
} from "@nuntius/shared";
import { api } from "../api";
import { useArchiveThreadAction, useNavigate, useRenameThreadAction } from "../hooks";
import { useRoute } from "../stores";
import { ConnIndicator, ThreadRow, TopBar } from "../components";
import { NewThreadSheet } from "../sheets/NewThreadSheet";

export function ProjectPage({ projectId }: { projectId: string }) {
  const navigate = useNavigate();
  const { archive, busyIds } = useArchiveThreadAction();
  const renameThread = useRenameThreadAction();
  const back = useRoute((s) => s.back);
  const toast = useToast();
  const qc = useQueryClient();
  const { confirm, node: confirmNode } = useConfirmAction();
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [renamingThread, setRenamingThread] = useState<ThreadSummary | null>(null);

  const info = useQuery({ queryKey: ["info"], queryFn: api.info });
  const projects = useQuery({ queryKey: ["projects"], queryFn: api.projects });
  const threads = useQuery({
    queryKey: ["projectThreads", projectId],
    queryFn: () => api.projectThreads(projectId),
  });

  const project = projects.data?.find((p) => p.id === projectId);

  useEffect(() => {
    if (!deleting && (projects.isError || (projects.isSuccess && !project))) {
      navigate({ name: "overview" }, { replace: true });
    }
  }, [deleting, navigate, project, projects.isError, projects.isSuccess]);

  const unassigned = project?.kind === "system_unassigned";
  const providerStatuses = info.data?.providers ?? [];
  const canCreate = Boolean(!unassigned && providerStatuses.some((status) => status.available));
  const sorted = [...(threads.data ?? [])]
    .filter((thread) => !busyIds.has(thread.id))
    .sort(compareThreadStatusCreation);

  const remove = () => {
    if (!project || unassigned || deleting) return;
    confirm({
      title: `删除「${project.displayName}」？`,
      body: `会从本机 Nuntius 删除项目登记及 ${project.threadCount} 个会话记录，并在设备重新连上服务器后同步删除。不会删除磁盘上的项目文件或代理原始会话文件。`,
      confirmLabel: "删除项目",
      danger: true,
      action: async () => {
        setDeleting(true);
        try {
          await api.deleteProject(projectId);
          await Promise.all([
            qc.invalidateQueries({ queryKey: ["projects"] }),
            qc.invalidateQueries({ queryKey: ["threads"] }),
            qc.invalidateQueries({ queryKey: ["info"] }),
          ]);
          toast("项目已删除");
          navigate({ name: "projects" }, { replace: true });
        } catch (error) {
          toast(error instanceof Error ? error.message : "删除失败，请重试", { error: true });
        } finally {
          setDeleting(false);
        }
      },
    });
  };

  return (
    <div className="page">
      <TopBar
        title={project?.displayName ?? "项目"}
        subtitle={project?.branch ? `${project.branch}${project.isDirty ? "*" : ""}` : undefined}
        onBack={() => back({ name: "projects" })}
        trailing={
          canCreate ? (
            <button className="icon-btn" onClick={() => setCreating(true)} aria-label="新建会话">
              <IconPlus size={19} />
            </button>
          ) : (
            <ConnIndicator />
          )
        }
      />
      <div className="page-scroll">
        <div className="page-col">
          {!canCreate && !info.isLoading ? (
            <div className="notice-banner warn compact">本机没有可用的编码代理</div>
          ) : null}
          {threads.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 48 }}>
              <Spinner />
            </div>
          ) : sorted.length === 0 ? (
            <Empty
              icon={<IconChat size={24} />}
              headline="还没有会话"
              action={
                canCreate ? (
                  <button className="btn primary" onClick={() => setCreating(true)}>
                    <IconPlus size={15} />
                    新建会话
                  </button>
                ) : undefined
              }
            />
          ) : (
            <div className="list-group">
              {sorted.map((t) => (
                <SwipeActionRow
                  key={t.id}
                  icon={<IconArchive size={18} />}
                  label="归档"
                  busy={busyIds.has(t.id)}
                  disabled={unassigned}
                  onAction={() => archive(t.id)}
                >
                  <ThreadRow
                    thread={t}
                    onRename={() => setRenamingThread(t)}
                    onArchive={unassigned ? undefined : () => archive(t.id)}
                    onClick={() => navigate({ name: "thread", projectId, threadId: t.id })}
                  />
                </SwipeActionRow>
              ))}
            </div>
          )}
          {!unassigned && project ? (
            <>
              <div className="section-label micro project-management-label">项目管理</div>
              <div className="danger-zone project-danger-zone">
                <div className="project-danger-copy">
                  <strong>从 Nuntius 中删除项目</strong>
                  <span>删除项目登记和会话记录，不会动磁盘文件；已配对时会同步到服务器。</span>
                </div>
                <button className="btn danger sm" onClick={remove} disabled={deleting}>
                  {deleting ? <Spinner sm /> : null}
                  删除项目
                </button>
              </div>
            </>
          ) : null}
        </div>
      </div>

      <NewThreadSheet
        projectId={projectId}
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(threadId) => navigate({ name: "thread", projectId, threadId })}
      />
      <RenameThreadSheet
        thread={renamingThread}
        open={renamingThread !== null}
        onClose={() => setRenamingThread(null)}
        onRename={(title) => renamingThread ? renameThread(renamingThread, title) : Promise.resolve()}
      />
      {confirmNode}
    </div>
  );
}
