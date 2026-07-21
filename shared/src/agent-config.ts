import type { AgentModelOption, AgentProvider, AgentProviderStatus } from "./types";

const FALLBACK_MODELS: Record<AgentProvider, AgentModelOption[]> = {
  codex: [
    {
      id: "gpt-5.6-sol",
      label: "GPT-5.6 Sol",
      description: "OpenAI 当前旗舰编码与复杂推理模型",
      isDefault: true,
      defaultReasoningEffort: "xhigh",
      reasoningEfforts: ["low", "medium", "high", "xhigh", "max"],
    },
  ],
  kimi: [
    {
      id: "kimi-code/k3",
      label: "K3",
      description: "Kimi 旗舰编程模型 · 最高 1M 上下文",
      isDefault: true,
      defaultReasoningEffort: "max",
      reasoningEfforts: ["low", "high", "max"],
    },
    {
      id: "kimi-code/kimi-for-coding",
      label: "K2.7 Coding",
      description: "K2.7 Code · 稳定编程模型",
      isDefault: false,
      defaultReasoningEffort: "on",
      reasoningEfforts: ["on"],
    },
    {
      id: "kimi-code/kimi-for-coding-highspeed",
      label: "K2.7 Coding Highspeed",
      description: "K2.7 Code · 高速输出",
      isDefault: false,
      defaultReasoningEffort: "on",
      reasoningEfforts: ["on"],
    },
  ],
  pi: [
    {
      id: "anthropic/claude-opus-4-5",
      label: "Claude Opus 4.5",
      description: "Pi 默认 Anthropic 模型 · 深度推理",
      isDefault: true,
      defaultReasoningEffort: "medium",
      reasoningEfforts: ["off", "low", "medium", "high"],
    },
    {
      id: "anthropic/claude-sonnet-4-20250514",
      label: "Claude Sonnet 4",
      description: "均衡编码模型",
      isDefault: false,
      defaultReasoningEffort: "medium",
      reasoningEfforts: ["off", "low", "medium", "high"],
    },
  ],
};

export interface AgentSelection {
  model: string;
  reasoningEffort: string;
}

export function modelsForProvider(
  provider: AgentProvider,
  status?: AgentProviderStatus,
): AgentModelOption[] {
  return status?.models?.length ? status.models : FALLBACK_MODELS[provider];
}

export function defaultAgentSelection(
  provider: AgentProvider,
  status?: AgentProviderStatus,
): AgentSelection {
  const models = modelsForProvider(provider, status);
  const model = models.find((candidate) => candidate.isDefault) ?? models[0];
  return {
    model: model?.id ?? "",
    reasoningEffort:
      model?.defaultReasoningEffort ?? model?.reasoningEfforts[0] ?? "",
  };
}

export function agentThreadOptions(
  provider: AgentProvider,
  selection: AgentSelection,
): Record<string, unknown> {
  if (!selection.model) return {};
  if (provider === "kimi") {
    return {
      model: selection.model,
      thinking: selection.reasoningEffort || "max",
    };
  }
  if (provider === "pi") {
    return {
      model: selection.model,
      thinking: selection.reasoningEffort || "medium",
    };
  }
  return {
    model: selection.model,
    ...(selection.reasoningEffort ? { reasoningEffort: selection.reasoningEffort } : {}),
  };
}
