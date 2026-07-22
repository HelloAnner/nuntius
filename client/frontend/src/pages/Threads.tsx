/* All local threads, newest creation first. */
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconArchive,
  IconClock,
  Spinner,
  RenameThreadSheet,
  SwipeActionRow,
  compareThreadCreation,
  type ThreadSummary,
} from "@nuntius/shared";
import { api } from "../api";
import { useArchiveThreadAction, useRenameThreadAction } from "../hooks";
import { ConnIndicator, ThreadRowLink, TopBar } from "../components";

export function ThreadsPage() {
  const { archive, busyIds } = useArchiveThreadAction();
  const renameThread = useRenameThreadAction();
  const [renamingThread, setRenamingThread] = useState<ThreadSummary | null>(null);
  const threads = useQuery({ queryKey: ["threads"], queryFn: api.threads });
  const projects = useQuery({ queryKey: ["projects"], queryFn: api.projects });

  const projectName = (id: string) => projects.data?.find((p) => p.id === id)?.displayName ?? "";
  const canArchive = (thread: ThreadSummary) =>
    projects.data?.find((project) => project.id === thread.projectId)?.kind === "workspace";
  const list = useMemo(
    () => [...(threads.data ?? [])]
      .filter((thread) => !busyIds.has(thread.id))
      .sort(compareThreadCreation),
    [busyIds, threads.data],
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
                  disabled={!canArchive(t)}
                  onAction={() => archive(t.id)}
                >
                  <ThreadRowLink
                    thread={t}
                    context={projectName(t.projectId)}
                    onRename={() => setRenamingThread(t)}
                    onArchive={canArchive(t) ? () => archive(t.id) : undefined}
                  />
                </SwipeActionRow>
              ))}
            </div>
          )}
        </div>
      </div>
      <RenameThreadSheet
        thread={renamingThread}
        open={renamingThread !== null}
        onClose={() => setRenamingThread(null)}
        onRename={(title) => renamingThread ? renameThread(renamingThread, title) : Promise.resolve()}
      />
    </div>
  );
}
