/* Reusable new-thread flow for project lists and wide conversation layouts. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ModelPicker,
  ProviderPicker,
  SelectMenu,
  Sheet,
  agentThreadOptions,
  defaultAgentSelection,
  newIdemKey,
  providerLabel,
  useToast,
  type AgentSelection,
  type AgentProvider,
  type ThreadSummary,
} from "@nuntius/shared";
import { api, ApiError } from "../api";
import { trackCommand } from "../events";
import { liveStore, useAccessMode } from "../stores";

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
      if (TERMINAL_COMMANDS.has(command.status)) throw new Error("创建失败，请重试");
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

export function NewThreadSheet({
  deviceId,
  projectId,
  open,
  onClose,
  onCreated,
}: {
  deviceId?: string;
  projectId?: string;
  open: boolean;
  onClose: () => void;
  onCreated: (threadId: string, deviceId: string, projectId: string) => void;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const accessMode = useAccessMode((state) => state.mode);
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const [scopeDeviceId, setScopeDeviceId] = useState(deviceId ?? "");
  const [scopeProjectId, setScopeProjectId] = useState(projectId ?? "");
  const effectiveDeviceId = deviceId ?? scopeDeviceId;
  const effectiveProjectId = projectId ?? scopeProjectId;
  const fixedScope = Boolean(deviceId && projectId);
  const projects = useQuery({
    queryKey: ["projects", effectiveDeviceId],
    queryFn: () => api.projects(effectiveDeviceId),
    enabled: open && Boolean(effectiveDeviceId),
  });
  const [firstMessage, setFirstMessage] = useState("");
  const [provider, setProvider] = useState<AgentProvider>("codex");
  const [selection, setSelection] = useState<AgentSelection>(() =>
    defaultAgentSelection("codex"),
  );
  const [busy, setBusy] = useState(false);
  const providerStatuses = devices.data?.find((device) => device.id === effectiveDeviceId)?.providers ?? [];
  const scopeDevice = devices.data?.find((device) => device.id === effectiveDeviceId);
  const scopeProject = projects.data?.find((project) => project.id === effectiveProjectId);
  const selectedProviderStatus = providerStatuses.find(
    (status) => status.provider === provider,
  );
  const providerAvailable =
    selectedProviderStatus?.available ?? provider === "codex";

  useEffect(() => {
    if (!open) return;
    if (deviceId) setScopeDeviceId(deviceId);
    else {
      const current = devices.data?.find((device) => device.id === scopeDeviceId && device.status === "online");
      if (!current) setScopeDeviceId(devices.data?.find((device) => device.status === "online")?.id ?? "");
    }
  }, [deviceId, devices.data, open, scopeDeviceId]);

  useEffect(() => {
    if (!open) return;
    if (projectId) setScopeProjectId(projectId);
    else {
      const available = (projects.data ?? []).filter((project) => project.kind === "workspace");
      if (!available.some((project) => project.id === scopeProjectId)) setScopeProjectId(available[0]?.id ?? "");
    }
  }, [open, projectId, projects.data, scopeProjectId]);

  useEffect(() => {
    if (!open) return;
    setProvider("codex");
    setSelection(
      defaultAgentSelection(
        "codex",
        providerStatuses.find((status) => status.provider === "codex"),
      ),
    );
  }, [effectiveDeviceId, open]);

  useEffect(() => {
    if (!open) return;
    const models = selectedProviderStatus?.models ?? [];
    if (
      !selection.model ||
      (models.length > 0 && !models.some((model) => model.id === selection.model))
    ) {
      setSelection(defaultAgentSelection(provider, selectedProviderStatus));
    }
  }, [open, provider, selectedProviderStatus, selection.model]);

  const create = async () => {
    const text = firstMessage.trim();
    if (busy || !effectiveDeviceId || !effectiveProjectId) return;
    setBusy(true);
    const idemKey = newIdemKey();
    try {
      const receipt = await api.createThread(
        effectiveDeviceId,
        effectiveProjectId,
        text ? Array.from(text).slice(0, 48).join("") : null,
        null,
        provider,
        accessMode,
        agentThreadOptions(provider, selection),
        idemKey,
      );
      trackCommand(qc, receipt.commandId, undefined, "thread.create");
      const created = await waitForCreatedThread(receipt.commandId);
      if (created.thread) {
        const upsert = (items: ThreadSummary[] | undefined) => [
          created.thread!,
          ...(items ?? []).filter((item) => item.id !== created.threadId),
        ];
        qc.setQueryData<ThreadSummary[]>(["projectThreads", effectiveDeviceId, effectiveProjectId], upsert);
        qc.setQueryData<ThreadSummary[]>(["allThreads"], upsert);
      }
      setFirstMessage("");
      onClose();
      void qc.invalidateQueries({ queryKey: ["projectThreads", effectiveDeviceId, effectiveProjectId] });
      void qc.invalidateQueries({ queryKey: ["allThreads"] });
      onCreated(created.threadId, effectiveDeviceId, effectiveProjectId);
      if (text) {
        const firstTurnKey = newIdemKey();
        liveStore.addOptimistic(created.threadId, firstTurnKey, text, [], firstTurnKey);
        void api
          .startTurn(
            created.threadId,
            text,
            [],
            firstTurnKey,
            accessMode,
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
    } catch (error) {
      toast(
        error instanceof ApiError && error.code === "device_offline"
          ? "设备离线，无法创建会话"
          : error instanceof Error
            ? error.message
            : "创建失败，请重试",
        { error: true },
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <Sheet
      open={open}
      onClose={onClose}
      className="new-thread-sheet"
      title={
        <span className="new-thread-title">
          <strong>新建会话</strong>
          {scopeDevice && scopeProject ? <small>{scopeDevice.displayName} · {scopeProject.displayName}</small> : null}
        </span>
      }
    >
      <div className="new-thread-form">
        {!fixedScope ? (
          <div className="new-thread-scope">
            <div className="field">
              <label>设备</label>
              <SelectMenu
                className="field-select"
                label="设备"
                value={effectiveDeviceId}
                onChange={(value) => {
                  setScopeDeviceId(value);
                  setScopeProjectId("");
                }}
                disabled={busy}
                options={(devices.data ?? []).filter((device) => device.status === "online").map((device) => ({
                  value: device.id,
                  label: device.displayName,
                  description: `${device.projectCount} 个项目 · 在线`,
                }))}
              />
            </div>
            <div className="field">
              <label>项目</label>
              <SelectMenu
                className="field-select"
                label="项目"
                value={effectiveProjectId}
                onChange={setScopeProjectId}
                disabled={busy || !effectiveDeviceId}
                options={(projects.data ?? []).filter((project) => project.kind === "workspace").map((project) => ({
                  value: project.id,
                  label: project.displayName,
                  description: project.pathHint ?? project.repoName ?? undefined,
                }))}
              />
            </div>
          </div>
        ) : null}
        <ProviderPicker
          value={provider}
          onChange={(nextProvider) => {
            setProvider(nextProvider);
            setSelection(
              defaultAgentSelection(
                nextProvider,
                providerStatuses.find((status) => status.provider === nextProvider),
              ),
            );
          }}
          statuses={providerStatuses}
          disabled={busy}
        />
        <ModelPicker
          provider={provider}
          status={selectedProviderStatus}
          model={selection.model}
          reasoningEffort={selection.reasoningEffort}
          onChange={(model, reasoningEffort) => setSelection({ model, reasoningEffort })}
          disabled={busy}
        />
        <div className="field">
          <label htmlFor={`first-msg-${effectiveProjectId || "new"}`}>第一条消息（可选）</label>
          <textarea
            id={`first-msg-${effectiveProjectId || "new"}`}
            rows={4}
            placeholder={`描述一下想让 ${providerLabel(provider)} 做什么…`}
            value={firstMessage}
            onChange={(event) => setFirstMessage(event.target.value)}
          />
        </div>
        <div className="new-thread-actions">
          <button className="btn outline" onClick={onClose} disabled={busy}>取消</button>
          <button className="btn primary" onClick={create} disabled={busy || !providerAvailable || !effectiveDeviceId || !effectiveProjectId}>
          {busy ? "正在创建…" : "开始对话"}
          </button>
        </div>
      </div>
    </Sheet>
  );
}
