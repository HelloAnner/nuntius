/* Conversation item renderers: user bubble, agent message, work items,
 * approval card. Used identically by the remote and local consoles. */
import { memo, useState, type ReactNode } from "react";
import type { LiveItem, LiveStatus } from "../stream";
import { Markdown } from "./Markdown";
import {
  IconAlert,
  IconChevronRight,
  IconFile,
  IconShield,
  IconSparkle,
  IconTerminal,
  IconThought,
  IconTool,
} from "./icons";
import { Spinner } from "./ui";

/* ---------- user ---------- */
export const UserBubble = memo(function UserBubble({
  text,
  state,
  stateLabel,
  stateError,
  errorMessage,
  onRetry,
}: {
  text: string;
  state?: string | null;
  stateLabel?: string | null;
  stateError?: boolean;
  errorMessage?: string | null;
  onRetry?: () => void;
}) {
  return (
    <div className="msg-user">
      <div className="bubble">{text}</div>
      {stateLabel ? (
        <div className={`send-state${stateError ? " err" : ""}`}>
          {state === "applying" || state === "accepted" || state === "waiting_device" ? (
            <Spinner sm />
          ) : null}
          <span className="send-state-label">{stateLabel}</span>
          {stateError && errorMessage ? (
            <span className="send-state-error">· {errorMessage}</span>
          ) : null}
          {stateError && onRetry ? (
            <button className="send-retry" onClick={onRetry}>重试</button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
});

/* ---------- agent ---------- */
export const AgentMessage = memo(function AgentMessage({
  text,
  streaming,
}: {
  text: string;
  streaming?: boolean;
}) {
  return (
    <div className="msg-agent">
      <span className="mark">
        <IconSparkle size={13} />
      </span>
      <div className="body">
        {text ? (
          <Markdown text={text} />
        ) : streaming ? (
          <span style={{ color: "var(--ink-3)", fontStyle: "italic" }}>正在思考…</span>
        ) : null}
        {streaming && text ? <span className="caret" /> : null}
        {!text && streaming ? <span className="caret" /> : null}
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
  onDecide: (decision: string) => void;
  locked?: boolean;
}) {
  const { kind, detail } = approvalSummary(approval.method, approval.params);
  const pending = approval.state === "pending";
  const responding = approval.state === "responding";
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
      {detail && detail !== "null" ? (
        <div className="ap-detail">
          <pre>{detail.length > 4000 ? `${detail.slice(0, 4000)}\n…（内容已截断）` : detail}</pre>
        </div>
      ) : null}
      {pending || responding ? (
        <div className="ap-actions">
          <button
            className="btn primary sm"
            disabled={responding || locked}
            onClick={() => onDecide("accept")}
          >
            {responding ? <Spinner sm /> : null}
            批准
          </button>
          <button
            className="btn ghost sm"
            disabled={responding || locked}
            onClick={() => onDecide("accept_for_session")}
          >
            本会话都允许
          </button>
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
