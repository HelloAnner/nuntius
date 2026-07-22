/* Message composer: autosizing textarea, image upload/paste, send / steer / interrupt. */
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import type { AttachmentView } from "../types";
import { isRunningStatus } from "../format";
import { IconArrowUp, IconImage, IconStop, IconX } from "./icons";
import { Spinner } from "./ui";

type ComposerDraftStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export interface ComposerDraftState {
  draftKey: string;
  text: string;
}

const memoryDrafts = new Map<string, string>();

function defaultDraftStorage(): ComposerDraftStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function draftStorageKey(draftKey: string): string {
  return `nuntius:draft:${draftKey}`;
}

export function loadComposerDraft(
  draftKey: string,
  storage: ComposerDraftStorage | null = defaultDraftStorage(),
): string {
  try {
    const stored = storage?.getItem(draftStorageKey(draftKey));
    if (stored != null) {
      if (stored) memoryDrafts.set(draftKey, stored);
      else memoryDrafts.delete(draftKey);
      return stored;
    }
  } catch {
    /* Keep drafts available in this tab when browser storage is unavailable. */
  }
  return memoryDrafts.get(draftKey) ?? "";
}

export function saveComposerDraft(
  draftKey: string,
  text: string,
  storage: ComposerDraftStorage | null = defaultDraftStorage(),
): void {
  if (text) memoryDrafts.set(draftKey, text);
  else memoryDrafts.delete(draftKey);

  try {
    if (text) storage?.setItem(draftStorageKey(draftKey), text);
    else storage?.removeItem(draftStorageKey(draftKey));
  } catch {
    /* The in-memory copy still isolates drafts for the lifetime of this tab. */
  }
}

export function resolveComposerDraft(
  current: ComposerDraftState,
  draftKey: string,
  storage: ComposerDraftStorage | null = defaultDraftStorage(),
): ComposerDraftState {
  return current.draftKey === draftKey
    ? current
    : { draftKey, text: loadComposerDraft(draftKey, storage) };
}

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
  onUpload,
  onDeleteAttachment,
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
  onSend: (text: string, attachments: AttachmentView[], clientMessageId: string) => void;
  onUpload?: (file: File, onProgress: (progress: number) => void) => Promise<AttachmentView>;
  onDeleteAttachment?: (attachmentId: string) => Promise<void>;
  onInterrupt: () => void;
}) {
  const [draft, setDraft] = useState<ComposerDraftState>(() => ({
    draftKey,
    text: loadComposerDraft(draftKey),
  }));
  const activeDraft = resolveComposerDraft(draft, draftKey);
  const text = activeDraft.text;
  const [uploads, setUploads] = useState<PendingImage[]>([]);
  const ref = useRef<HTMLTextAreaElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const uploadsRef = useRef<PendingImage[]>([]);
  const previousDraftKey = useRef(draftKey);

  useEffect(() => {
    uploadsRef.current = uploads;
  }, [uploads]);

  useEffect(() => {
    if (previousDraftKey.current === draftKey) return;
    for (const upload of uploadsRef.current) {
      URL.revokeObjectURL(upload.previewUrl);
      if (upload.attachment && onDeleteAttachment) {
        void onDeleteAttachment(upload.attachment.id).catch(() => {});
      }
    }
    uploadsRef.current = [];
    setUploads([]);
    previousDraftKey.current = draftKey;
  }, [draftKey, onDeleteAttachment]);

  useEffect(() => () => {
    for (const upload of uploadsRef.current) URL.revokeObjectURL(upload.previewUrl);
  }, []);

  useLayoutEffect(() => {
    if (draft.draftKey !== draftKey) setDraft(activeDraft);
  }, [activeDraft, draft.draftKey, draftKey]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 140)}px`;
  }, [text]);

  const updateText = (nextText: string) => {
    setDraft({ draftKey, text: nextText });
    saveComposerDraft(draftKey, nextText);
  };

  const uploadFile = (entry: PendingImage) => {
    if (!onUpload) return;
    setUploads((current) => current.map((item) => item.localId === entry.localId
      ? { ...item, status: "uploading", progress: 0, error: null }
      : item));
    void onUpload(entry.file, (progress) => {
      setUploads((current) => current.map((item) => item.localId === entry.localId
        ? { ...item, progress }
        : item));
    }).then((attachment) => {
      setUploads((current) => current.map((item) => item.localId === entry.localId
        ? { ...item, status: "ready", progress: 100, attachment }
        : item));
    }).catch((error) => {
      setUploads((current) => current.map((item) => item.localId === entry.localId
        ? { ...item, status: "failed", error: error instanceof Error ? error.message : "上传失败" }
        : item));
    });
  };

  const selectFiles = (files: FileList | readonly File[] | null) => {
    if (!files || !onUpload) return;
    const remaining = Math.max(0, 4 - uploads.length);
    for (const file of Array.from(files).slice(0, remaining)) {
      const entry: PendingImage = {
        localId: `image:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`,
        file,
        previewUrl: URL.createObjectURL(file),
        progress: 0,
        status: "uploading",
        attachment: null,
        error: null,
      };
      setUploads((current) => [...current, entry]);
      if (file.size > 20 * 1024 * 1024) {
        setUploads((current) => current.map((item) => item.localId === entry.localId
          ? { ...item, status: "failed", error: "图片不能超过 20 MB" }
          : item));
      } else if (file.type && !["image/jpeg", "image/png", "image/webp"].includes(file.type)) {
        setUploads((current) => current.map((item) => item.localId === entry.localId
          ? { ...item, status: "failed", error: "仅支持 JPEG、PNG 和 WebP" }
          : item));
      } else {
        uploadFile(entry);
      }
    }
    if (fileRef.current) fileRef.current.value = "";
  };

  const removeUpload = (entry: PendingImage) => {
    URL.revokeObjectURL(entry.previewUrl);
    setUploads((current) => current.filter((item) => item.localId !== entry.localId));
    if (entry.attachment && onDeleteAttachment) void onDeleteAttachment(entry.attachment.id).catch(() => {});
  };

  const uploadBusy = uploads.some((item) => item.status === "uploading");
  const uploadFailed = uploads.some((item) => item.status === "failed");
  const readyAttachments = uploads.flatMap((item) => item.attachment ? [item.attachment] : []);

  const submit = () => {
    const value = text.trim();
    if ((!value && readyAttachments.length === 0) || busy || uploadBusy || uploadFailed) return;
    const clientMessageId = typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : `msg-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    onSend(value, readyAttachments, clientMessageId);
    updateText("");
    for (const upload of uploads) URL.revokeObjectURL(upload.previewUrl);
    setUploads([]);
  };

  const locked = !canSend;
  return (
    <div className={`composer${locked ? " locked" : ""}`}>
      <RuntimeStatus status={runtimeStatus} connected={runtimeConnected} />
      {uploads.length ? (
        <div className="composer-attachments" aria-label="待发送图片">
          {uploads.map((upload) => (
            <div className={`composer-attachment ${upload.status}`} key={upload.localId}>
              <img src={upload.previewUrl} alt={upload.file.name} />
              {upload.status === "uploading" ? (
                <span className="attachment-progress" style={{ "--progress": `${upload.progress}%` } as CSSProperties}>
                  {upload.progress > 0 ? `${upload.progress}%` : <Spinner sm />}
                </span>
              ) : null}
              {upload.status === "failed" ? (
                <button className="attachment-error" onClick={() => uploadFile(upload)} title={upload.error ?? "上传失败"}>
                  重试
                </button>
              ) : null}
              <button className="attachment-remove" onClick={() => removeUpload(upload)} aria-label={`移除 ${upload.file.name}`}>
                <IconX size={13} />
              </button>
            </div>
          ))}
        </div>
      ) : null}
      <div className="composer-inner">
        {onUpload ? (
          <>
            <input
              ref={fileRef}
              className="composer-file-input"
              type="file"
              accept="image/jpeg,image/png,image/webp"
              multiple
              onChange={(event) => selectFiles(event.target.files)}
            />
            <button
              className="attach-btn"
              onClick={() => fileRef.current?.click()}
              disabled={locked || busy || uploads.length >= 4}
              aria-label="添加图片，也可以直接粘贴"
              title="选择图片，或直接粘贴"
            >
              <IconImage size={19} />
            </button>
          </>
        ) : null}
        <textarea
          ref={ref}
          rows={1}
          value={text}
          disabled={locked}
          placeholder={
            locked
              ? (lockedReason ?? "当前不可发送")
              : (placeholder ?? (onUpload ? "输入消息，或粘贴图片…" : "输入消息…"))
          }
          onChange={(e) => updateText(e.target.value)}
          onPaste={(event) => {
            if (!onUpload || locked || busy || uploads.length >= 4) return;
            const images = clipboardImageFiles(event.clipboardData);
            if (!images.length) return;
            event.preventDefault();
            selectFiles(images);
          }}
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
          disabled={locked || busy || uploadBusy || uploadFailed || (!text.trim() && readyAttachments.length === 0)}
          aria-label={running ? "发送指导" : "发送"}
        >
          {busy ? <Spinner sm /> : <IconArrowUp size={17} />}
        </button>
      </div>
    </div>
  );
}

type ClipboardImageSource = Pick<DataTransfer, "items" | "files">;

export function clipboardImageFiles(source: ClipboardImageSource): File[] {
  const itemImages = Array.from(source.items).flatMap((item) => {
    if (item.kind !== "file" || !item.type.startsWith("image/")) return [];
    const file = item.getAsFile();
    return file ? [file] : [];
  });
  if (itemImages.length) return itemImages;
  return Array.from(source.files).filter((file) => file.type.startsWith("image/"));
}

interface PendingImage {
  localId: string;
  file: File;
  previewUrl: string;
  progress: number;
  status: "uploading" | "ready" | "failed";
  attachment: AttachmentView | null;
  error: string | null;
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
  } else if (isRunningStatus(status)) {
    tone = "running";
    label = "正在运行";
  } else if (status === "recovering") {
    tone = "syncing";
    label = "正在恢复运行连接";
  } else if (status === "stalled") {
    tone = "warning";
    label = "长时间无活动，可尝试中断或继续";
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
