/* Projects: local project list + directory picker for adding workspaces. */
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Empty, IconFolder, IconPlus, Spinner } from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { ProjectRow, TopBar } from "../components";
import { DirectoryPicker } from "../sheets/DirectoryPicker";

export function ProjectsPage() {
  const navigate = useNavigate();
  const [pickerOpen, setPickerOpen] = useState(false);
  const projects = useQuery({ queryKey: ["projects"], queryFn: api.projects });

  return (
    <div className="page">
      <TopBar
        title="项目"
        trailing={
          <button className="icon-btn" onClick={() => setPickerOpen(true)} aria-label="添加项目">
            <IconPlus size={19} />
          </button>
        }
      />
      <div className="page-scroll">
        <div className="page-col">
          {projects.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 48 }}>
              <Spinner />
            </div>
          ) : (projects.data ?? []).length === 0 ? (
            <Empty
              icon={<IconFolder size={24} />}
              headline="还没有项目"
              hint="把本机的一个工作目录登记为项目，Codex 会话会在这个目录中执行。"
              action={
                <button className="btn primary" onClick={() => setPickerOpen(true)}>
                  <IconPlus size={15} />
                  添加项目
                </button>
              }
            />
          ) : (
            <>
              <div className="section-label micro">本机项目 · {projects.data?.length}</div>
              <div className="list-group">
                {(projects.data ?? []).map((p) => (
                  <ProjectRow
                    key={p.id}
                    project={p}
                    onClick={() => navigate({ name: "project", projectId: p.id })}
                  />
                ))}
              </div>
            </>
          )}
        </div>
      </div>
      <DirectoryPicker open={pickerOpen} onClose={() => setPickerOpen(false)} />
    </div>
  );
}
