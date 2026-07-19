/* Project page: its threads, newest first, plus new-thread composer entry. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconChat,
  IconPlus,
  Sheet,
  Spinner,
  SwipeActionRow,
  newIdemKey,
  statusLabel,
  threadOptionsForAccess,
  turnOptionsForAccess,
  useConfirmAction,
  useToast,
  type ThreadSummary,
} from "@nuntius/shared";
import { api, ApiError } from "../api";
import { useArchiveThreadAction, useNavigate } from "../hooks";
import { liveStore, useAccessMode, useRoute } from "../stores";
import { trackCommand, waitForCommand } from "../events";
import { ConnIndicator, ThreadRow, TopBar } from "../components";

const TERMINAL_COMMANDS = new Set(["completed", "failed", "rejected", "unknown", "expired"]);

interface CreatedThread {
  threadId: string;
  thread: ThreadSummary | null;
}

function createdThreadFromResult(result: unknown): CreatedThread | null {
  if (!result || typeof result !== "object" || !("threadId" in result)) return null;
  const value = result as { threadId?: unknown; thread?: unknown };
  if (typeof value.threadId !== "string" || value.threadId.length === 0) return null;
  const candidate = value.thread;
  const thread =
    candidate &&
    typeof candidate === "object" &&
    "id" in candidate &&
    (candidate as { id?: unknown }).id === value.threadId
      ? (candidate as ThreadSummary)
      : null;
  return { threadId: value.threadId, thread };
}

async function waitForCreatedThread(commandId: string): Promise<CreatedThread> {
  const deadline = Date.now() + 90_000;
  let delay = 180;
  while (Date.now() < deadline) {
    try {
      const command = await api.command(commandId);
      if (command.status === "completed") {
        const created = createdThreadFromResult(command.result);
        if (created) return created;
        throw new Error("会话已创建，但没有返回会话编号");
      }
      if (TERMINAL_COMMANDS.has(command.status)) {
        throw new Error("创建失败，请重试");
      }
    } catch (error) {
      if (!(error instanceof ApiError && (error.retryable || error.code === "not_found"))) {
        throw error;
      }
    }
    await new Promise((resolve) => window.setTimeout(resolve, delay));
    delay = Math.min(1_000, Math.round(delay * 1.5));
  }
  throw new Error("创建超时，请重试");
}

export function ProjectPage({ deviceId, projectId }: { deviceId: string; projectId: string }) {
  const navigate = useNavigate();
  const { archive, busyIds } = useArchiveThreadAction();
  const back = useRoute((s) => s.back);
  const toast = useToast();
  const qc = useQueryClient();
  const accessMode = useAccessMode((state) => state.mode);
  const { confirm, node: confirmNode } = useConfirmAction();
  const [creating, setCreating] = useState(false);
  const [firstMessage, setFirstMessage] = useState("");
  const [busy, setBusy] = useState(false);
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
  const sorted = [...(threads.data ?? [])].sort(
    (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
  );

  const create = async () => {
    const text = firstMessage.trim();
    if (busy) return;
    setBusy(true);
    const idemKey = newIdemKey();
    try {
      const receipt = await api.createThread(
        deviceId,
        projectId,
        text ? Array.from(text).slice(0, 48).join("") : null,
        null,
        threadOptionsForAccess(accessMode),
        idemKey,
      );
      trackCommand(qc, receipt.commandId, undefined, "thread.create");
      const created = await waitForCreatedThread(receipt.commandId);
      if (created.thread) {
        const upsert = (items: ThreadSummary[] | undefined) => [
          created.thread!,
          ...(items ?? []).filter((item) => item.id !== created.threadId),
        ];
        qc.setQueryData<ThreadSummary[]>(["projectThreads", deviceId, projectId], upsert);
        qc.setQueryData<ThreadSummary[]>(["allThreads"], upsert);
      }
      setCreating(false);
      setFirstMessage("");
      void qc.invalidateQueries({ queryKey: ["projectThreads", deviceId, projectId] });
      void qc.invalidateQueries({ queryKey: ["allThreads"] });
      navigate({ name: "thread", deviceId, projectId, threadId: created.threadId });
      if (text) {
        const firstTurnKey = newIdemKey();
        liveStore.addOptimistic(created.threadId, firstTurnKey, text);
        void api
          .startTurn(
            created.threadId,
            text,
            turnOptionsForAccess(accessMode),
            firstTurnKey,
          )
          .then((turnReceipt) => {
            liveStore.bindCommand(firstTurnKey, turnReceipt.commandId);
            liveStore.applyCommandStatus(turnReceipt.commandId, turnReceipt.status);
            trackCommand(qc, turnReceipt.commandId, created.threadId, "thread.input");
          })
          .catch((error) => {
            liveStore.applyCommandStatus(
              firstTurnKey,
              "failed",
              error instanceof ApiError ? error.code : "request_failed",
              error instanceof Error ? error.message : "发送失败",
            );
          });
      }
    } catch (e) {
      toast(
        e instanceof ApiError && e.code === "device_offline"
          ? "设备离线，无法创建会话"
          : e instanceof Error
            ? e.message
            : "创建失败，请重试",
        { error: true },
      );
    } finally {
      setBusy(false);
    }
  };

  const remove = () => {
    if (!project || !canDelete || deleting) return;
    confirm({
      title: `删除「${project.displayName}」？`,
      body: `会从 Nuntius 服务器和「${device?.displayName ?? "这台设备"}」删除项目登记及 ${project.threadCount} 个会话记录。不会删除磁盘上的项目文件或 Codex 原始会话文件；以后重新添加此目录可以再次同步历史。`,
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

      <Sheet open={creating} onClose={() => setCreating(false)} title="新建会话">
        <div style={{ padding: 20, display: "flex", flexDirection: "column", gap: 16 }}>
          <div className="field">
            <label htmlFor="first-msg">第一条消息（可选）</label>
            <textarea
              id="first-msg"
              rows={4}
              style={{ resize: "vertical", minHeight: 96 }}
              placeholder="描述一下想让 Codex 做什么…"
              value={firstMessage}
              onChange={(e) => setFirstMessage(e.target.value)}
            />
          </div>
          <button className="btn primary block" onClick={create} disabled={busy}>
            开始对话
          </button>
        </div>
      </Sheet>
      {confirmNode}
    </div>
  );
}
