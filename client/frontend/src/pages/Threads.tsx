/* All local threads, newest activity first. */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconClock,
  IconRefresh,
  Spinner,
  SwipeActionRow,
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
    () =>
      [...(threads.data ?? [])].sort(
        (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
      ),
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
                  icon={t.archived ? <IconRefresh size={18} /> : <IconArchive size={18} />}
                  label={t.archived ? "恢复" : "归档"}
                  busy={busyIds.has(t.id)}
                  onAction={() => archive(t.id, !t.archived)}
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
