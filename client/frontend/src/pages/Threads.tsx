/* All local threads, newest activity first. */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconClock,
  Spinner,
  SwipeActionRow,
  compareByRecentActivity,
} from "@nuntius/shared";
import { api } from "../api";
import { useArchiveThreadAction } from "../hooks";
import { ConnIndicator, ThreadRowLink, TopBar } from "../components";

export function ThreadsPage() {
  const { archive, busyIds } = useArchiveThreadAction();
  const threads = useQuery({ queryKey: ["threads"], queryFn: api.threads });
  const projects = useQuery({ queryKey: ["projects"], queryFn: api.projects });

  const projectName = (id: string) => projects.data?.find((p) => p.id === id)?.displayName ?? "";
  const list = useMemo(
    () => [...(threads.data ?? [])].sort(compareByRecentActivity),
    [threads.data],
  );

  return (
    <div className="page">
      <TopBar title="会话" trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col">
          {threads.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 48 }}>
              <Spinner />
            </div>
          ) : list.length === 0 ? (
            <Empty
              icon={<IconClock size={24} />}
              headline="还没有会话"
            />
          ) : (
            <div className="list-group">
              {list.map((t) => (
                <SwipeActionRow
                  key={t.id}
                  icon={<IconArchive size={18} />}
                  label="归档"
                  busy={busyIds.has(t.id)}
                  onAction={() => archive(t.id)}
                >
                  <ThreadRowLink thread={t} context={projectName(t.projectId)} />
                </SwipeActionRow>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
