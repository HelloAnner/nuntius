/* Reusable local new-thread flow for project and conversation pages. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ModelPicker,
  ProviderPicker,
  Sheet,
  agentThreadOptions,
  defaultAgentSelection,
  newIdemKey,
  useToast,
  type AgentProvider,
  type AgentSelection,
} from "@nuntius/shared";
import { api } from "../api";
import { liveStore } from "../stores";

export function NewThreadSheet({
  projectId,
  open,
  onClose,
  onCreated,
}: {
  projectId: string;
  open: boolean;
  onClose: () => void;
  onCreated: (threadId: string) => void;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const info = useQuery({ queryKey: ["info"], queryFn: api.info });
  const [firstMessage, setFirstMessage] = useState("");
  const [provider, setProvider] = useState<AgentProvider>("codex");
  const [selection, setSelection] = useState<AgentSelection>(() =>
    defaultAgentSelection("codex"),
  );
  const [busy, setBusy] = useState(false);
  const providerStatuses = info.data?.providers ?? [];
  const selectedProviderStatus = providerStatuses.find(
    (status) => status.provider === provider,
  );
  const providerAvailable = selectedProviderStatus?.available ?? provider === "codex";

  useEffect(() => {
    if (!open) return;
    setProvider("codex");
    setSelection(
      defaultAgentSelection(
        "codex",
        providerStatuses.find((status) => status.provider === "codex"),
      ),
    );
  }, [open]);

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
    if (busy) return;
    setBusy(true);
    try {
      const result = await api.createThread(
        projectId,
        text ? Array.from(text).slice(0, 48).join("") : null,
        provider,
        agentThreadOptions(provider, selection),
      );
      setFirstMessage("");
      onClose();
      void qc.invalidateQueries({ queryKey: ["projectThreads", projectId] });
      void qc.invalidateQueries({ queryKey: ["threads"] });
      onCreated(result.threadId);
      if (text) {
        const optimisticKey = `initial:${newIdemKey()}`;
        const clientMessageId = newIdemKey();
        liveStore.addOptimistic(result.threadId, optimisticKey, text, [], clientMessageId);
        void api
          .startTurn(result.threadId, text, clientMessageId)
          .then(() => {
            liveStore.applyCommandStatus(optimisticKey, "completed");
            void qc.invalidateQueries({ queryKey: ["projectThreads", projectId] });
          })
          .catch((error) => {
            liveStore.applyCommandStatus(
              optimisticKey,
              "failed",
              "request_failed",
              error instanceof Error ? error.message : "发送失败",
            );
          });
      }
    } catch (error) {
      toast(error instanceof Error ? error.message : "创建失败", { error: true });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Sheet open={open} onClose={onClose} title="新建会话">
      <div style={{ padding: 20, display: "flex", flexDirection: "column", gap: 16 }}>
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
