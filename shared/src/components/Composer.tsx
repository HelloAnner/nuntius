/* Message composer: autosizing textarea, send / steer / interrupt. */
import { useEffect, useRef, useState } from "react";
import { IconArrowUp, IconStop } from "./icons";
import { Spinner } from "./ui";

export function Composer({
  draftKey,
  canSend,
  lockedReason,
  running,
  busy,
  placeholder,
  onSend,
  onSteer,
  onInterrupt,
}: {
  draftKey: string;
  canSend: boolean;
  lockedReason?: string | null;
  running: boolean;
  busy?: boolean;
  placeholder?: string;
  onSend: (text: string) => void;
  onSteer: (text: string) => void;
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
    if (running) onSteer(value);
    else onSend(value);
    setText("");
  };

  const locked = !canSend;
  return (
    <div className={`composer${locked ? " locked" : ""}`}>
      <div className="composer-inner">
        <textarea
          ref={ref}
          rows={1}
          value={text}
          disabled={locked}
          placeholder={
            locked
              ? (lockedReason ?? "当前不可发送")
              : running
                ? "追加指导，转向当前任务…"
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
            title="中断当前 Turn"
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
