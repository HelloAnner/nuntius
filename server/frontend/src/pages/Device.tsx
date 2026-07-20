/* Device detail: status hero + project list + remote directory picker. */
import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconEdit,
  IconFolder,
  IconPlus,
  Spinner,
  osLabel,
  relTime,
  statusLabel,
} from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { useRoute } from "../stores";
import { ConnIndicator, ProjectRow, TopBar } from "../components";
import { DirectoryPicker } from "../sheets/DirectoryPicker";
import { RenameDeviceSheet } from "../sheets/RenameDeviceSheet";

export function DevicePage({ deviceId }: { deviceId: string }) {
  const navigate = useNavigate();
  const back = useRoute((s) => s.back);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [renameOpen, setRenameOpen] = useState(false);
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
    <div className="page device-detail-page">
      <TopBar
        title={device ? <>{device.displayName}<IconEdit size={13} /></> : "设备"}
        subtitle={device
          ? device.status === "online"
            ? osLabel(device.osFamily, device.architecture)
            : `${statusLabel(device.status)}${device.lastSeenAt ? ` · ${relTime(device.lastSeenAt)}在线` : ""}`
          : undefined}
        onBack={() => back({ name: "devices" })}
        onTitleClick={device && device.status !== "revoked" ? () => setRenameOpen(true) : undefined}
        trailing={<ConnIndicator />}
      />
      <div className="page-scroll">
        <div className="page-col console-page-col detail-page-col">
          {device ? (
            <>
              {device.historyCompleteness === "backfilling" ? (
                <div className="sync-note" role="status">
                  <span className="row-state-spinner" aria-hidden="true" />
                  历史同步中
                </div>
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
                <div className="content-state compact">
                  <Spinner />
                </div>
              ) : (projects.data ?? []).length === 0 ? (
                <Empty
                  icon={<IconFolder size={24} />}
                  headline="还没有项目"
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
            <div className="content-state">
              <Spinner />
            </div>
          )}
        </div>
      </div>
      <DirectoryPicker deviceId={deviceId} open={pickerOpen} onClose={() => setPickerOpen(false)} />
      <RenameDeviceSheet device={device ?? null} open={renameOpen} onClose={() => setRenameOpen(false)} />
    </div>
  );
}
