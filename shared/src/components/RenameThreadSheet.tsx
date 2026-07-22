import { useEffect, useState, type FormEvent } from "react";
import type { ThreadSummary } from "../types";
import { Sheet, Spinner } from "./ui";

export function RenameThreadSheet({
  thread,
  open,
  onClose,
  onRename,
}: {
  thread: ThreadSummary | null;
  open: boolean;
  onClose: () => void;
  onRename: (title: string | null) => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!open || !thread) return;
    setValue(thread.title);
    setFailed(false);
  }, [open, thread?.id, thread?.title]);

  const normalized = value.trim();
  const byteLength = new TextEncoder().encode(normalized).length;
  const hasControlCharacter = [...normalized].some((character) =>
    /[\u0000-\u001f\u007f]/.test(character),
  );
  const invalid = byteLength === 0 || byteLength > 256 || hasControlCharacter;
  const unchanged = normalized === thread?.title;
  const validationMessage =
    byteLength === 0
      ? "请输入会话名称"
      : byteLength > 256
        ? "UTF-8 编码后最多 256 字节"
        : hasControlCharacter
          ? "名称只能占一行"
          : "单行名称；自动标题仍会在后台保留";

  const save = async (title: string | null) => {
    if (!thread || saving) return;
    setSaving(true);
    setFailed(false);
    try {
      await onRename(title);
      onClose();
    } catch {
      setFailed(true);
    } finally {
      setSaving(false);
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!invalid && !unchanged) void save(normalized);
  };

  return (
    <Sheet
      open={open && thread !== null}
      onClose={() => {
        if (!saving) onClose();
      }}
      title="重命名会话"
      className="rename-thread-sheet"
    >
      <form className="rename-thread-form" onSubmit={submit}>
        <p className="rename-thread-intro">
          只修改 Nuntius 中显示的名称，不影响 Codex、Kimi 或 Pi 中的原始会话。
        </p>
        <div className="field">
          <label htmlFor="thread-display-title">会话名称</label>
          <input
            id="thread-display-title"
            value={value}
            onChange={(event) => {
              setValue(event.target.value);
              setFailed(false);
            }}
            onFocus={(event) => event.currentTarget.select()}
            autoComplete="off"
            autoFocus
            maxLength={256}
            aria-invalid={invalid || failed}
            aria-describedby="thread-title-help"
          />
          <div
            id="thread-title-help"
            className={`rename-thread-meta${invalid || failed ? " error" : ""}`}
          >
            <span>
              {failed
                ? "名称未能保存，请检查连接后重试"
                : validationMessage}
            </span>
            <span className="num">{byteLength}/256</span>
          </div>
        </div>
        {thread?.displayTitleOverride != null ? (
          <button
            type="button"
            className="rename-thread-reset"
            onClick={() => void save(null)}
            disabled={saving}
          >
            恢复自动标题
          </button>
        ) : null}
        <div className="rename-thread-actions">
          <button type="button" className="btn ghost block" onClick={onClose} disabled={saving}>
            取消
          </button>
          <button type="submit" className="btn primary block" disabled={invalid || unchanged || saving}>
            {saving ? <Spinner sm /> : null}
            保存名称
          </button>
        </div>
      </form>
    </Sheet>
  );
}
