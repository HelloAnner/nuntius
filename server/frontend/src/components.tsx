/* Server console chrome and design-system primitives. */
import type { ReactNode } from "react";
import {
  IconChat,
  IconChevronLeft,
  IconChevronRight,
  IconClock,
  IconDevice,
  IconFolder,
  IconGit,
  IconSearch,
  IconSettings,
  IconShield,
  initials,
  osLabel,
  providerLabel,
  relTime,
  statusLabel,
  truncateMiddle,
  type ConnState,
  type DeviceSummary,
  type ProjectSummary,
  type ThreadSummary,
} from "@nuntius/shared";
import { useNavigate } from "./hooks";
import { useSse } from "./events";
import { usePendingApprovalCount, useRoute, useSession, type Route } from "./stores";

export function ConnIndicator() {
  const status = useSse((state) => state.status);
  const map: Record<string, { state: ConnState; label: string }> = {
    live: { state: "live", label: "已连接" },
    connecting: { state: "busy", label: "连接中" },
    reconnecting: { state: "busy", label: "重连中" },
    syncing: { state: "busy", label: "同步中" },
  };
  const current = map[status] ?? map.connecting;
  return (
    <span className="console-connection">
      <span className={`connection-pulse ${current.state}`} aria-hidden="true">
        <span />
      </span>
      <span>{current.label}</span>
    </span>
  );
}

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

const TABS: { route: Route; label: string; icon: (size: number) => ReactNode }[] = [
  { route: { name: "recents" }, label: "最近", icon: (size) => <IconClock size={size} /> },
  { route: { name: "devices" }, label: "设备", icon: (size) => <IconDevice size={size} /> },
  { route: { name: "projects" }, label: "项目", icon: (size) => <IconFolder size={size} /> },
  { route: { name: "approvals" }, label: "审批", icon: (size) => <IconShield size={size} /> },
  { route: { name: "settings" }, label: "设置", icon: (size) => <IconSettings size={size} /> },
];

function tabActive(route: Route, tab: Route): boolean {
  if (tab.name === "devices") return route.name === "devices" || route.name === "device";
  if (tab.name === "projects") {
    return route.name === "projects" || route.name === "project" || route.name === "thread";
  }
  if (tab.name === "recents") return route.name === "recents" || route.name === "recentThread";
  return route.name === tab.name;
}

function PendingBadge({ count }: { count: number }) {
  if (count <= 0) return null;
  return <span className="badge num">{count > 99 ? "99+" : count}</span>;
}

export function TabBar() {
  const { route, navigate } = useRoute();
  const pending = usePendingApprovalCount();
  return (
    <nav className="tabbar" aria-label="主导航">
      {TABS.map((tab) => (
        <button
          key={tab.label}
          className={`tab${tabActive(route, tab.route) ? " on" : ""}`}
          onClick={() => navigate(tab.route)}
          aria-current={tabActive(route, tab.route) ? "page" : undefined}
        >
          <span className="tab-icon">
            {tab.icon(21)}
            {tab.route.name === "approvals" ? <PendingBadge count={pending} /> : null}
          </span>
          <span className="tab-label">{tab.label}</span>
        </button>
      ))}
    </nav>
  );
}

export function NavRail() {
  const { route, navigate } = useRoute();
  const loginName = useSession((state) => state.session?.loginName ?? "用户");
  const pending = usePendingApprovalCount();
  return (
    <nav className="navrail" aria-label="主导航">
      <div className="rail-top">
        <button className="rail-brand" onClick={() => navigate({ name: "recents" })}>
          <span className="rail-mark">N</span>
          <span>Nuntius</span>
        </button>
        <div className="rail-links">
          {TABS.map((tab) => {
            const active = tabActive(route, tab.route);
            return (
              <button
                key={tab.label}
                className={`rail-btn${active ? " on" : ""}`}
                onClick={() => navigate(tab.route)}
                aria-current={active ? "page" : undefined}
              >
                <span className="tab-icon">
                  {tab.icon(18)}
                  {tab.route.name === "approvals" ? <PendingBadge count={pending} /> : null}
                </span>
                <span className="rail-label">{tab.label === "最近" ? "最近会话" : tab.label}</span>
              </button>
            );
          })}
        </div>
      </div>
      <div className="rail-bottom">
        <ConnIndicator />
        <button className="rail-user" onClick={() => navigate({ name: "settings" })}>
          <span className="rail-avatar">{initials(loginName)}</span>
          <span className="rail-user-name">{loginName}</span>
        </button>
      </div>
    </nav>
  );
}

export function SearchField({
  value,
  onChange,
  placeholder = "搜索…",
  label = "搜索",
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  label?: string;
}) {
  return (
    <label className="console-search">
      <span className="sr-only">{label}</span>
      <IconSearch size={16} />
      <input value={value} onChange={(event) => onChange(event.target.value)} placeholder={placeholder} />
    </label>
  );
}

export function FilterSelect({
  value,
  onChange,
  options,
  label,
}: {
  value: string;
  onChange: (value: string) => void;
  options: { value: string; label: string }[];
  label: string;
}) {
  return (
    <label className="filter-select">
      <span className="sr-only">{label}</span>
      <select value={value} onChange={(event) => onChange(event.target.value)} aria-label={label}>
        {options.map((option) => (
          <option key={option.value} value={option.value}>{option.label}</option>
        ))}
      </select>
    </label>
  );
}

export type StatusTone = "active" | "warning" | "success" | "danger" | "idle" | "offline";

export function StatusDot({ tone, pulse = false }: { tone: StatusTone; pulse?: boolean }) {
  return <span className={`status-orbit ${tone}${pulse ? " pulse" : ""}`} aria-hidden="true"><span /></span>;
}

export function ProviderBadge({ provider }: { provider: ThreadSummary["provider"] }) {
  return (
    <span className={`provider-badge ${provider}`}>
      <span className="provider-mark">{provider === "kimi" ? "K" : "C"}</span>
      {providerLabel(provider)}
    </span>
  );
}

function deviceTone(device: DeviceSummary): StatusTone {
  if (device.status === "online") return "success";
  if (device.status === "syncing" || device.status === "pairing") return "warning";
  if (device.status === "degraded") return "danger";
  return "offline";
}

export function DeviceRow({ device }: { device: DeviceSummary }) {
  const navigate = useNavigate();
  const online = device.status === "online";
  const historyText =
    device.historyCompleteness === "backfilling"
      ? "历史同步中"
      : device.historyCompleteness === "complete"
        ? "历史已同步"
        : device.historyCompleteness === "error"
          ? "历史同步异常"
          : "历史未完整";
  return (
    <article className={`device-card device-${device.status}`}>
      <div className="device-card-head">
        <span className="device-icon"><IconDevice size={22} /></span>
        <div className="device-card-title">
          <strong>{device.displayName}</strong>
          <span>{osLabel(device.osFamily, device.architecture)}{device.agentVersion ? ` · CLI ${device.agentVersion}` : ""}</span>
        </div>
        <span className={`status-pill ${deviceTone(device)}`}>
          <span className="status-dot" />
          {statusLabel(device.status)}
        </span>
      </div>
      <div className="device-stats">
        <span><strong className="num">{device.projectCount}</strong><small>项目</small></span>
        <span><strong className="num">{device.activeTurnCount}</strong><small>运行中</small></span>
        <span><strong className="num">{device.pendingApprovalCount}</strong><small>待审批</small></span>
      </div>
      <div className="device-card-foot">
        <span className={device.historyCompleteness === "error" ? "danger-copy" : ""}>
          {online ? historyText : `最后在线：${relTime(device.lastSeenAt)}`}
        </span>
        <button className="btn outline sm" onClick={() => navigate({ name: "device", deviceId: device.id })}>
          查看项目
        </button>
      </div>
    </article>
  );
}

export function ProjectRow({ project, onClick }: { project: ProjectSummary; onClick: () => void }) {
  const unassigned = project.kind === "system_unassigned";
  return (
    <button className="list-row project-row" onClick={onClick}>
      <span className={`row-glyph${unassigned ? " muted" : ""}`}>
        {unassigned ? <IconChat size={17} /> : project.repoName ? <IconGit size={17} /> : <IconFolder size={17} />}
      </span>
      <span className="grow">
        <span className="title">{project.displayName}</span>
        <span className="sub mono">
          {unassigned ? "未归类 · 只读" : truncateMiddle(project.pathHint ?? project.repoName ?? "本地项目", 46)}
        </span>
      </span>
      {project.branch ? (
        <span className="branch-badge">
          <IconGit size={12} />
          <span className="mono">{project.branch}{project.isDirty ? "*" : ""}</span>
        </span>
      ) : null}
      <span className="project-count num">{project.threadCount} 会话</span>
      <span className="project-time num">{relTime(project.lastActivityAt)}</span>
      <IconChevronRight className="row-chevron" size={16} />
    </button>
  );
}

function threadTone(thread: ThreadSummary): StatusTone {
  if (thread.status === "active") return "active";
  if (thread.status === "recovering") return "warning";
  if (["failed", "error", "rejected"].includes(thread.status)) return "danger";
  if (["completed", "idle"].includes(thread.status)) return "success";
  return thread.archived ? "offline" : "idle";
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
  const tone = threadTone(thread);
  return (
    <button
      className={`list-row thread-row${selected ? " selected" : ""}`}
      onClick={onClick}
      aria-current={selected ? "page" : undefined}
    >
      <StatusDot tone={tone} pulse={tone === "active"} />
      <span className="grow">
        <span className="title">{thread.title || "未命名会话"}</span>
        <span className="thread-meta" aria-label={`${deviceName}，${projectName}`}>
          <span><IconDevice size={11} />{deviceName}</span>
          <span aria-hidden="true">·</span>
          <span><IconFolder size={11} />{projectName}</span>
          <span aria-hidden="true">·</span>
          <span className="num">{relTime(thread.lastActivityAt ?? thread.createdAt)}</span>
        </span>
      </span>
      <ProviderBadge provider={thread.provider} />
      <IconChevronRight className="row-chevron" size={16} />
    </button>
  );
}
