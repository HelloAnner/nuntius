import type { AgentProvider, AgentProviderStatus } from "../types";

const PROVIDERS: Array<{
  id: AgentProvider;
  label: string;
  mark: string;
  description: string;
}> = [
  { id: "codex", label: "Codex", mark: "C", description: "OpenAI 编码代理" },
  { id: "kimi", label: "Kimi", mark: "K", description: "Kimi Code CLI" },
  { id: "pi", label: "Pi", mark: "P", description: "Pi 编码代理 (RPC)" },
];

export function ProviderPicker({
  value,
  onChange,
  statuses,
  disabled = false,
}: {
  value: AgentProvider;
  onChange: (provider: AgentProvider) => void;
  statuses?: AgentProviderStatus[];
  disabled?: boolean;
}) {
  return (
    <fieldset className="provider-picker" disabled={disabled}>
      <legend>执行引擎</legend>
      <div className="provider-picker-grid">
        {PROVIDERS.map((provider) => {
          const status = statuses?.find((candidate) => candidate.provider === provider.id);
          const available = status?.available ?? provider.id === "codex";
          const selected = value === provider.id;
          return (
            <label
              className={`provider-option${selected ? " selected" : ""}${available ? "" : " unavailable"}`}
              key={provider.id}
            >
              <input
                type="radio"
                name="agent-provider"
                value={provider.id}
                checked={selected}
                disabled={disabled || !available}
                onChange={() => onChange(provider.id)}
              />
              <span className="provider-mark" aria-hidden="true">{provider.mark}</span>
              <span className="provider-copy">
                <strong>{provider.label}</strong>
                <small>{available ? provider.description : "本机未安装"}</small>
              </span>
              <span className={`provider-state ${status?.status ?? "unknown"}`}>
                {status?.status === "online" ? "已连接" : available ? "可启动" : "不可用"}
              </span>
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}
