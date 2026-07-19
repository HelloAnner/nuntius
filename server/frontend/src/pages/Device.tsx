/* Device detail: status hero + project list + remote directory picker. */
import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Avatar,
  Empty,
  IconFolder,
  IconPlus,
  Pill,
  Spinner,
  completenessLabel,
  deviceTone,
  fullTime,
  initials,
  osLabel,
  statusLabel,
  tintIndex,
} from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { useRoute } from "../stores";
import { ConnIndicator, ProjectRow, TopBar } from "../components";
import { DirectoryPicker } from "../sheets/DirectoryPicker";

export function DevicePage({ deviceId }: { deviceId: string }) {
  const navigate = useNavigate();
  const back = useRoute((s) => s.back);
  const [pickerOpen, setPickerOpen] = useState(false);
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projects = useQuery({
    queryKey: ["projects", deviceId],
    queryFn: () => api.projects(deviceId),
  });
  const device = devices.data?.find((d) => d.id === deviceId);

  useEffect(() => {
    if (devices.isError || (devices.isSuccess && !device)) {
      navigate({ name: "devices" }, { replace: true });
    }
  }, [device, devices.isError, devices.isSuccess, navigate]);

  const online = device?.status === "online";

  return (
    <div className="page">
      <TopBar
        title={device?.displayName ?? "设备"}
        subtitle={device ? statusLabel(device.status) : undefined}
        onBack={() => back({ name: "devices" })}
        trailing={<ConnIndicator />}
      />
      <div className="page-scroll">
        <div className="page-col">
          {device ? (
            <>
              <div className="hero">
                <Avatar text={initials(device.displayName)} tint={tintIndex(device.id)} online={online} />
                <div className="meta">
                  <div className="name display">{device.displayName}</div>
                  <div className="facts">
                    <span>{osLabel(device.osFamily, device.architecture)}</span>
                    {device.codexVersion ? <span>Codex {device.codexVersion}</span> : null}
                    {device.agentVersion ? <span>CLI {device.agentVersion}</span> : null}
                  </div>
                </div>
                <Pill tone={deviceTone(device.status)} pulse={device.status === "syncing"}>
                  {statusLabel(device.status)}
                </Pill>
              </div>

              {!online ? (
                <div className="notice-banner warn">
                  设备当前{statusLabel(device.status)}
                  {device.lastSeenAt ? `，最后在线 ${fullTime(device.lastSeenAt)}` : ""}
                  。已同步的会话历史仍可阅读，但不能发起新的执行。
                </div>
              ) : null}
              {device.historyCompleteness === "backfilling" ? (
                <div className="notice-banner info">历史记录正在回填中（{completenessLabel(device.historyCompleteness)}），旧会话会逐步完整。</div>
              ) : null}

              <div className="section-label micro">
                <span>项目 · {projects.data?.length ?? 0}</span>
                {online ? (
                  <button className="btn quiet sm" onClick={() => setPickerOpen(true)}>
                    <IconPlus size={14} />
                    添加项目
                  </button>
                ) : null}
              </div>

              {projects.isLoading ? (
                <div style={{ display: "grid", placeItems: "center", padding: 40 }}>
                  <Spinner />
                </div>
              ) : (projects.data ?? []).length === 0 ? (
                <Empty
                  icon={<IconFolder size={24} />}
                  headline="还没有项目"
                  hint={
                    online
                      ? "浏览这台电脑上允许访问的目录，选一个作为工作区。"
                      : "设备离线时无法创建项目。"
                  }
                  action={
                    online ? (
                      <button className="btn primary" onClick={() => setPickerOpen(true)}>
                        <IconPlus size={15} />
                        浏览目录
                      </button>
                    ) : undefined
                  }
                />
              ) : (
                <div className="list-group">
                  {(projects.data ?? []).map((p) => (
                    <ProjectRow
                      key={p.id}
                      project={p}
                      onClick={() => navigate({ name: "project", deviceId, projectId: p.id })}
                    />
                  ))}
                </div>
              )}
            </>
          ) : (
            <div style={{ display: "grid", placeItems: "center", padding: 60 }}>
              <Spinner />
            </div>
          )}
        </div>
      </div>
      <DirectoryPicker deviceId={deviceId} open={pickerOpen} onClose={() => setPickerOpen(false)} />
    </div>
  );
}
