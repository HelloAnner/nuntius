/* App chrome: top bar, bottom tab bar, nav rail, list rows, banners. */
import type { ReactNode } from "react";
import { useNavigate } from "./hooks";
import {
  Avatar,
  ConnPill,
  initials,
  osLabel,
  relTime,
  providerLabel,
  statusLabel,
  tintIndex,
  truncateMiddle,
  type ConnState,
  type DeviceSummary,
  type ProjectSummary,
  type ThreadSummary,
  IconChat,
  IconChevronLeft,
  IconChevronRight,
  IconClock,
  IconDevice,
  IconFolder,
  IconGit,
  IconSettings,
  IconShield,
} from "@nuntius/shared";
import { usePendingApprovalCount, useRoute, type Route } from "./stores";
import { useSse } from "./events";

/* ---------- connection indicator ---------- */
export function ConnIndicator() {
  const status = useSse((s) => s.status);
  const map: Record<string, { state: ConnState; label: string }> = {
    live: { state: "live", label: "实时" },
    connecting: { state: "busy", label: "连接中" },
    reconnecting: { state: "busy", label: "重连中" },
    syncing: { state: "busy", label: "同步中" },
  };
  const { state, label } = map[status] ?? map.connecting;
  return <ConnPill state={state} label={label} />;
}

/* ---------- top bar ---------- */
export function TopBar({
  title,
  subtitle,
  onBack,
  trailing,
  onTitleClick,
  titleHint,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  onBack?: () => void;
  trailing?: ReactNode;
  onTitleClick?: () => void;
  titleHint?: string;
}) {
  return (
    <header className="topbar">
      <div className="topbar-side">
        {onBack ? (
          <button className="icon-btn" onClick={onBack} aria-label="返回">
            <IconChevronLeft size={20} />
          </button>
        ) : null}
      </div>
      <button
        className={`topbar-title${onTitleClick ? " clickable" : ""}`}
        onClick={onTitleClick}
        disabled={!onTitleClick}
        aria-label={titleHint}
        title={titleHint}
      >
        <span className="t">{title}</span>
        {subtitle ? <span className="s">{subtitle}</span> : null}
      </button>
      <div className="topbar-side right">{trailing}</div>
    </header>
  );
}

/* ---------- bottom tab bar (mobile) ---------- */
const TABS: { route: Route; label: string; icon: (size: number) => ReactNode }[] = [
  { route: { name: "devices" }, label: "设备", icon: (s) => <IconDevice size={s} /> },
  { route: { name: "recents" }, label: "最近", icon: (s) => <IconClock size={s} /> },
  { route: { name: "approvals" }, label: "审批", icon: (s) => <IconShield size={s} /> },
  { route: { name: "settings" }, label: "设置", icon: (s) => <IconSettings size={s} /> },
];

function tabActive(route: Route, tab: Route): boolean {
  if (tab.name === "devices") {
    return route.name === "devices" || route.name === "device" || route.name === "project" || route.name === "thread";
  }
  if (tab.name === "recents") return route.name === "recents" || route.name === "recentThread";
  return route.name === tab.name;
}

export function TabBar() {
  const { route, navigate } = useRoute();
  const pending = usePendingApprovalCount();
  return (
    <nav className="tabbar">
      {TABS.map((t) => (
        <button
          key={t.label}
          className={`tab${tabActive(route, t.route) ? " on" : ""}`}
          onClick={() => navigate(t.route)}
        >
          <span className="tab-icon">
            {t.icon(21)}
            {t.route.name === "approvals" && pending > 0 ? (
              <span className="badge num">{pending > 99 ? "99+" : pending}</span>
            ) : null}
          </span>
          <span className="tab-label">{t.label}</span>
        </button>
      ))}
    </nav>
  );
}

/* ---------- nav rail (tablet / desktop) ---------- */
export function NavRail() {
  const { route, navigate } = useRoute();
  const pending = usePendingApprovalCount();
  return (
    <nav className="navrail">
      <div className="rail-logo display">N</div>
      {TABS.map((t) => (
        <button
          key={t.label}
          className={`rail-btn${tabActive(route, t.route) ? " on" : ""}`}
          onClick={() => navigate(t.route)}
          title={t.label}
        >
          <span className="tab-icon">
            {t.icon(21)}
            {t.route.name === "approvals" && pending > 0 ? (
              <span className="badge num">{pending > 99 ? "99+" : pending}</span>
            ) : null}
          </span>
          <span className="rail-label">{t.label}</span>
        </button>
      ))}
      <div style={{ flex: 1 }} />
      <ConnIndicator />
    </nav>
  );
}

/* ---------- rows ---------- */
export function DeviceRow({ device }: { device: DeviceSummary }) {
  const navigate = useNavigate();
  const online = device.status === "online";
  const transient = device.status === "syncing" || device.status === "pairing";
  return (
    <button
      className="list-row"
      onClick={() => navigate({ name: "device", deviceId: device.id })}
    >
      <Avatar
        text={initials(device.displayName)}
        tint={tintIndex(device.id)}
        online={device.status === "online" ? true : device.status === "offline" ? false : undefined}
      />
      <div className="grow">
        <div className="title">{device.displayName}</div>
        <div className="sub">
          <span>{osLabel(device.osFamily, device.architecture)}</span>
          <span className="sep">·</span>
          <span>{online || transient ? `${device.projectCount} 个项目` : `${relTime(device.lastSeenAt)}在线`}</span>
        </div>
      </div>
      <div className="trailing">
        {device.pendingApprovalCount > 0 ? (
          <span className="row-signal approval" role="img" aria-label={`${device.pendingApprovalCount} 个待审批`} title={`${device.pendingApprovalCount} 个待审批`}>
            <IconShield size={16} />
            <span className="signal-count num">{device.pendingApprovalCount}</span>
          </span>
        ) : null}
        {device.activeTurnCount > 0 ? (
          <span className="row-signal activity" role="img" aria-label={`${device.activeTurnCount} 个会话运行中`} title={`${device.activeTurnCount} 个会话运行中`}>
            <span className="live-dot" />
            {device.activeTurnCount > 1 ? <span className="signal-count num">{device.activeTurnCount}</span> : null}
          </span>
        ) : null}
        {transient ? (
          <span className="row-state-spinner" role="status" aria-label={statusLabel(device.status)} title={statusLabel(device.status)} />
        ) : device.status === "degraded" || device.status === "revoked" ? (
          <span className={`row-state-dot ${device.status}`} role="img" aria-label={statusLabel(device.status)} title={statusLabel(device.status)} />
        ) : null}
        <IconChevronRight size={16} />
      </div>
    </button>
  );
}

export function ProjectRow({
  project,
  onClick,
}: {
  project: ProjectSummary;
  onClick: () => void;
}) {
  const unassigned = project.kind === "system_unassigned";
  return (
    <button className="list-row" onClick={onClick}>
      <span className={`row-glyph${unassigned ? " muted" : ""}`}>
        {unassigned ? <IconChat size={17} /> : project.repoName ? <IconGit size={17} /> : <IconFolder size={17} />}
      </span>
      <div className="grow">
        <div className="title">{project.displayName}</div>
        <div className="sub">
          {unassigned ? (
            <span className="ellipsis">未归类 · 仅可阅读历史</span>
          ) : (
            <>
              {project.branch ? <span className="mono ellipsis">{project.branch}{project.isDirty ? "*" : ""}</span> : null}
              {project.pathHint ? <span className="ellipsis">{truncateMiddle(project.pathHint, 36)}</span> : null}
              {!project.branch && !project.pathHint ? <span>{project.threadCount} 个会话</span> : null}
            </>
          )}
        </div>
      </div>
      <div className="trailing">
        <span className="num" style={{ fontSize: 12 }}>{relTime(project.lastActivityAt)}</span>
        <IconChevronRight size={16} />
      </div>
    </button>
  );
}

export function ThreadRow({
  thread,
  deviceName,
  projectName,
  selected = false,
  onClick,
}: {
  thread: ThreadSummary;
  deviceName: string;
  projectName: string;
  selected?: boolean;
  onClick: () => void;
}) {
  const active = thread.status === "active";
  return (
    <button
      className={`list-row${selected ? " selected" : ""}`}
      onClick={onClick}
      aria-current={selected ? "page" : undefined}
    >
      <span className={`row-glyph thread${thread.archived ? " muted" : ""}`}>
        <IconChat size={16} />
      </span>
      <div className="grow">
        <div className="title" style={thread.archived ? { color: "var(--ink-3)" } : undefined}>
          {thread.title || "未命名会话"}
        </div>
        <div className="sub thread-context" aria-label={`${deviceName}，${projectName}`}>
          <span className="ellipsis">{deviceName}</span>
          <span className="sep" aria-hidden="true">·</span>
          <span className="ellipsis">{projectName}</span>
        </div>
      </div>
      <div className="trailing">
        {active ? <span className="live-dot" aria-label="进行中" /> : null}
        <IconChevronRight size={16} />
      </div>
    </button>
  );
}
