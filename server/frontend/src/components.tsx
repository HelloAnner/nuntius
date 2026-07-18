/* App chrome: top bar, bottom tab bar, nav rail, list rows, banners. */
import type { ReactNode } from "react";
import { useNavigate } from "./hooks";
import {
  Avatar,
  ConnPill,
  Pill,
  deviceTone,
  initials,
  osLabel,
  relTime,
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
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  onBack?: () => void;
  trailing?: ReactNode;
  onTitleClick?: () => void;
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
  return (
    <button
      className="list-row"
      onClick={() => navigate({ name: "device", deviceId: device.id })}
    >
      <Avatar text={initials(device.displayName)} tint={tintIndex(device.id)} online={online} />
      <div className="grow">
        <div className="title">{device.displayName}</div>
        <div className="sub">
          <span>{osLabel(device.osFamily, device.architecture)}</span>
          <span className="sep">·</span>
          <span>{online ? `${device.projectCount} 个项目` : `${relTime(device.lastSeenAt)}在线`}</span>
        </div>
      </div>
      <div className="trailing">
        {device.pendingApprovalCount > 0 ? (
          <Pill tone="warn" pulse>
            {device.pendingApprovalCount} 待审批
          </Pill>
        ) : null}
        {device.activeTurnCount > 0 ? <Pill tone="info" pulse>{device.activeTurnCount} 运行中</Pill> : null}
        <Pill tone={deviceTone(device.status)} pulse={device.status === "syncing"}>
          {statusLabel(device.status)}
        </Pill>
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
  context,
  onClick,
}: {
  thread: ThreadSummary;
  context?: string;
  onClick: () => void;
}) {
  const active = thread.status === "active";
  return (
    <button className="list-row" onClick={onClick}>
      <span className={`row-glyph thread${active ? " live" : ""}${thread.archived ? " muted" : ""}`}>
        <IconChat size={16} />
      </span>
      <div className="grow">
        <div className="title" style={thread.archived ? { color: "var(--ink-3)" } : undefined}>
          {thread.title || "未命名会话"}
        </div>
        <div className="sub">
          {context ? <span className="ellipsis">{context}</span> : null}
          <span>{statusLabel(thread.status)}</span>
          {thread.archived ? <span>· 已归档</span> : null}
        </div>
      </div>
      <div className="trailing">
        {active ? <span className="live-dot" aria-label="进行中" /> : null}
        <span className="num" style={{ fontSize: 12 }}>{relTime(thread.lastActivityAt)}</span>
        <IconChevronRight size={16} />
      </div>
    </button>
  );
}

/* ---------- insecure transport banner ---------- */
export function InsecureBanner() {
  return (
    <div className="insecure-banner" role="alert">
      当前为 HTTP 不安全传输：登录凭证与会话内容可能被窃听，请仅在可信网络中使用
    </div>
  );
}
