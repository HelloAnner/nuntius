/* Global project index, grouped by device as specified by the console design. */
import { useMemo, useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconDevice,
  IconFolder,
  IconPlus,
  Sheet,
  Spinner,
  statusLabel,
} from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { FilterSelect, ProjectRow, StatusDot, TopBar } from "../components";
import { DirectoryPicker } from "../sheets/DirectoryPicker";

export function ProjectsPage() {
  const navigate = useNavigate();
  const [deviceFilter, setDeviceFilter] = useState("all");
  const [devicePickerOpen, setDevicePickerOpen] = useState(false);
  const [directoryDeviceId, setDirectoryDeviceId] = useState<string | null>(null);
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const projectQueries = useQueries({
    queries: (devices.data ?? []).map((device) => ({
      queryKey: ["projects", device.id],
      queryFn: () => api.projects(device.id),
    })),
  });

  const groups = useMemo(
    () =>
      (devices.data ?? [])
        .map((device, index) => ({
          device,
          projects: [...(projectQueries[index]?.data ?? [])].sort(
            (a, b) => Date.parse(b.lastActivityAt ?? "") - Date.parse(a.lastActivityAt ?? ""),
          ),
        }))
        .filter(({ device }) => deviceFilter === "all" || device.id === deviceFilter),
    [deviceFilter, devices.data, projectQueries],
  );
  const projectCount = groups.reduce((total, group) => total + group.projects.length, 0);
  const loading = devices.isLoading || projectQueries.some((query) => query.isLoading);
  const onlineDevices = (devices.data ?? []).filter((device) => device.status === "online");

  const addProject = () => {
    const selected = devices.data?.find((device) => device.id === deviceFilter);
    if (selected?.status === "online") {
      setDirectoryDeviceId(selected.id);
      return;
    }
    setDevicePickerOpen(true);
  };

  return (
    <div className="page projects-page">
      <TopBar
        title="项目"
        subtitle={`${projectCount} 个项目 · 按设备分组，移除项目不会删除文件`}
        trailing={
          <div className="page-actions">
            <FilterSelect
              label="按设备筛选项目"
              value={deviceFilter}
              onChange={setDeviceFilter}
              options={[
                { value: "all", label: "全部设备" },
                ...(devices.data ?? []).map((device) => ({ value: device.id, label: device.displayName })),
              ]}
            />
            <button className="btn primary" onClick={addProject} disabled={onlineDevices.length === 0} aria-label="新建项目">
              <IconPlus size={16} />
              <span className="desktop-only">新建项目</span>
            </button>
          </div>
        }
      />
      <div className="page-scroll">
        <div className="page-col console-page-col">
          {loading ? (
            <div className="content-state"><Spinner /></div>
          ) : projectCount === 0 ? (
            <Empty
              icon={<IconFolder size={24} />}
              headline="还没有项目"
              hint={onlineDevices.length ? "从一台在线设备选择本地目录开始" : "设备在线后即可添加项目"}
              action={
                onlineDevices.length ? (
                  <button className="btn primary" onClick={addProject}><IconPlus size={15} />新建项目</button>
                ) : undefined
              }
            />
          ) : (
            <div className="project-groups">
              {groups.map(({ device, projects }) =>
                projects.length ? (
                  <section className="project-group" key={device.id}>
                    <header className="project-group-head">
                      <IconDevice size={15} />
                      <strong>{device.displayName}</strong>
                      <span className={`compact-status ${device.status}`}>
                        <StatusDot
                          tone={device.status === "online" ? "success" : device.status === "syncing" ? "warning" : "offline"}
                        />
                        {statusLabel(device.status)}
                      </span>
                      <span className="group-count num">{projects.length} 个项目</span>
                    </header>
                    <div className="list-group project-panel">
                      {projects.map((project) => (
                        <ProjectRow
                          key={project.id}
                          project={project}
                          onClick={() => navigate({ name: "project", deviceId: device.id, projectId: project.id })}
                        />
                      ))}
                    </div>
                  </section>
                ) : null,
              )}
            </div>
          )}
        </div>
      </div>

      <Sheet open={devicePickerOpen} onClose={() => setDevicePickerOpen(false)} title="选择设备">
        <div className="scope-picker">
          <p>项目目录位于设备本地，请先选择一台在线设备。</p>
          {onlineDevices.map((device) => (
            <button
              className="scope-option"
              key={device.id}
              onClick={() => {
                setDevicePickerOpen(false);
                setDirectoryDeviceId(device.id);
              }}
            >
              <span className="device-icon"><IconDevice size={19} /></span>
              <span><strong>{device.displayName}</strong><small>{device.projectCount} 个项目</small></span>
              <StatusDot tone="success" />
            </button>
          ))}
        </div>
      </Sheet>
      <DirectoryPicker
        deviceId={directoryDeviceId ?? ""}
        open={directoryDeviceId !== null}
        onClose={() => setDirectoryDeviceId(null)}
      />
    </div>
  );
}
