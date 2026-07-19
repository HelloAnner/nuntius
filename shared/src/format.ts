/* small formatting helpers (zh-CN) */

export function relTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const diff = Date.now() - t;
  const abs = Math.abs(diff);
  const min = 60_000;
  const hour = 60 * min;
  const day = 24 * hour;
  if (abs < 45_000) return "刚刚";
  if (diff > 0) {
    if (abs < hour) return `${Math.floor(abs / min)} 分钟前`;
    if (abs < day) return `${Math.floor(abs / hour)} 小时前`;
    if (abs < 2 * day) return "昨天";
    if (abs < 7 * day) return `${Math.floor(abs / day)} 天前`;
  }
  const d = new Date(t);
  const now = new Date();
  const sameYear = d.getFullYear() === now.getFullYear();
  const md = `${d.getMonth() + 1}月${d.getDate()}日`;
  return sameYear ? md : `${d.getFullYear()}年${md}`;
}

export function clockTime(iso: string | null | undefined): string {
  if (!iso) return "";
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return "";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(t.getHours())}:${p(t.getMinutes())}`;
}

export function fullTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return "—";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${t.getFullYear()}-${p(t.getMonth() + 1)}-${p(t.getDate())} ${p(t.getHours())}:${p(t.getMinutes())}`;
}

type RecentActivityItem = {
  id: string;
  lastActivityAt: string | null | undefined;
};

/** Newest meaningful activity first; missing or invalid timestamps stay last. */
export function compareByRecentActivity(a: RecentActivityItem, b: RecentActivityItem): number {
  const time = (value: string | null | undefined) => {
    const parsed = value ? Date.parse(value) : Number.NaN;
    return Number.isNaN(parsed) ? Number.NEGATIVE_INFINITY : parsed;
  };
  const difference = time(b.lastActivityAt) - time(a.lastActivityAt);
  return difference || a.id.localeCompare(b.id);
}

const STATUS_LABELS: Record<string, string> = {
  online: "在线",
  offline: "离线",
  syncing: "同步中",
  degraded: "降级",
  pairing: "配对中",
  revoked: "已撤销",
  active: "进行中",
  recovering: "恢复中",
  idle: "空闲",
  running: "运行中",
  interrupted: "已中断",
  completed: "已完成",
  failed: "失败",
  waiting_approval: "等待审批",
  unknown: "待核对",
  accepted: "已受理",
  waiting_device: "等待设备",
  device_accepted: "已送达",
  applying: "执行中",
  rejected: "已拒绝",
  expired: "已过期",
  archived: "已归档",
  pending: "待处理",
  responding: "提交中",
  approved: "已批准",
  denied: "已拒绝",
  cancelled: "已取消",
  complete: "完整",
  backfilling: "回填中",
  partial: "部分",
  not_started: "未开始",
  error: "异常",
  paused: "已暂停",
  notloaded: "未加载",
  notLoaded: "未加载",
};

export function statusLabel(status: string | null | undefined): string {
  if (!status) return "—";
  return STATUS_LABELS[status] ?? status;
}

export type Tone = "ok" | "warn" | "danger" | "info" | "muted";

export function deviceTone(status: string): Tone {
  switch (status) {
    case "online":
      return "ok";
    case "syncing":
    case "pairing":
      return "info";
    case "degraded":
      return "warn";
    case "offline":
      return "muted";
    case "revoked":
      return "danger";
    default:
      return "muted";
  }
}

export function completenessLabel(c: string | null | undefined): string {
  return statusLabel(c ?? "not_started");
}

export function osLabel(osFamily: string | null, arch?: string | null): string {
  const os =
    (
      {
        macos: "macOS",
        darwin: "macOS",
        linux: "Linux",
        windows: "Windows",
      } as Record<string, string>
    )[osFamily ?? ""] ?? (osFamily || "未知系统");
  return arch ? `${os} · ${arch}` : os;
}

export function initials(name: string): string {
  const clean = name.trim();
  if (!clean) return "?";
  const parts = clean.split(/[\s_\-.]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return Array.from(clean).slice(0, 2).join("").toUpperCase();
}

/** deterministic muted tint index for device avatars */
export function tintIndex(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return (h % 6) + 1;
}

export function newIdemKey(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `idem_${Date.now()}_${Math.random().toString(36).slice(2)}`;
}

export function truncateMiddle(text: string, max = 48): string {
  if (text.length <= max) return text;
  const head = Math.ceil(max * 0.6);
  const tail = Math.floor(max * 0.32);
  return `${text.slice(0, head)}…${text.slice(-tail)}`;
}
