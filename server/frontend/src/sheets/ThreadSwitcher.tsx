/* Fast thread switcher: search + recent threads across every device. */
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  IconChat,
  IconSearch,
  Sheet,
  relTime,
  statusLabel,
} from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";

export function ThreadSwitcher({
  open,
  onClose,
  currentThreadId,
}: {
  open: boolean;
  onClose: () => void;
  currentThreadId?: string;
}) {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });

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
              <button
                key={t.id}
                className="list-row"
                style={t.id === currentThreadId ? { background: "var(--accent-soft)" } : undefined}
                onClick={() => {
                  navigate({
                    name: "thread",
                    deviceId: t.deviceId,
                    projectId: t.projectId,
                    threadId: t.id,
                  });
                  onClose();
                }}
              >
                <span className={`row-glyph thread${t.status === "active" ? " live" : ""}`}>
                  <IconChat size={15} />
                </span>
                <div className="grow">
                  <div className="title" style={{ fontSize: 14.5 }}>{t.title || "未命名会话"}</div>
                  <div className="sub">
                    <span>{statusLabel(t.status)}</span>
                    <span>·</span>
                    <span className="num">{relTime(t.lastActivityAt)}</span>
                  </div>
                </div>
              </button>
            ))}
          </div>
        ))
      )}
    </Sheet>
  );
}
