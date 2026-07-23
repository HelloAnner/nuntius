/* Conversation item renderers: user bubble, agent message, work items,
 * approval card. Used identically by the remote and local consoles. */
import { memo, useEffect, useState, type ReactNode } from "react";
import type { LiveItem, LiveStatus } from "../stream";
import type { AttachmentView } from "../types";
import { Markdown } from "./Markdown";
import {
  IconAlert,
  IconBook,
  IconCheck,
  IconChevronRight,
  IconFile,
  IconShield,
  IconSparkle,
  IconTerminal,
  IconThought,
  IconTool,
  IconX,
} from "./icons";
import { Spinner } from "./ui";

/* ---------- user ---------- */
export const UserBubble = memo(function UserBubble({
  text,
  attachments = [],
  state,
  stateLabel,
  stateError,
  errorMessage,
  onRetry,
}: {
  text: string;
  attachments?: AttachmentView[];
  state?: string | null;
  stateLabel?: string | null;
  stateError?: boolean;
  errorMessage?: string | null;
  onRetry?: () => void;
}) {
  const pending = state === "applying" || state === "accepted" || state === "waiting_device";
  return (
    <div className="msg-user">
      <div className={`bubble${attachments.length ? " has-attachments" : ""}`}>
        {attachments.length ? <AttachmentGallery attachments={attachments} /> : null}
        {text ? <div className="bubble-text">{text}</div> : null}
      </div>
      {stateLabel ? (
        <div
          className={`send-state${stateError ? " err" : ""}`}
          role="status"
          aria-label={errorMessage ?? stateLabel}
        >
          {pending ? <Spinner sm /> : <span className="send-state-label">{errorMessage ?? stateLabel}</span>}
          {stateError && onRetry ? (
            <button className="send-retry" onClick={onRetry}>重试</button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
});

function AttachmentGallery({ attachments }: { attachments: AttachmentView[] }) {
  const [active, setActive] = useState<AttachmentView | null>(null);
  useEffect(() => {
    if (!active) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") setActive(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [active]);
  return (
    <>
      <div className={`message-images count-${Math.min(attachments.length, 4)}`}>
        {attachments.map((attachment) => (
          <button
            key={attachment.id}
            className="message-image"
            onClick={() => setActive(attachment)}
            aria-label={`查看图片 ${attachment.originalName}`}
          >
            <img
              src={`/api/v1/attachments/${encodeURIComponent(attachment.id)}/thumbnail`}
              alt={attachment.originalName}
              loading="lazy"
            />
            <span>{attachment.width} × {attachment.height}</span>
          </button>
        ))}
      </div>
      {active ? (
        <div
          className="image-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label={active.originalName}
          onClick={() => setActive(null)}
        >
          <button className="image-lightbox-close" onClick={() => setActive(null)} aria-label="关闭图片">
            <IconX size={20} />
          </button>
          <img
            src={`/api/v1/attachments/${encodeURIComponent(active.id)}/content`}
            alt={active.originalName}
            onClick={(event) => event.stopPropagation()}
          />
          <div className="image-lightbox-meta" onClick={(event) => event.stopPropagation()}>
            <span>{active.originalName}</span>
            <span>{active.width} × {active.height}</span>
          </div>
        </div>
      ) : null}
    </>
  );
}

/* ---------- agent ---------- */
export const AgentMessage = memo(function AgentMessage({
  text,
  streaming,
  saveState = "idle",
  onSave,
}: {
  text: string;
  streaming?: boolean;
  saveState?: "idle" | "saving" | "saved";
  onSave?: () => void;
}) {
  return (
    <div
      className={`msg-agent${streaming ? " streaming" : ""}`}
      aria-busy={streaming || undefined}
    >
      <span className="mark">
        <IconSparkle size={13} />
      </span>
      <div className="body">
        {text ? (
          <Markdown text={text} />
        ) : streaming ? (
          <span className="thinking-indicator" role="status" aria-label="正在思考">
            <span aria-hidden="true" />
          </span>
        ) : null}
        {onSave ? (
          <div className="message-actions">
            <button
              className={`message-save ${saveState}`}
              type="button"
              onClick={onSave}
              disabled={saveState !== "idle"}
              aria-label={saveState === "saved" ? "这条回答已保存" : "保存这条回答"}
            >
              {saveState === "saving" ? (
                <Spinner sm />
              ) : saveState === "saved" ? (
                <IconCheck size={14} />
              ) : (
                <IconBook size={14} />
              )}
              {saveState === "saving" ? "保存中" : saveState === "saved" ? "已保存" : "保存"}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
});

/* ---------- work items ---------- */
const KIND_META: Record<
  string,
  { icon: (size: number) => ReactNode; label: string }
> = {
  command: { icon: (s) => <IconTerminal size={s} />, label: "命令" },
  tool: { icon: (s) => <IconTool size={s} />, label: "工具" },
  file: { icon: (s) => <IconFile size={s} />, label: "文件变更" },
  reasoning: { icon: (s) => <IconThought size={s} />, label: "思考过程" },
  plan: { icon: (s) => <IconThought size={s} />, label: "计划" },
  other: { icon: (s) => <IconTool size={s} />, label: "事件" },
};

function statusDot(status: LiveStatus) {
  return <span className={`wi-status-dot ${status}`} />;
}

export const WorkItemView = memo(function WorkItemView({
  item,
}: {
  item: LiveItem;
}) {
  const meta = KIND_META[item.kind] ?? KIND_META.other;
  const hasBody = Boolean(item.text) || item.files.length > 0;
  const [open, setOpen] = useState(item.status === "running" && item.kind !== "reasoning");
  const title =
    item.title ||
    (item.kind === "reasoning" ? meta.label : item.text.split("\n", 1)[0].slice(0, 80)) ||
    meta.label;

  return (
    <div className={`work-item ${item.kind}${open ? " open" : ""}`}>
      <button
        className="wi-head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="wi-icon">{meta.icon(15)}</span>
        <span className="wi-title">{title}</span>
        <span className="wi-state">
          {item.status === "running" ? <Spinner sm /> : statusDot(item.status)}
          {hasBody ? <IconChevronRight size={14} className="chev" /> : null}
        </span>
      </button>
      {open && item.text ? (
        <div className="wi-body">
          <pre>{item.text}</pre>
        </div>
      ) : null}
      {open && item.files.length > 0 ? (
        <div className="file-chips">
          {item.files.map((f, i) => (
            <span key={`${f.path}-${i}`} className="file-chip">
              <span className={`fc-kind ${f.kind}`}>
                {f.kind === "add" ? "+" : f.kind === "del" ? "−" : "~"}
              </span>
              <span>{f.path}</span>
            </span>
          ))}
        </div>
      ) : null}
    </div>
  );
});

/* ---------- approval ---------- */
export type ApprovalState = "pending" | "responding" | "approved" | "denied" | "expired" | "cancelled" | "unknown";

export interface ApprovalView {
  id: string;
  method: string;
  params: unknown;
  state: ApprovalState;
  decidedAs?: string;
  occurredAt: string;
  threadId?: string | null;
  deviceId: string;
  deviceName?: string;
  projectName?: string;
  threadTitle?: string;
}

function approvalSummary(method: string, params: unknown): { kind: string; detail: string } {
  const p = (params ?? {}) as Record<string, unknown>;
  const pick = (...keys: string[]): string | null => {
    for (const k of keys) {
      const v = p[k];
      if (typeof v === "string" && v) return v;
      if (Array.isArray(v) && v.length) return v.map(String).join(" ");
    }
    return null;
  };
  // detail JSON: drop transport noise (ids, nulls, timestamps)
  const clean = (() => {
    const HIDDEN = new Set(["threadId", "turnId", "itemId", "startedAtMs", "grantRoot", "id"]);
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(p)) {
      if (HIDDEN.has(k) || v === null || v === undefined) continue;
      out[k] = v;
    }
    const text = JSON.stringify(out, null, 2);
    return text === "{}" ? "" : text;
  })();
  const m = method.toLowerCase();
  const cmd = pick("command", "cmd") ??
    (p.command && typeof p.command === "object"
      ? JSON.stringify(p.command)
      : null);
  if (cmd || m.includes("exec") || m.includes("command")) {
    return { kind: "命令执行", detail: cmd ?? clean };
  }
  if (m.includes("patch") || m.includes("file") || m.includes("apply")) {
    return { kind: "文件修改", detail: pick("reason", "path") ?? clean };
  }
  if (m.includes("mcp") || m.includes("tool")) {
    return { kind: "工具调用", detail: pick("tool", "name", "reason") ?? clean };
  }
  return { kind: "操作审批", detail: pick("reason", "message") ?? clean };
}

const DECISION_LABEL: Record<string, string> = {
  accept: "已批准",
  accept_for_session: "本会话内批准",
  decline: "已拒绝",
  cancel: "已取消",
};

export function ApprovalCard({
  approval,
  onDecide,
  locked,
}: {
  approval: ApprovalView;
  onDecide: (decision: string, response?: unknown) => void;
  locked?: boolean;
}) {
  const { kind, detail } = approvalSummary(approval.method, approval.params);
  const pending = approval.state === "pending";
  const responding = approval.state === "responding";
  const isKimiQuestion = approval.method === "kimi/question";
  const supportsSessionDecision = approval.method !== "pi/extension_ui";
  const piMethod = approval.method === "pi/extension_ui"
    ? ((approval.params ?? {}) as Record<string, unknown>).method
    : null;
  const isPiInteractive = piMethod === "select" || piMethod === "input" || piMethod === "editor";
  return (
    <div className={`approval-card${pending || responding ? "" : " decided"}`}>
      <div className="ap-head">
        <span className="ap-icon">
          {pending || responding ? <IconShield size={17} /> : <IconAlert size={17} />}
        </span>
        {pending || responding ? `${kind}审批` : DECISION_LABEL[approval.decidedAs ?? ""] ?? "审批已结束"}
      </div>
      <div className="ap-context">
        {approval.deviceName ? <span>设备 {approval.deviceName}</span> : null}
        {approval.projectName ? <span>项目 {approval.projectName}</span> : null}
        <span className="mono" style={{ fontSize: 11 }}>{approval.method}</span>
      </div>
      {!isKimiQuestion && !isPiInteractive && detail && detail !== "null" ? (
        <div className="ap-detail">
          <pre>{detail.length > 4000 ? `${detail.slice(0, 4000)}\n…（内容已截断）` : detail}</pre>
        </div>
      ) : null}
      {(pending || responding) && isKimiQuestion ? (
        <KimiQuestionForm
          params={approval.params}
          disabled={responding || locked}
          responding={responding}
          onDecide={onDecide}
        />
      ) : (pending || responding) && isPiInteractive ? (
        <PiExtensionForm
          params={approval.params}
          disabled={responding || locked}
          responding={responding}
          onDecide={onDecide}
        />
      ) : pending || responding ? (
        <div className="ap-actions">
          <button
            className="btn primary sm"
            disabled={responding || locked}
            onClick={() => onDecide("accept")}
          >
            {responding ? <Spinner sm /> : null}
            批准
          </button>
          {supportsSessionDecision ? (
            <button
              className="btn ghost sm"
              disabled={responding || locked}
              onClick={() => onDecide("accept_for_session")}
            >
              本会话都允许
            </button>
          ) : null}
          <button
            className="btn danger sm"
            disabled={responding || locked}
            onClick={() => onDecide("decline")}
          >
            拒绝
          </button>
        </div>
      ) : null}
    </div>
  );
}

function PiExtensionForm({
  params,
  disabled,
  responding,
  onDecide,
}: {
  params: unknown;
  disabled?: boolean;
  responding: boolean;
  onDecide: (decision: string, response?: unknown) => void;
}) {
  const raw = (params ?? {}) as Record<string, unknown>;
  const method = typeof raw.method === "string" ? raw.method : "input";
  const options = (Array.isArray(raw.options) ? raw.options : []).map((option) => {
    if (typeof option === "string") return { label: option, value: option };
    const value = (option ?? {}) as Record<string, unknown>;
    const label = typeof value.label === "string"
      ? value.label
      : typeof value.value === "string"
        ? value.value
        : String(value.value ?? "");
    return { label, value: value.value ?? label };
  });
  const [value, setValue] = useState<unknown>(
    typeof raw.prefill === "string" ? raw.prefill : "",
  );
  const ready = method !== "select" || options.some((option) => option.value === value);
  return (
    <div className="ap-question-form">
      <fieldset disabled={disabled}>
        <legend>{String(raw.title ?? raw.message ?? (method === "select" ? "请选择" : "请输入"))}</legend>
        {raw.title && raw.message && raw.title !== raw.message ? (
          <p>{String(raw.message)}</p>
        ) : null}
        {method === "select" ? (
          <div className="ap-question-options">
            {options.map((option, index) => (
              <label key={`${String(option.value)}:${index}`}>
                <input
                  type="radio"
                  name={`pi-extension-${String(raw.id ?? raw.requestId ?? "select")}`}
                  checked={option.value === value}
                  onChange={() => setValue(option.value)}
                />
                <span><strong>{option.label}</strong></span>
              </label>
            ))}
          </div>
        ) : method === "editor" ? (
          <textarea
            className="ap-question-other"
            rows={5}
            value={String(value)}
            onChange={(event) => setValue(event.target.value)}
            placeholder={typeof raw.placeholder === "string" ? raw.placeholder : undefined}
          />
        ) : (
          <input
            className="ap-question-other"
            value={String(value)}
            onChange={(event) => setValue(event.target.value)}
            placeholder={typeof raw.placeholder === "string" ? raw.placeholder : undefined}
          />
        )}
      </fieldset>
      <div className="ap-actions">
        <button
          className="btn primary sm"
          disabled={disabled || !ready}
          onClick={() => onDecide("accept", { value })}
        >
          {responding ? <Spinner sm /> : null}
          提交
        </button>
        <button className="btn danger sm" disabled={disabled} onClick={() => onDecide("cancel")}>
          取消
        </button>
      </div>
    </div>
  );
}

interface KimiQuestionOption {
  id: string;
  label: string;
  description?: string;
}

interface KimiQuestionItem {
  id: string;
  question: string;
  header?: string;
  body?: string;
  options: KimiQuestionOption[];
  multi_select?: boolean;
  allow_other?: boolean;
  other_label?: string;
}

function KimiQuestionForm({
  params,
  disabled,
  responding,
  onDecide,
}: {
  params: unknown;
  disabled?: boolean;
  responding: boolean;
  onDecide: (decision: string, response?: unknown) => void;
}) {
  const raw = (params ?? {}) as Record<string, unknown>;
  const questions = (Array.isArray(raw.questions) ? raw.questions : [])
    .filter((question): question is KimiQuestionItem => {
      if (!question || typeof question !== "object") return false;
      const item = question as Partial<KimiQuestionItem>;
      return typeof item.id === "string"
        && typeof item.question === "string"
        && Array.isArray(item.options);
    });
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [other, setOther] = useState<Record<string, string>>({});
  const answered = questions.length > 0 && questions.every((question) =>
    (selected[question.id]?.length ?? 0) > 0
    || (question.allow_other && (other[question.id]?.trim().length ?? 0) > 0)
  );
  const submit = () => {
    const answers: Record<string, unknown> = {};
    for (const question of questions) {
      const optionIds = selected[question.id] ?? [];
      const otherText = other[question.id]?.trim() ?? "";
      answers[question.id] = otherText
        ? optionIds.length
          ? { kind: "multi_with_other", option_ids: optionIds, other_text: otherText }
          : { kind: "other", text: otherText }
        : question.multi_select
          ? { kind: "multi", option_ids: optionIds }
          : { kind: "single", option_id: optionIds[0] };
    }
    onDecide("accept", { answers, method: "click" });
  };
  return (
    <div className="ap-question-form">
      {questions.map((question) => (
        <fieldset key={question.id} disabled={disabled}>
          <legend>{question.header || question.question}</legend>
          {question.header && question.header !== question.question ? (
            <p>{question.question}</p>
          ) : null}
          {question.body ? <p>{question.body}</p> : null}
          <div className="ap-question-options">
            {question.options.map((option) => {
              const checked = selected[question.id]?.includes(option.id) ?? false;
              return (
                <label key={option.id}>
                  <input
                    type={question.multi_select ? "checkbox" : "radio"}
                    name={`kimi-question-${question.id}`}
                    checked={checked}
                    onChange={(event) => {
                      setSelected((current) => {
                        const previous = current[question.id] ?? [];
                        const next = question.multi_select
                          ? event.target.checked
                            ? [...new Set([...previous, option.id])]
                            : previous.filter((id) => id !== option.id)
                          : [option.id];
                        return { ...current, [question.id]: next };
                      });
                    }}
                  />
                  <span>
                    <strong>{option.label}</strong>
                    {option.description ? <small>{option.description}</small> : null}
                  </span>
                </label>
              );
            })}
          </div>
          {question.allow_other ? (
            <input
              className="ap-question-other"
              value={other[question.id] ?? ""}
              onChange={(event) =>
                setOther((current) => ({ ...current, [question.id]: event.target.value }))}
              placeholder={question.other_label || "其他答案"}
            />
          ) : null}
        </fieldset>
      ))}
      <div className="ap-actions">
        <button className="btn primary sm" disabled={disabled || !answered} onClick={submit}>
          {responding ? <Spinner sm /> : null}
          提交回答
        </button>
        <button className="btn danger sm" disabled={disabled} onClick={() => onDecide("cancel")}>
          取消
        </button>
      </div>
    </div>
  );
}
