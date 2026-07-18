/* Recents: every synced thread across all devices, filterable. */
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Empty, IconClock, Segmented, Spinner } from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { ConnIndicator, ThreadRow, TopBar } from "../components";

export function RecentsPage() {
  const navigate = useNavigate();
  const [filter, setFilter] = useState<string>("all");
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });

  const deviceName = (id: string) => devices.data?.find((d) => d.id === id)?.displayName ?? "";

  const options = useMemo(
    () => [
      { value: "all", label: "全部" },
      ...(devices.data ?? []).map((d) => ({ value: d.id, label: d.displayName })),
    ],
    [devices.data],
  );

  const list = useMemo(() => {
    const sorted = [...(threads.data ?? [])].sort(
      (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
    );
    return filter === "all" ? sorted : sorted.filter((t) => t.deviceId === filter);
  }, [threads.data, filter]);

  return (
    <div className="page">
      <TopBar title="最近会话" trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col">
          {options.length > 2 ? (
            <div style={{ marginBottom: 14 }}>
              <Segmented options={options} value={filter} onChange={setFilter} />
            </div>
          ) : null}
          {threads.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 48 }}>
              <Spinner />
            </div>
          ) : list.length === 0 ? (
            <Empty
              icon={<IconClock size={24} />}
              headline="还没有会话记录"
              hint="设备同步上来的会话会按最近活动聚合在这里，设备离线也能阅读。"
            />
          ) : (
            <div className="list-group">
              {list.map((t) => (
                <ThreadRow
                  key={t.id}
                  thread={t}
                  context={deviceName(t.deviceId)}
                  onClick={() =>
                    navigate({
                      name: "thread",
                      deviceId: t.deviceId,
                      projectId: t.projectId,
                      threadId: t.id,
                    })
                  }
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
