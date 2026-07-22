/* App chrome: top bar, tabs, nav rail, list rows. */
import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  ConnPill,
  IconChat,
  IconArchive,
  IconChevronLeft,
  IconChevronRight,
  IconClock,
  IconDevice,
  IconEdit,
  IconFolder,
  IconGit,
  IconMore,
  IconShield,
  isRunningStatus,
  relTime,
  providerLabel,
  statusLabel,
  threadNeedsReview,
  threadPresentationStatus,
  truncateMiddle,
  type ConnState,
  type ProjectSummary,
  type ThreadSummary,
} from "@nuntius/shared";
import { usePendingApprovalCount, useRoute, type Route } from "./stores";
import { useSse } from "./events";
import { useNavigate } from "./hooks";

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
  { route: { name: "overview" }, label: "概览", icon: (s) => <IconDevice size={s} /> },
  { route: { name: "projects" }, label: "项目", icon: (s) => <IconFolder size={s} /> },
  { route: { name: "threads" }, label: "会话", icon: (s) => <IconClock size={s} /> },
  { route: { name: "approvals" }, label: "审批", icon: (s) => <IconShield size={s} /> },
];

function tabActive(route: Route, tab: Route): boolean {
  if (tab.name === "projects") {
    return route.name === "projects" || route.name === "project" || route.name === "thread";
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

export function ProjectRow({
  project,
  onClick,
}: {
  project: ProjectSummary;
  onClick: () => void;
}) {
  return (
    <button className="list-row" onClick={onClick}>
      <span className="row-glyph">
        {project.repoName ? <IconGit size={17} /> : <IconFolder size={17} />}
      </span>
      <div className="grow">
        <div className="title">{project.displayName}</div>
        <div className="sub">
          {project.branch ? (
            <span className="mono ellipsis">
              {project.branch}
              {project.isDirty ? "*" : ""}
            </span>
          ) : null}
          {project.pathHint ? (
            <span className="ellipsis">{truncateMiddle(project.pathHint, 36)}</span>
          ) : null}
          {!project.branch && !project.pathHint ? <span>{project.threadCount} 个会话</span> : null}
        </div>
      </div>
      <div className="trailing">
        <span className="num" style={{ fontSize: 12 }}>
          {relTime(project.lastActivityAt)}
        </span>
        <IconChevronRight size={16} />
      </div>
    </button>
  );
}

export function ThreadRow({
  thread,
  context,
  onClick,
  onRename,
  onArchive,
}: {
  thread: ThreadSummary;
  context?: string;
  onClick: () => void;
  onRename?: () => void;
  onArchive?: () => void;
}) {
  const active = isRunningStatus(thread.status);
  const needsReview = threadNeedsReview(thread);
  const presentationStatus = threadPresentationStatus(thread) ?? thread.status;
  const secondaryStatus =
    !thread.archived && !["active", "completed", "idle"].includes(presentationStatus)
      ? statusLabel(presentationStatus)
      : null;
  const details = [context, providerLabel(thread.provider), thread.archived ? "已归档" : secondaryStatus].filter(Boolean) as string[];
  const hasActions = Boolean(onRename || onArchive);
  return (
    <div className={`thread-row-shell${hasActions ? " has-actions" : ""}`}>
      <button className="list-row" onClick={onClick}>
        <span className={`row-glyph thread${thread.archived ? " muted" : ""}`}>
          <IconChat size={16} />
        </span>
        <div className="grow">
          <div className="title" style={thread.archived ? { color: "var(--ink-3)" } : undefined}>
            {thread.title || "未命名会话"}
          </div>
          {details.length ? (
            <div className="sub">
              {details.map((detail) => (
                <span
                  className={`ellipsis${needsReview && detail === secondaryStatus ? " review-label" : ""}`}
                  key={detail}
                >
                  {detail}
                </span>
              ))}
            </div>
          ) : null}
        </div>
        <div className="trailing">
          {active ? <span className="live-dot" aria-label="进行中" /> : null}
          {needsReview ? <span className="live-dot review" aria-label="待查看" /> : null}
          <span className="num" style={{ fontSize: 12 }}>
            {relTime(thread.lastActivityAt)}
          </span>
          {!hasActions ? <IconChevronRight size={16} /> : null}
        </div>
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

export function ThreadRowLink({
  thread,
  context,
  onRename,
  onArchive,
}: {
  thread: ThreadSummary;
  context?: string;
  onRename?: () => void;
  onArchive?: () => void;
}) {
  const navigate = useNavigate();
  return (
    <ThreadRow
      thread={thread}
      context={context}
      onRename={onRename}
      onArchive={onArchive}
      onClick={() =>
        navigate({ name: "thread", projectId: thread.projectId, threadId: thread.id })
      }
    />
  );
}
