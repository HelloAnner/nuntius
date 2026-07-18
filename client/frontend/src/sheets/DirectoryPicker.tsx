/* Local directory picker: browse allowed roots and register a workspace. */
import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import {
  IconChevronRight,
  IconFolder,
  IconGit,
  Sheet,
  Spinner,
  useToast,
  type DirectoryEntry,
  type DirectoryListResponse,
} from "@nuntius/shared";
import { api } from "../api";

interface Level {
  parentRef: string | null;
  name: string;
}

export function DirectoryPicker({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const [path, setPath] = useState<Level[]>([{ parentRef: null, name: "允许的根目录" }]);
  const [data, setData] = useState<DirectoryListResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<DirectoryEntry | null>(null);
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);

  const current = path[path.length - 1];

  useEffect(() => {
    if (!open) {
      setPath([{ parentRef: null, name: "允许的根目录" }]);
      setSelected(null);
      setName("");
      setError(null);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    const run = async () => {
      try {
        const result =
          current.parentRef === null
            ? await api.directoryRoots()
            : await api.directories(current.parentRef);
        if (!cancelled) setData(result);
      } catch {
        if (!cancelled) {
          setError("目录加载失败，请重试");
          setData(null);
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [open, current]);

  const create = async () => {
    if (!selected || creating) return;
    setCreating(true);
    try {
      await api.createProject(selected.directoryRef, name.trim() || selected.name);
      toast("项目已创建");
      await qc.invalidateQueries({ queryKey: ["projects"] });
      await qc.invalidateQueries({ queryKey: ["info"] });
      onClose();
    } catch (e) {
      toast(e instanceof Error ? e.message : "创建失败", { error: true });
    } finally {
      setCreating(false);
    }
  };

  return (
    <Sheet open={open} onClose={onClose} title={selected ? "创建项目" : "选择目录"}>
      {!selected ? (
        <>
          <div className="dir-breadcrumb">
            {path.map((lvl, i) => (
              <span key={i} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
                {i > 0 ? <IconChevronRight size={12} /> : null}
                <button onClick={() => setPath(path.slice(0, i + 1))} disabled={i === path.length - 1}>
                  {lvl.name}
                </button>
              </span>
            ))}
          </div>
          {loading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 48 }}>
              <Spinner />
            </div>
          ) : error ? (
            <div style={{ padding: 32, textAlign: "center", color: "var(--ink-3)", fontSize: 14 }}>
              {error}
            </div>
          ) : (data?.entries ?? []).length === 0 ? (
            <div style={{ padding: 32, textAlign: "center", color: "var(--ink-3)", fontSize: 14 }}>
              这里暂时没有可进入的目录。允许的根目录可在 ~/.nuntius/config.toml 中配置。
            </div>
          ) : (
            (data?.entries ?? []).map((entry) => (
              <div key={entry.directoryRef} className={`dir-entry${entry.selectable ? "" : " off"}`}>
                <span className="row-glyph" style={{ width: 32, height: 32, borderRadius: 9 }}>
                  {entry.gitKind ? <IconGit size={15} /> : <IconFolder size={15} />}
                </span>
                <button
                  className="name"
                  style={{ background: "none", textAlign: "left" }}
                  onClick={() =>
                    entry.hasChildren
                      ? setPath([...path, { parentRef: entry.directoryRef, name: entry.name }])
                      : undefined
                  }
                >
                  {entry.name}
                </button>
                {entry.projectId ? <span className="pill">已是项目</span> : null}
                {entry.selectable && !entry.projectId ? (
                  <button
                    className="btn ghost sm"
                    onClick={() => {
                      setSelected(entry);
                      setName(entry.name);
                    }}
                  >
                    选择
                  </button>
                ) : null}
                {entry.hasChildren ? (
                  <button
                    className="icon-btn"
                    aria-label={`进入 ${entry.name}`}
                    onClick={() =>
                      setPath([...path, { parentRef: entry.directoryRef, name: entry.name }])
                    }
                  >
                    <IconChevronRight size={17} />
                  </button>
                ) : null}
              </div>
            ))
          )}
        </>
      ) : (
        <div style={{ padding: 20, display: "flex", flexDirection: "column", gap: 16 }}>
          <div className="notice-banner info" style={{ margin: 0 }}>
            将把 <span className="mono">{selected.breadcrumb.join("/") || selected.name}</span>{" "}
            登记为本机项目。不会移动或修改任何文件。
          </div>
          <div className="field">
            <label htmlFor="project-name">项目名称</label>
            <input
              id="project-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={selected.name}
            />
          </div>
          <div style={{ display: "flex", gap: 10 }}>
            <button className="btn ghost block" onClick={() => setSelected(null)}>
              重选目录
            </button>
            <button className="btn primary block" onClick={create} disabled={creating}>
              {creating ? <Spinner sm /> : null}
              创建项目
            </button>
          </div>
        </div>
      )}
    </Sheet>
  );
}
