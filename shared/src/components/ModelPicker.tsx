import { useId } from "react";
import { modelsForProvider } from "../agent-config";
import type { AgentProvider, AgentProviderStatus } from "../types";

const EFFORT_LABELS: Record<string, string> = {
  none: "关闭",
  on: "开启",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "超高",
  max: "极致",
  ultra: "Ultra",
};

export function ModelPicker({
  provider,
  status,
  model,
  reasoningEffort,
  onChange,
  disabled = false,
}: {
  provider: AgentProvider;
  status?: AgentProviderStatus;
  model: string;
  reasoningEffort: string;
  onChange: (model: string, reasoningEffort: string) => void;
  disabled?: boolean;
}) {
  const controlId = useId();
  const models = modelsForProvider(provider, status);
  const selected = models.find((candidate) => candidate.id === model) ?? models[0];
  const efforts = selected?.reasoningEfforts ?? [];
  const selectedEffort = efforts.includes(reasoningEffort)
    ? reasoningEffort
    : selected?.defaultReasoningEffort ?? efforts[0] ?? "";

  return (
    <section className="agent-config-panel" aria-label="模型配置">
      <div className="agent-config-heading">
        <span>模型配置</span>
        <small>{provider === "kimi" ? "Kimi Code" : "OpenAI Codex"}</small>
      </div>
      <label className="agent-model-field" htmlFor={`${controlId}-model`}>
        <span>模型</span>
        <select
          id={`${controlId}-model`}
          value={selected?.id ?? ""}
          disabled={disabled || models.length === 0}
          onChange={(event) => {
            const next = models.find((candidate) => candidate.id === event.target.value);
            if (!next) return;
            onChange(
              next.id,
              next.defaultReasoningEffort ?? next.reasoningEfforts[0] ?? "",
            );
          }}
        >
          {models.map((candidate) => (
            <option key={candidate.id} value={candidate.id}>
              {candidate.label}{candidate.isDefault ? " · 默认" : ""}
            </option>
          ))}
        </select>
      </label>
      {selected?.description ? (
        <p className="agent-model-description">{selected.description}</p>
      ) : null}
      {efforts.length ? (
        <div className={`effort-picker${disabled ? " disabled" : ""}`}>
          <span className="effort-picker-label">思考强度</span>
          <div
            className="effort-picker-options"
            role="radiogroup"
            aria-label="思考强度"
          >
            {efforts.map((effort) => (
              <label
                className={selectedEffort === effort ? "selected" : ""}
                key={effort}
              >
                <input
                  type="radio"
                  name={`${controlId}-effort`}
                  value={effort}
                  checked={selectedEffort === effort}
                  disabled={disabled}
                  onChange={() => onChange(selected?.id ?? "", effort)}
                />
                <span>{EFFORT_LABELS[effort] ?? effort}</span>
              </label>
            ))}
          </div>
        </div>
      ) : null}
    </section>
  );
}
