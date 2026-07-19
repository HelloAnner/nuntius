/* Message composer: autosizing textarea, send / steer / interrupt. */
import { useEffect, useRef, useState } from "react";
import { IconArrowUp, IconStop } from "./icons";
import { Spinner } from "./ui";

export function Composer({
  draftKey,
  canSend,
  lockedReason,
  running,
  runtimeStatus,
  runtimeConnected,
  busy,
  placeholder,
  onSend,
  onInterrupt,
}: {
  draftKey: string;
  canSend: boolean;
  lockedReason?: string | null;
  running: boolean;
  runtimeStatus: string | null;
  runtimeConnected: boolean;
  busy?: boolean;
  placeholder?: string;
  onSend: (text: string) => void;
  onInterrupt: () => void;
}) {
  const storageKey = `nuntius:draft:${draftKey}`;
  const [text, setText] = useState(() => localStorage.getItem(storageKey) ?? "");
  const ref = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    localStorage.setItem(storageKey, text);
  }, [storageKey, text]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 140)}px`;
  }, [text]);

  const submit = () => {
    const value = text.trim();
    if (!value || busy) return;
    onSend(value);
    setText("");
  };

  const locked = !canSend;
  return (
    <div className={`composer${locked ? " locked" : ""}`}>
      <RuntimeStatus status={runtimeStatus} connected={runtimeConnected} />
      <div className="composer-inner">
        <textarea
          ref={ref}
          rows={1}
          value={text}
          disabled={locked}
          placeholder={
            locked
              ? (lockedReason ?? "当前不可发送")
              : (placeholder ?? "输入消息…")
          }
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              submit();
            }
          }}
          aria-label="消息输入"
        />
        {running ? (
          <button
            className="send-btn stop"
            onClick={onInterrupt}
            aria-label="中断执行"
            disabled={busy}
          >
            {busy ? <Spinner sm /> : <IconStop size={15} />}
          </button>
        ) : null}
        <button
          className="send-btn"
          onClick={submit}
          disabled={locked || busy || !text.trim()}
          aria-label={running ? "发送指导" : "发送"}
        >
          {busy ? <Spinner sm /> : <IconArrowUp size={17} />}
        </button>
      </div>
    </div>
  );
}

function RuntimeStatus({ status, connected }: { status: string | null; connected: boolean }) {
  let tone: "running" | "syncing" | "warning";
  let label: string;
  if (!connected) {
    tone = "warning";
    label = "连接已中断";
  } else if (!status) {
    tone = "syncing";
    label = "正在确认状态";
  } else if (status === "active") {
    tone = "running";
    label = "正在运行";
  } else if (status === "unknown" || status === "systemError") {
    tone = "warning";
    label = status === "systemError" ? "运行服务异常" : "状态待确认";
  } else {
    return null;
  }

  return (
    <div
      className={`thread-runtime ${tone}`}
      role="status"
      aria-live="polite"
      aria-label={label}
      title={label}
    >
      <span className="thread-runtime-dot" aria-hidden="true" />
    </div>
  );
}
