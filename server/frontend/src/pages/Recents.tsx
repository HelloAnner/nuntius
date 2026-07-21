/* Cross-device session index with the filters from the new console design. */
import { useCallback, useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconClock,
  IconPlus,
  IconSearch,
  Spinner,
  SwipeActionRow,
  compareThreadCreation,
  type ThreadSummary,
} from "@nuntius/shared";
import { api } from "../api";
import { projectNameFrom, useArchiveThreadAction, useNavigate, useProjectNameMap } from "../hooks";
import { useApprovals } from "../stores";
import { FilterSelect, SearchField, ThreadRow, TopBar } from "../components";
import {
  isRecentStatusFilter,
  loadRecentFilterPreferences,
  newThreadScopeFromRecentFilters,
  saveRecentFilterPreferences,
  type RecentFilterPreferences,
} from "../recentsFilters";
import { NewThreadSheet } from "../sheets/NewThreadSheet";

type SessionGroup = { key: string; label: string; threads: ThreadSummary[] };

export function RecentsPage() {
  const navigate = useNavigate();
  const { archive, busyIds } = useArchiveThreadAction();
  const approvals = useApprovals((state) => state.items);
  const [filterPreferences, setFilterPreferences] = useState(loadRecentFilterPreferences);
  const { deviceFilter, projectFilter, statusFilter } = filterPreferences;
  const [timeFilter, setTimeFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projectNames = useProjectNameMap((devices.data ?? []).map((device) => device.id));
  const newThreadScope = newThreadScopeFromRecentFilters(filterPreferences);

  const updateFilterPreferences = useCallback((update: Partial<RecentFilterPreferences>) => {
    setFilterPreferences((current) => {
      const next = { ...current, ...update };
      saveRecentFilterPreferences(next);
      return next;
    });
  }, []);

  const deviceName = (id: string) =>
    devices.data?.find((device) => device.id === id)?.displayName ?? "设备";
  const pendingThreadIds = useMemo(
    () => new Set(Object.values(approvals).filter((approval) => approval.state === "pending").map((approval) => approval.threadId)),
    [approvals],
  );

  const projectOptions = useMemo(() => {
    const seen = new Set<string>();
    return (threads.data ?? []).flatMap((thread) => {
      const value = `${thread.deviceId}:${thread.projectId}`;
      if (seen.has(value) || (deviceFilter !== "all" && thread.deviceId !== deviceFilter)) return [];
      seen.add(value);
      return [{ value, label: projectNameFrom(projectNames, thread.deviceId, thread.projectId) }];
    });
  }, [deviceFilter, projectNames, threads.data]);

  useEffect(() => {
    if (!devices.isSuccess || deviceFilter === "all") return;
    if (!devices.data.some((device) => device.id === deviceFilter)) {
      updateFilterPreferences({ deviceFilter: "all", projectFilter: "all" });
    }
  }, [deviceFilter, devices.data, devices.isSuccess, updateFilterPreferences]);

  useEffect(() => {
    if (!threads.isSuccess || projectFilter === "all") return;
    if (!projectOptions.some((option) => option.value === projectFilter)) {
      updateFilterPreferences({ projectFilter: "all" });
    }
  }, [projectFilter, projectOptions, threads.isSuccess, updateFilterPreferences]);

  const list = useMemo(() => {
    const now = Date.now();
    const q = query.trim().toLocaleLowerCase();
    return [...(threads.data ?? [])]
      .sort(compareThreadCreation)
      .filter((thread) => deviceFilter === "all" || thread.deviceId === deviceFilter)
      .filter((thread) => projectFilter === "all" || `${thread.deviceId}:${thread.projectId}` === projectFilter)
      .filter((thread) => {
        if (statusFilter === "running") return thread.status === "active";
        if (statusFilter === "approval") return pendingThreadIds.has(thread.id);
        if (statusFilter === "idle") return thread.status !== "active" && !pendingThreadIds.has(thread.id);
        if (statusFilter === "archived") return thread.archived;
        return true;
      })
      .filter((thread) => {
        if (timeFilter === "all") return true;
        const time = Date.parse(thread.lastActivityAt ?? thread.createdAt ?? "");
        const days = timeFilter === "today" ? 1 : timeFilter === "week" ? 7 : 30;
        return Number.isFinite(time) && now - time <= days * 86_400_000;
      })
      .filter((thread) => !q || thread.title.toLocaleLowerCase().includes(q));
  }, [deviceFilter, pendingThreadIds, projectFilter, query, statusFilter, threads.data, timeFilter]);

  const groups = useMemo(() => groupSessions(list), [list]);

  return (
    <div className="page recents-page">
      <TopBar
        title="最近会话"
        subtitle={
          <>
            <span className="desktop-only">全部设备的已同步会话，设备离线也可阅读历史</span>
            <span className="mobile-only">
              {(devices.data ?? []).length} 台设备 · {(devices.data ?? []).filter((device) => device.status === "online").length} 台在线
            </span>
          </>
        }
        trailing={
          <div className="page-actions">
            <div className="desktop-only"><SearchField value={query} onChange={setQuery} placeholder="搜索会话标题…" /></div>
            <button
              className="icon-btn mobile-only"
              onClick={() => setSearchOpen((value) => !value)}
              aria-label={searchOpen ? "收起搜索" : "搜索会话"}
              aria-pressed={searchOpen}
            >
              <IconSearch size={18} />
            </button>
            <button className="btn primary" onClick={() => setCreating(true)} aria-label="新建会话">
              <IconPlus size={16} />
              <span className="desktop-only">新建会话</span>
            </button>
          </div>
        }
      />
      <div className="page-scroll">
        <div className="page-col console-page-col">
          {searchOpen ? <div className="mobile-only mobile-search"><SearchField value={query} onChange={setQuery} placeholder="搜索会话标题…" /></div> : null}
          <div className="session-filters desktop-session-filters" aria-label="会话筛选">
            <div className="scope-filters">
              <FilterSelect
                label="设备"
                value={deviceFilter}
                onChange={(value) => updateFilterPreferences({ deviceFilter: value, projectFilter: "all" })}
                options={[
                  { value: "all", label: "全部设备" },
                  ...(devices.data ?? []).map((device) => ({ value: device.id, label: device.displayName })),
                ]}
              />
              <FilterSelect
                label="项目"
                value={projectFilter}
                onChange={(value) => updateFilterPreferences({ projectFilter: value })}
                options={[{ value: "all", label: "全部项目" }, ...projectOptions]}
              />
              <FilterSelect
                label="时间"
                value={timeFilter}
                onChange={setTimeFilter}
                options={[
                  { value: "all", label: "全部时间" },
                  { value: "today", label: "今天" },
                  { value: "week", label: "最近 7 天" },
                  { value: "month", label: "最近 30 天" },
                ]}
              />
            </div>
            <div className="status-filters" role="group" aria-label="状态">
              {[
                ["all", "全部"],
                ["running", "运行中"],
                ["approval", "等待审批"],
                ["idle", "空闲"],
                ["archived", "已归档"],
              ].map(([value, label]) => (
                <button
                  key={value}
                  className={statusFilter === value ? "on" : ""}
                  onClick={() => {
                    if (isRecentStatusFilter(value)) updateFilterPreferences({ statusFilter: value });
                  }}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <div className="mobile-only mobile-session-filters" aria-label="会话筛选">
            <FilterSelect
              label="设备"
              value={deviceFilter}
              onChange={(value) => updateFilterPreferences({ deviceFilter: value, projectFilter: "all" })}
              options={[
                { value: "all", label: "全部设备" },
                ...(devices.data ?? []).map((device) => ({ value: device.id, label: device.displayName })),
              ]}
            />
            <FilterSelect
              label="项目"
              value={projectFilter}
              onChange={(value) => updateFilterPreferences({ projectFilter: value })}
              options={[{ value: "all", label: "全部项目" }, ...projectOptions]}
            />
            <span className="accent-filter">
              <FilterSelect
                label="状态"
                value={statusFilter}
                onChange={(value) => {
                  if (isRecentStatusFilter(value)) updateFilterPreferences({ statusFilter: value });
                }}
                options={[
                  { value: "all", label: "状态：全部" },
                  { value: "running", label: "运行中" },
                  { value: "approval", label: "等待审批" },
                  { value: "idle", label: "空闲" },
                  { value: "archived", label: "已归档" },
                ]}
              />
            </span>
          </div>

          {threads.isLoading ? (
            <div className="content-state"><Spinner /></div>
          ) : list.length === 0 ? (
            <Empty
              icon={<IconClock size={24} />}
              headline="没有符合条件的会话"
              hint="调整筛选条件，或创建一个新会话"
              action={<button className="btn primary" onClick={() => setCreating(true)}><IconPlus size={15} />新建会话</button>}
            />
          ) : (
            <div className="session-list">
              {groups.map((group) => (
                <section className="session-group" key={group.key}>
                  <div className="session-group-label">
                    <span>{group.label}</span>
                    <span className="desktop-only"> · {group.threads.length}</span>
                  </div>
                  {group.threads.map((thread) => (
                    <SwipeActionRow
                      key={thread.id}
                      icon={<IconArchive size={18} />}
                      label="归档"
                      busy={busyIds.has(thread.id)}
                      onAction={() => archive(thread.id)}
                    >
                      <ThreadRow
                        thread={thread}
                        deviceName={deviceName(thread.deviceId)}
                        projectName={projectNameFrom(projectNames, thread.deviceId, thread.projectId)}
                        onClick={() => navigate({ name: "recentThread", threadId: thread.id })}
                      />
                    </SwipeActionRow>
                  ))}
                </section>
              ))}
            </div>
          )}
          {list.length ? <div className="list-footnote">已显示 {list.length} 条已同步会话</div> : null}
        </div>
      </div>
      <NewThreadSheet
        initialDeviceId={newThreadScope.deviceId}
        initialProjectId={newThreadScope.projectId}
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(threadId, deviceId, projectId) => navigate({ name: "thread", deviceId, projectId, threadId })}
      />
    </div>
  );
}

function groupSessions(threads: ThreadSummary[]): SessionGroup[] {
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterday = today - 86_400_000;
  const buckets: Record<string, ThreadSummary[]> = { today: [], yesterday: [], earlier: [] };
  for (const thread of threads) {
    const time = Date.parse(thread.lastActivityAt ?? thread.createdAt ?? "");
    const key = time >= today ? "today" : time >= yesterday ? "yesterday" : "earlier";
    buckets[key].push(thread);
  }
  return [
    { key: "today", label: "今天", threads: buckets.today },
    { key: "yesterday", label: "昨天", threads: buckets.yesterday },
    { key: "earlier", label: "更早", threads: buckets.earlier },
  ].filter((group) => group.threads.length);
}
