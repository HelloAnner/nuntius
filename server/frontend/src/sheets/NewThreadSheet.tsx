/* Reusable new-thread flow for project lists and wide conversation layouts. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ProviderPicker,
  Sheet,
  newIdemKey,
  useToast,
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
  deviceId: string;
  projectId: string;
  open: boolean;
  onClose: () => void;
  onCreated: (threadId: string) => void;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const accessMode = useAccessMode((state) => state.mode);
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const [firstMessage, setFirstMessage] = useState("");
  const [provider, setProvider] = useState<AgentProvider>("codex");
  const [busy, setBusy] = useState(false);
  const providerStatuses = devices.data?.find((device) => device.id === deviceId)?.providers ?? [];
  const providerAvailable =
    providerStatuses.find((status) => status.provider === provider)?.available ?? provider === "codex";

  useEffect(() => {
    if (open) setProvider("codex");
  }, [open]);

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
        provider,
        accessMode,
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
      setFirstMessage("");
      onClose();
      void qc.invalidateQueries({ queryKey: ["projectThreads", deviceId, projectId] });
      void qc.invalidateQueries({ queryKey: ["allThreads"] });
      onCreated(created.threadId);
      if (text) {
        const firstTurnKey = newIdemKey();
        liveStore.addOptimistic(created.threadId, firstTurnKey, text);
        void api
          .startTurn(
            created.threadId,
            text,
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
    <Sheet open={open} onClose={onClose} title="新建会话">
      <div style={{ padding: 20, display: "flex", flexDirection: "column", gap: 16 }}>
        <ProviderPicker
          value={provider}
          onChange={setProvider}
          statuses={providerStatuses}
          disabled={busy}
        />
        <div className="field">
          <label htmlFor={`first-msg-${projectId}`}>第一条消息（可选）</label>
          <textarea
            id={`first-msg-${projectId}`}
            rows={4}
            style={{ resize: "vertical", minHeight: 96 }}
            placeholder={`描述一下想让 ${provider === "kimi" ? "Kimi" : "Codex"} 做什么…`}
            value={firstMessage}
            onChange={(event) => setFirstMessage(event.target.value)}
          />
        </div>
        <button className="btn primary block" onClick={create} disabled={busy || !providerAvailable}>
          {busy ? "正在创建…" : "开始对话"}
        </button>
      </div>
    </Sheet>
  );
}
