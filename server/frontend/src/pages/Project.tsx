/* Project page: its threads, newest first, plus new-thread composer entry. */
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Empty,
  IconChat,
  IconPlus,
  Sheet,
  Spinner,
  newIdemKey,
  useToast,
} from "@nuntius/shared";
import { api, ApiError } from "../api";
import { useNavigate } from "../hooks";
import { useRoute } from "../stores";
import { trackCommand } from "../events";
import { ConnIndicator, ThreadRow, TopBar } from "../components";

export function ProjectPage({ deviceId, projectId }: { deviceId: string; projectId: string }) {
  const navigate = useNavigate();
  const back = useRoute((s) => s.back);
  const toast = useToast();
  const qc = useQueryClient();
  const [creating, setCreating] = useState(false);
  const [firstMessage, setFirstMessage] = useState("");
  const [busy, setBusy] = useState(false);

  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projects = useQuery({
    queryKey: ["projects", deviceId],
    queryFn: () => api.projects(deviceId),
  });
  const threads = useQuery({
    queryKey: ["projectThreads", deviceId, projectId],
    queryFn: () => api.projectThreads(deviceId, projectId),
  });

  const device = devices.data?.find((d) => d.id === deviceId);
  const project = projects.data?.find((p) => p.id === projectId);
  const unassigned = project?.kind === "system_unassigned";
  const canCreate = device?.status === "online" && !unassigned;
  const sorted = [...(threads.data ?? [])].sort(
    (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
  );

  const create = async () => {
    const text = firstMessage.trim();
    if (busy) return;
    setBusy(true);
    const idemKey = newIdemKey();
    try {
      const receipt = await api.createThread(deviceId, projectId, null, text || null, idemKey);
      trackCommand(qc, receipt.commandId, undefined, "thread.create");
      toast("新会话创建中");
      setCreating(false);
      setFirstMessage("");
      await qc.invalidateQueries({ queryKey: ["projectThreads", deviceId, projectId] });
    } catch (e) {
      toast(
        e instanceof ApiError && e.code === "device_offline" ? "设备离线，无法创建会话" : "创建失败，请重试",
        { error: true },
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page">
      <TopBar
        title={project?.displayName ?? "项目"}
        subtitle={device ? `${device.displayName}${project?.branch ? ` · ${project.branch}${project.isDirty ? "*" : ""}` : ""}` : undefined}
        onBack={() => back({ name: "device", deviceId })}
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
          {unassigned ? (
            <div className="notice-banner info">
              这里是「未归类」会话：无法安全映射到工作目录的历史会话。可以阅读记录；回到这台电脑的本地控制台把它关联到真实项目后，才能继续对话。
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
              hint={canCreate ? "发起第一个对话，让这台电脑开始工作。" : "设备离线时不能创建新会话。"}
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
                  onClick={() =>
                    navigate({ name: "thread", deviceId, projectId, threadId: t.id })
                  }
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
