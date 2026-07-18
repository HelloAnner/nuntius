/* All local threads, newest activity first. */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { Empty, IconClock, Spinner } from "@nuntius/shared";
import { api } from "../api";
import { ConnIndicator, ThreadRowLink, TopBar } from "../components";

export function ThreadsPage() {
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
              hint="进入一个项目，发起第一段对话。"
            />
          ) : (
            <div className="list-group">
              {list.map((t) => (
                <ThreadRowLink key={t.id} thread={t} context={projectName(t.projectId)} />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
