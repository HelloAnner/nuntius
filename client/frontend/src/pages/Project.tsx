/* Project page: threads of one local project + new-thread entry. */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Empty, IconChat, IconPlus, Sheet, Spinner, useToast } from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { useRoute } from "../stores";
import { ConnIndicator, ThreadRow, TopBar } from "../components";

export function ProjectPage({ projectId }: { projectId: string }) {
  const navigate = useNavigate();
  const back = useRoute((s) => s.back);
  const toast = useToast();
  const qc = useQueryClient();
  const [creating, setCreating] = useState(false);
  const [firstMessage, setFirstMessage] = useState("");
  const [busy, setBusy] = useState(false);

  const info = useQuery({ queryKey: ["info"], queryFn: api.info });
  const projects = useQuery({ queryKey: ["projects"], queryFn: api.projects });
  const threads = useQuery({
    queryKey: ["projectThreads", projectId],
    queryFn: () => api.projectThreads(projectId),
  });

  const project = projects.data?.find((p) => p.id === projectId);

  useEffect(() => {
    if (projects.isError || (projects.isSuccess && !project)) {
      navigate({ name: "overview" }, { replace: true });
    }
  }, [navigate, project, projects.isError, projects.isSuccess]);

  const canCreate = info.data?.appServerRunning ?? false;
  const sorted = [...(threads.data ?? [])].sort(
    (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
  );

  const create = async () => {
    const text = firstMessage.trim();
    if (busy) return;
    setBusy(true);
    try {
      const result = await api.createThread(projectId, text || null);
      setCreating(false);
      setFirstMessage("");
      void qc.invalidateQueries({ queryKey: ["projectThreads", projectId] });
      void qc.invalidateQueries({ queryKey: ["threads"] });
      navigate({ name: "thread", projectId, threadId: result.threadId });
    } catch (e) {
      toast(e instanceof Error ? e.message : "创建失败", { error: true });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page">
      <TopBar
        title={project?.displayName ?? "项目"}
        subtitle={project?.branch ? `${project.branch}${project.isDirty ? "*" : ""}` : undefined}
        onBack={() => back({ name: "projects" })}
        trailing={
          canCreate ? (
            <button className="icon-btn" onClick={() => setCreating(true)} aria-label="新建会话">
              <IconPlus size={19} />
            </button>
          ) : (
            <ConnIndicator />
          )
        }
      />
      <div className="page-scroll">
        <div className="page-col">
          {!canCreate && !info.isLoading ? (
            <div className="notice-banner warn">
              Codex App Server 未运行，暂时不能创建或继续会话。
            </div>
          ) : null}
          {threads.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 48 }}>
              <Spinner />
            </div>
          ) : sorted.length === 0 ? (
            <Empty
              icon={<IconChat size={24} />}
              headline="还没有会话"
              hint={canCreate ? "发起第一个对话，让 Codex 在这个项目里开始工作。" : "App Server 未运行时不能创建会话。"}
              action={
                canCreate ? (
                  <button className="btn primary" onClick={() => setCreating(true)}>
                    <IconPlus size={15} />
                    新建会话
                  </button>
                ) : undefined
              }
            />
          ) : (
            <div className="list-group">
              {sorted.map((t) => (
                <ThreadRow
                  key={t.id}
                  thread={t}
                  onClick={() => navigate({ name: "thread", projectId, threadId: t.id })}
                />
              ))}
            </div>
          )}
        </div>
      </div>

      <Sheet open={creating} onClose={() => setCreating(false)} title="新建会话">
        <div style={{ padding: 20, display: "flex", flexDirection: "column", gap: 16 }}>
          <div className="field">
            <label htmlFor="first-msg">第一条消息（可选）</label>
            <textarea
              id="first-msg"
              rows={4}
              style={{ resize: "vertical", minHeight: 96 }}
              placeholder="描述一下想让 Codex 做什么…"
              value={firstMessage}
              onChange={(e) => setFirstMessage(e.target.value)}
            />
          </div>
          <button className="btn primary block" onClick={create} disabled={busy}>
            {busy ? <Spinner sm /> : null}
            创建会话
          </button>
        </div>
      </Sheet>
    </div>
  );
}
