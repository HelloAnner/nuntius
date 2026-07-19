/* Project page: its threads, newest first, plus new-thread composer entry. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconChat,
  IconPlus,
  Spinner,
  SwipeActionRow,
  compareThreadActivity,
  newIdemKey,
  statusLabel,
  useConfirmAction,
  useToast,
  type ThreadSummary,
} from "@nuntius/shared";
import { api } from "../api";
import { useArchiveThreadAction, useNavigate } from "../hooks";
import { useRoute } from "../stores";
import { trackCommand, waitForCommand } from "../events";
import { ConnIndicator, ThreadRow, TopBar } from "../components";
import { NewThreadSheet } from "../sheets/NewThreadSheet";

export function ProjectPage({ deviceId, projectId }: { deviceId: string; projectId: string }) {
  const navigate = useNavigate();
  const { archive, busyIds } = useArchiveThreadAction();
  const back = useRoute((s) => s.back);
  const toast = useToast();
  const qc = useQueryClient();
  const { confirm, node: confirmNode } = useConfirmAction();
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projects = useQuery({
    queryKey: ["projects", deviceId],
    queryFn: () => api.projects(deviceId),
  });
  const threads = useQuery({
    queryKey: ["projectThreads", deviceId, projectId],
    queryFn: () => api.projectThreads(deviceId, projectId),
  });

  const device = devices.data?.find((d) => d.id === deviceId);
  const project = projects.data?.find((p) => p.id === projectId);

  useEffect(() => {
    const missingDevice = devices.isSuccess && !device;
    const missingProject = projects.isSuccess && !project;
    if (!deleting && (devices.isError || projects.isError || missingDevice || missingProject)) {
      navigate({ name: "devices" }, { replace: true });
    }
  }, [deleting, device, devices.isError, devices.isSuccess, navigate, project, projects.isError, projects.isSuccess]);

  const unassigned = project?.kind === "system_unassigned";
  const canCreate = device?.status === "online" && !unassigned;
  const canDelete = device?.status === "online" && !unassigned;
  const sorted = [...(threads.data ?? [])].sort(compareThreadActivity);

  const remove = () => {
    if (!project || !canDelete || deleting) return;
    confirm({
      title: `删除「${project.displayName}」？`,
      body: `会从 Nuntius 服务器和「${device?.displayName ?? "这台设备"}」删除项目登记及 ${project.threadCount} 个会话记录。不会删除磁盘上的项目文件或代理原始会话文件；以后重新添加此目录可以再次同步历史。`,
      confirmLabel: "删除项目",
      danger: true,
      action: async () => {
        setDeleting(true);
        try {
          const receipt = await api.deleteProject(deviceId, projectId, newIdemKey());
          trackCommand(qc, receipt.commandId, undefined, "project.delete");
          await waitForCommand(receipt.commandId);
          qc.setQueryData<typeof projects.data>(["projects", deviceId], (old) =>
            old?.filter((item) => item.id !== projectId),
          );
          qc.setQueryData<ThreadSummary[]>(["allThreads"], (old) =>
            old?.filter((item) => item.projectId !== projectId),
          );
          await Promise.all([
            qc.invalidateQueries({ queryKey: ["projects", deviceId] }),
            qc.invalidateQueries({ queryKey: ["allThreads"] }),
            qc.invalidateQueries({ queryKey: ["devices"] }),
          ]);
          toast("项目已从服务器和设备删除");
          navigate({ name: "device", deviceId }, { replace: true });
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
        subtitle={device
          ? `${device.displayName}${device.status === "online" ? "" : ` · ${statusLabel(device.status)}`}${project?.branch ? ` · ${project.branch}${project.isDirty ? "*" : ""}` : ""}`
          : undefined}
        onBack={() => back({ name: "device", deviceId })}
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
          {unassigned ? (
            <div className="notice-banner info compact">未归类 · 仅可查看历史</div>
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
                  disabled={!canDelete}
                  onAction={() => archive(t.id)}
                >
                  <ThreadRow
                    thread={t}
                    deviceName={device?.displayName ?? "设备"}
                    projectName={project?.displayName ?? "项目"}
                    onClick={() =>
                      navigate({ name: "thread", deviceId, projectId, threadId: t.id })
                    }
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
                  <span>同步删除服务器与设备上的项目登记和会话记录，不会动磁盘文件。</span>
                </div>
                <button className="btn danger sm" onClick={remove} disabled={!canDelete || deleting}>
                  {deleting ? <Spinner sm /> : null}
                  {device?.status === "online" ? "删除项目" : "设备离线"}
                </button>
              </div>
            </>
          ) : null}
        </div>
      </div>

      <NewThreadSheet
        deviceId={deviceId}
        projectId={projectId}
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(threadId) => navigate({ name: "thread", deviceId, projectId, threadId })}
      />
      {confirmNode}
    </div>
  );
}
