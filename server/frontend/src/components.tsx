/* Server console chrome and design-system primitives. */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  IconArchive,
  IconBook,
  IconChat,
  IconChevronLeft,
  IconChevronRight,
  IconClock,
  IconDevice,
  IconEdit,
  IconFolder,
  IconGit,
  IconMore,
  IconSearch,
  IconSettings,
  IconShield,
  SelectMenu,
  initials,
  osLabel,
  providerLabel,
  relTime,
  statusLabel,
  threadNeedsReview,
  threadPresentationStatus,
  truncateMiddle,
  type ConnState,
  type DeviceSummary,
  type ProjectSummary,
  type ThreadSummary,
} from "@nuntius/shared";
import { useNavigate } from "./hooks";
import { api } from "./api";
import { useSse } from "./events";
import { usePendingApprovalCount, useRoute, useSession, type Route } from "./stores";
import { fleetVersionState } from "./versioning";

export function ConnIndicator() {
  const status = useSse((state) => state.status);
  const navigate = useNavigate();
  const info = useQuery({
    queryKey: ["info"],
    queryFn: api.info,
    staleTime: 60_000,
  });
  const devices = useQuery({
    queryKey: ["devices"],
    queryFn: api.devices,
    refetchInterval: 15_000,
  });
  const map: Record<string, { state: ConnState; label: string }> = {
    live: { state: "live", label: "已连接" },
    connecting: { state: "busy", label: "连接中" },
    reconnecting: { state: "busy", label: "重连中" },
    syncing: { state: "busy", label: "同步中" },
  };
  const transport = map[status] ?? map.connecting;
  const versionState = fleetVersionState(info.data?.serverVersion, devices.data);
  const current =
    status === "live" && versionState === "mismatch"
      ? { state: "busy" as const, label: "版本不一致" }
      : status === "live" && versionState === "unknown"
        ? { state: "busy" as const, label: "版本待确认" }
        : transport;
  const versionWarning = status === "live" && versionState !== "compatible";
  const contents = (
    <>
      <span className={`connection-pulse ${current.state}${versionWarning ? " version-warning" : ""}`} aria-hidden="true">
        <span />
      </span>
      <span>{current.label}</span>
    </>
  );
  return versionWarning ? (
    <button
      className="console-connection version-warning"
      type="button"
      onClick={() => navigate({ name: "settings" })}
      aria-label={`${current.label}，打开设置查看详情`}
      title="打开设置查看 Client / Server 版本"
    >
      {contents}
    </button>
  ) : (
    <span className="console-connection" role="status" aria-label={current.label}>
      {contents}
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
  { route: { name: "learning" }, label: "学习", icon: (size) => <IconBook size={size} /> },
  { route: { name: "projects" }, label: "项目", icon: (size) => <IconFolder size={size} /> },
  { route: { name: "approvals" }, label: "审批", icon: (size) => <IconShield size={size} /> },
  { route: { name: "settings" }, label: "设置", icon: (size) => <IconSettings size={size} /> },
];

function tabActive(route: Route, tab: Route): boolean {
  if (tab.name === "settings") {
    return route.name === "settings" || route.name === "devices" || route.name === "device";
  }
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
  const openRecents = () => {
    if (route.name !== "recentThread") navigate({ name: "recents" });
  };
  return (
    <nav className="navrail" aria-label="主导航">
      <div className="rail-top">
        <button className="rail-brand" onClick={openRecents}>
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
                onClick={() => tab.route.name === "recents" ? openRecents() : navigate(tab.route)}
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
    <SelectMenu
      className="filter-select"
      value={value}
      onChange={onChange}
      options={options}
      label={label}
    />
  );
}

export type StatusTone = "active" | "warning" | "success" | "danger" | "idle" | "offline";

export function StatusDot({ tone, pulse = false }: { tone: StatusTone; pulse?: boolean }) {
  return <span className={`status-orbit ${tone}${pulse ? " pulse" : ""}`} aria-hidden="true"><span /></span>;
}

export function ProviderBadge({ provider }: { provider: ThreadSummary["provider"] }) {
  return (
    <span className={`provider-badge ${provider}`}>
      <span className="provider-mark">{{ codex: "C", kimi: "K", pi: "P" }[provider]}</span>
      {providerLabel(provider)}
    </span>
  );
}

function deviceTone(device: DeviceSummary): StatusTone {
  if (device.versionCompatibility === "mismatch") return "warning";
  if (device.status === "online") return "success";
  if (device.status === "syncing" || device.status === "pairing") return "warning";
  if (device.status === "degraded") return "danger";
  return "offline";
}

export function DeviceRow({ device }: { device: DeviceSummary }) {
  const navigate = useNavigate();
  const lastSeen = device.lastSeenAt ? relTime(device.lastSeenAt) : "刚刚";
  const footerText =
    device.status === "syncing" || device.historyCompleteness === "backfilling"
      ? "正在同步历史"
      : device.status === "online"
        ? `最后在线：${lastSeen}`
        : `最后在线：${lastSeen}`;
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
          {device.versionCompatibility === "mismatch" ? "版本不一致" : statusLabel(device.status)}
        </span>
      </div>
      <div className="device-stats">
        <span><strong className="num">{device.projectCount}</strong><small>项目</small></span>
        <span><strong className="num">{device.activeTurnCount}</strong><small>运行中</small></span>
        <span><strong className="num">{device.pendingApprovalCount}</strong><small>待审批</small></span>
      </div>
      <div className="device-card-foot">
        <span className={device.historyCompleteness === "error" ? "danger-copy" : ""}>
          {device.historyCompleteness === "error" ? "历史同步异常" : footerText}
        </span>
        <span className="device-card-actions">
          <button className="btn outline sm" onClick={() => navigate({ name: "device", deviceId: device.id })}>
            查看项目
          </button>
          <button className="device-more" onClick={() => navigate({ name: "settings" })} aria-label={`${device.displayName}设备操作`}>
            <IconMore size={18} />
          </button>
        </span>
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
        <span className="sub mono project-sub project-sub-desktop">
          {unassigned ? "无法映射目录的历史会话 · 只读" : truncateMiddle(project.pathHint ?? project.repoName ?? "本地项目", 46)}
        </span>
        <span className="sub mono project-sub project-sub-mobile">
          {unassigned
            ? "历史会话待归类 · 只读"
            : [
                truncateMiddle(project.pathHint ?? project.repoName ?? "本地项目", 28),
                project.branch ? `${project.branch}${project.isDirty ? "*" : ""}` : null,
                `${project.threadCount} 会话`,
              ].filter(Boolean).join(" · ")}
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

export function threadTone(thread: ThreadSummary): StatusTone {
  if (thread.status === "active") return "active";
  if (threadNeedsReview(thread)) return "warning";
  if (thread.status === "recovering") return "warning";
  if (["failed", "error", "rejected"].includes(thread.status)) return "danger";
  if (["completed", "idle"].includes(thread.status)) return "success";
  return thread.archived ? "offline" : "idle";
}

export function ThreadListItem({
  thread,
  pendingApproval = false,
  selected = false,
  contextDevice,
  contextProject,
  contextBelow = false,
  timestamp,
  onClick,
}: {
  thread: ThreadSummary;
  pendingApproval?: boolean;
  selected?: boolean;
  contextDevice?: string;
  contextProject?: string;
  contextBelow?: boolean;
  timestamp?: string | null;
  onClick: () => void;
}) {
  const needsReview = threadNeedsReview(thread);
  const tone = pendingApproval ? "warning" : threadTone(thread);
  const state = pendingApproval ? "等待审批" : statusLabel(threadPresentationStatus(thread));
  const context = contextDevice || contextProject ? (
    <span className="thread-list-context">
      {contextDevice ? <span className="ctx-device"><IconDevice size={10} />{contextDevice}</span> : null}
      {contextDevice && contextProject ? <span className="ctx-sep">·</span> : null}
      {contextProject ? <span className="ctx-project"><IconFolder size={10} />{contextProject}</span> : null}
    </span>
  ) : null;
  const relativeTime = relTime(timestamp === undefined ? thread.lastActivityAt ?? thread.createdAt : timestamp);
  return (
    <button
      className={`thread-list-item thread-${tone}${needsReview ? " needs-review" : ""}${selected ? " selected" : ""}`}
      onClick={onClick}
      aria-current={selected ? "page" : undefined}
    >
      <StatusDot tone={tone} pulse={tone === "active" || needsReview} />
      <span className="thread-list-copy">
        <span className="thread-list-title">{thread.title || "未命名会话"}</span>
        <span className={`thread-list-meta${contextBelow && context ? " context-below" : ""}`}>
          {contextBelow ? null : context}
          <span className="thread-list-state">
            <span className={needsReview ? "review-label" : undefined}>{state}</span>
            <span aria-hidden="true"> · </span>{providerLabel(thread.provider)} · {relativeTime}
          </span>
          {contextBelow ? context : null}
        </span>
      </span>
    </button>
  );
}

export function ThreadRow({
  thread,
  deviceName,
  projectName,
  selected = false,
  onClick,
  onRename,
  onArchive,
}: {
  thread: ThreadSummary;
  deviceName: string;
  projectName: string;
  selected?: boolean;
  onClick: () => void;
  onRename?: () => void;
  onArchive?: () => void;
}) {
  const needsReview = threadNeedsReview(thread);
  const tone = threadTone(thread);
  const hasActions = Boolean(onRename || onArchive);
  return (
    <div className={`thread-row-shell${hasActions ? " has-actions" : ""}`}>
      <button
        className={`list-row thread-row thread-${tone}${selected ? " selected" : ""}`}
        onClick={onClick}
        aria-current={selected ? "page" : undefined}
      >
        <StatusDot tone={tone} pulse={tone === "active" || needsReview} />
        <span className="grow">
          <span className="title">{thread.title || "未命名会话"}</span>
          <span className="thread-meta" aria-label={`${deviceName}，${projectName}`}>
            <span className="thread-device"><IconDevice size={11} />{deviceName}</span>
            <span aria-hidden="true">·</span>
            <span className="thread-project"><IconFolder size={11} />{projectName || "未归属项目"}</span>
            <span aria-hidden="true">·</span>
            {needsReview ? <><span className="review-label">待查看</span><span aria-hidden="true">·</span></> : null}
            <span className="thread-time num">{relTime(thread.lastActivityAt ?? thread.createdAt)}</span>
          </span>
        </span>
        <ProviderBadge provider={thread.provider} />
        {!hasActions ? <IconChevronRight className="row-chevron" size={16} /> : null}
      </button>
      <ThreadRowActions
        label={`“${thread.title || "未命名会话"}”的会话操作`}
        onRename={onRename}
        onArchive={onArchive}
      />
    </div>
  );
}

function ThreadRowActions({
  label,
  onRename,
  onArchive,
}: {
  label: string;
  onRename?: () => void;
  onArchive?: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [open]);
  if (!onRename && !onArchive) return null;
  return (
    <div ref={root} className="thread-row-actions">
      <button
        type="button"
        className="thread-row-more"
        onClick={() => setOpen((value) => !value)}
        aria-label={label}
        aria-expanded={open}
      >
        <IconMore size={17} />
      </button>
      {open ? (
        <div className="thread-row-menu" role="menu">
          {onRename ? (
            <button type="button" role="menuitem" onClick={() => { setOpen(false); onRename(); }}>
              <IconEdit size={14} />重命名
            </button>
          ) : null}
          {onArchive ? (
            <button type="button" role="menuitem" onClick={() => { setOpen(false); onArchive(); }}>
              <IconArchive size={14} />归档
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
