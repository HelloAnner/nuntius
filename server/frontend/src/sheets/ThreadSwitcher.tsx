/* Fast thread switcher: search + recent threads across every device. */
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { IconArchive, IconSearch, Sheet, SwipeActionRow } from "@nuntius/shared";
import { api } from "../api";
import {
  projectNameFrom,
  useArchiveThreadAction,
  useNavigate,
  useProjectNameMap,
} from "../hooks";
import { ThreadRow } from "../components";

export function ThreadSwitcher({
  open,
  onClose,
  currentThreadId,
  navigationContext = "project",
}: {
  open: boolean;
  onClose: () => void;
  currentThreadId?: string;
  navigationContext?: "project" | "recents";
}) {
  const navigate = useNavigate();
  const { archive, busyIds } = useArchiveThreadAction();
  const [query, setQuery] = useState("");
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projectNames = useProjectNameMap((devices.data ?? []).map((device) => device.id));

  const deviceName = (id: string) => devices.data?.find((d) => d.id === id)?.displayName ?? "设备";

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const list = [...(threads.data ?? [])].sort(
      (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
    );
    if (!q) return list.slice(0, 60);
    return list.filter((t) => t.title.toLowerCase().includes(q)).slice(0, 60);
  }, [threads.data, query]);

  const groups = useMemo(() => {
    const map = new Map<string, typeof filtered>();
    for (const t of filtered) {
      const key = t.deviceId;
      const arr = map.get(key) ?? [];
      arr.push(t);
      map.set(key, arr);
    }
    return [...map.entries()];
  }, [filtered]);

  return (
    <Sheet open={open} onClose={onClose} title="切换会话">
      <div className="switcher-search">
        <span className="search-icon">
          <IconSearch size={16} />
        </span>
        <input
          placeholder="搜索会话标题…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus={false}
        />
      </div>
      {groups.length === 0 ? (
        <div style={{ padding: 36, textAlign: "center", color: "var(--ink-3)", fontSize: 14 }}>
          没有匹配的会话
        </div>
      ) : (
        groups.map(([deviceId, list]) => (
          <div key={deviceId}>
            <div className="switch-group-label">{deviceName(deviceId)}</div>
            {list.map((t) => (
              <SwipeActionRow
                key={t.id}
                icon={<IconArchive size={18} />}
                label="归档"
                busy={busyIds.has(t.id)}
                onAction={() => archive(t.id)}
              >
                <ThreadRow
                  thread={t}
                  deviceName={deviceName(t.deviceId)}
                  projectName={projectNameFrom(projectNames, t.deviceId, t.projectId)}
                  selected={t.id === currentThreadId}
                  onClick={() => {
                    navigate(
                      navigationContext === "recents"
                        ? { name: "recentThread", threadId: t.id }
                        : {
                            name: "thread",
                            deviceId: t.deviceId,
                            projectId: t.projectId,
                            threadId: t.id,
                          },
                    );
                    onClose();
                  }}
                />
              </SwipeActionRow>
            ))}
          </div>
        ))
      )}
    </Sheet>
  );
}
