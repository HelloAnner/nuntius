/* small shared UI primitives */
import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import type { Tone } from "../format";
import { IconX } from "./icons";

export function Pill({
  tone,
  children,
  pulse,
}: {
  tone?: Tone;
  children: ReactNode;
  pulse?: boolean;
}) {
  return (
    <span className={`pill${tone ? ` tone-${tone}` : ""}`}>
      <span className={`dot${pulse ? " pulse" : ""}`} />
      {children}
    </span>
  );
}

export function Spinner({ sm }: { sm?: boolean }) {
  return <span className={`spinner${sm ? " sm" : ""}`} role="status" aria-label="加载中" />;
}

export function Empty({
  icon,
  headline,
  hint,
  action,
}: {
  icon: ReactNode;
  headline: string;
  hint?: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      <div className="glyph">{icon}</div>
      <div className="headline">{headline}</div>
      {hint ? <div className="hint">{hint}</div> : null}
      {action ? <div style={{ marginTop: 14 }}>{action}</div> : null}
    </div>
  );
}

export function Avatar({
  text,
  tint = 1,
  sm,
  online,
}: {
  text: string;
  tint?: number;
  sm?: boolean;
  online?: boolean;
}) {
  return (
    <span
      className={`avatar${sm ? " sm" : ""}`}
      style={{ background: `var(--tint-${tint})` }}
    >
      {text}
      {online !== undefined ? (
        <span className={`presence${online ? " online" : ""}`} />
      ) : null}
    </span>
  );
}

/* ---- bottom sheet / modal ---- */
export function Sheet({
  open,
  onClose,
  title,
  children,
  trailing,
  className,
}: {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  trailing?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = "";
    };
  }, [open, onClose]);
  if (!open) return null;
  return createPortal(
    <>
      <div className="sheet-backdrop" onClick={onClose} />
      <div className={`sheet${className ? ` ${className}` : ""}`} role="dialog" aria-modal="true">
        <div className="grabber" />
        <div className="sheet-head">
          <div style={{ flex: 1, minWidth: 0, fontWeight: 600, fontSize: 16 }}>
            {title}
          </div>
          {trailing}
          <button className="icon-btn" onClick={onClose} aria-label="关闭">
            <IconX size={18} />
          </button>
        </div>
        <div className="sheet-body">{children}</div>
      </div>
    </>,
    document.body,
  );
}

/* ---- toast ---- */
interface ToastItem {
  id: number;
  text: string;
  error: boolean;
}
type ToastFn = (text: string, opts?: { error?: boolean }) => void;
const ToastCtx = createContext<ToastFn>(() => {});

export function ToastHost({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([]);
  const next = useRef(1);
  const push: ToastFn = (text, opts) => {
    const id = next.current++;
    setItems((xs) => [...xs, { id, text, error: Boolean(opts?.error) }]);
    setTimeout(() => setItems((xs) => xs.filter((t) => t.id !== id)), 3200);
  };
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="toast-host" aria-live="polite">
        {items.map((t) => (
          <div key={t.id} className={`toast${t.error ? " error" : ""}`}>
            {t.text}
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}
export const useToast = () => useContext(ToastCtx);

/* ---- segmented control ---- */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="segmented" role="tablist">
      {options.map((o) => (
        <button
          key={o.value}
          role="tab"
          aria-selected={o.value === value}
          className={o.value === value ? "on" : ""}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/* ---- connection status pill ---- */
export type ConnState = "live" | "busy" | "down";
export function ConnPill({ state, label }: { state: ConnState; label: string }) {
  return (
    <span className={`conn-pill ${state}`}>
      <span className="dot" />
      {label}
    </span>
  );
}

/* ---- confirm inline hook: returns a function wrapping an action with window.confirm-free UI ---- */
export function useConfirmAction() {
  const [pending, setPending] = useState<{
    title: string;
    body?: string;
    confirmLabel: string;
    danger?: boolean;
    action: () => void;
  } | null>(null);
  const confirm = (cfg: {
    title: string;
    body?: string;
    confirmLabel: string;
    danger?: boolean;
    action: () => void;
  }) => setPending(cfg);
  const node = (
    <Sheet
      open={pending !== null}
      onClose={() => setPending(null)}
      title={pending?.title}
    >
      <div style={{ padding: 20 }}>
        {pending?.body ? (
          <p style={{ color: "var(--ink-2)", fontSize: 14, lineHeight: 1.65, marginBottom: 20 }}>
            {pending.body}
          </p>
        ) : null}
        <div style={{ display: "flex", gap: 10 }}>
          <button className="btn ghost block" onClick={() => setPending(null)}>
            取消
          </button>
          <button
            className={`btn block${pending?.danger ? " danger" : " primary"}`}
            onClick={() => {
              pending?.action();
              setPending(null);
            }}
          >
            {pending?.confirmLabel ?? "确认"}
          </button>
        </div>
      </div>
    </Sheet>
  );
  return { confirm, node };
}

/* ---- theme ---- */
export type Theme = "auto" | "light" | "dark";
export function applyTheme(theme: Theme) {
  const dark =
    theme === "dark" ||
    (theme === "auto" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "";
}
export function useTheme(theme: Theme) {
  useEffect(() => {
    applyTheme(theme);
    if (theme !== "auto") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const fn = () => applyTheme("auto");
    mq.addEventListener("change", fn);
    return () => mq.removeEventListener("change", fn);
  }, [theme]);
}
