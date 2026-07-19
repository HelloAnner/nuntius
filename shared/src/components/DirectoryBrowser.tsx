import { useEffect, useMemo, useRef, useState } from "react";
import type { DirectoryEntry, DirectoryListResponse } from "../types";
import {
  IconCheck,
  IconChevronLeft,
  IconChevronRight,
  IconFolder,
  IconGit,
  IconLink,
  IconRefresh,
  IconSearch,
} from "./icons";
import { Spinner } from "./ui";

const MAX_PAGES = 100;

export function DirectoryBrowser({
  loadRoots,
  loadDirectory,
  onCreate,
  emptyHint,
}: {
  loadRoots: () => Promise<DirectoryListResponse>;
  loadDirectory: (parentRef: string, cursor?: string) => Promise<DirectoryListResponse>;
  onCreate: (entry: DirectoryEntry, name: string) => Promise<void>;
  emptyHint?: string;
}) {
  const [levels, setLevels] = useState<DirectoryEntry[]>([]);
  const [data, setData] = useState<DirectoryListResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<DirectoryEntry | null>(null);
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const requestId = useRef(0);

  const loadEntry = async (entry: DirectoryEntry, nextLevels: DirectoryEntry[]) => {
    const id = ++requestId.current;
    setLevels(nextLevels);
    setFilter("");
    setLoading(true);
    setError(null);
    try {
      let result = await loadDirectory(entry.directoryRef);
      const entries = [...result.entries];
      const seen = new Set<string>();
      let pages = 1;
      while (result.nextCursor && pages < MAX_PAGES && !seen.has(result.nextCursor)) {
        seen.add(result.nextCursor);
        result = await loadDirectory(entry.directoryRef, result.nextCursor);
        entries.push(...result.entries);
        pages += 1;
      }
      if (result.nextCursor) throw new Error("目录内容过多，未能完整加载");
      if (requestId.current === id) setData({ ...result, entries });
    } catch (cause) {
      if (requestId.current === id) {
        setData(null);
        setError(cause instanceof Error ? cause.message : "目录加载失败，请重试");
      }
    } finally {
      if (requestId.current === id) setLoading(false);
    }
  };

  const loadRootEntries = async () => {
    const id = ++requestId.current;
    setLevels([]);
    setFilter("");
    setLoading(true);
    setError(null);
    try {
      const result = await loadRoots();
      if (requestId.current !== id) return;
      if (result.entries.length === 1) {
        await loadEntry(result.entries[0], [result.entries[0]]);
      } else {
        setData(result);
      }
    } catch (cause) {
      if (requestId.current === id) {
        setData(null);
        setError(cause instanceof Error ? cause.message : "目录加载失败，请重试");
      }
    } finally {
      if (requestId.current === id) setLoading(false);
    }
  };

  useEffect(() => {
    void loadRootEntries();
    return () => {
      requestId.current += 1;
    };
    // These callbacks are intentionally captured once for the lifetime of the sheet.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const visibleEntries = useMemo(() => {
    const query = filter.trim().toLocaleLowerCase();
    if (!query) return data?.entries ?? [];
    return (data?.entries ?? []).filter((entry) =>
      entry.name.toLocaleLowerCase().includes(query),
    );
  }, [data, filter]);

  const current = levels.at(-1) ?? null;
  const chooseCurrent = () => {
    if (!current?.selectable || current.projectId) return;
    setSelected(current);
    setName(current.name);
    setCreateError(null);
  };
  const create = async () => {
    if (!selected || creating) return;
    setCreating(true);
    setCreateError(null);
    try {
      await onCreate(selected, name.trim() || selected.name);
    } catch (cause) {
      setCreateError(cause instanceof Error ? cause.message : "创建失败，请重试");
    } finally {
      setCreating(false);
    }
  };

  if (selected) {
    return (
      <div className="directory-confirm">
        <button className="directory-back" type="button" onClick={() => setSelected(null)}>
          <IconChevronLeft size={17} />
          返回目录
        </button>
        <div className="directory-confirm-card">
          <span className="directory-confirm-glyph">
            {selected.gitKind ? <IconGit size={22} /> : <IconFolder size={22} />}
          </span>
          <div>
            <div className="directory-confirm-title">{selected.name}</div>
            <div className="directory-path">{selected.breadcrumb.join(" / ")}</div>
          </div>
        </div>
        <p className="directory-confirm-copy">只登记这个目录，不会移动或修改其中的文件。</p>
        <div className="field">
          <label htmlFor="project-name">项目名称</label>
          <input
            id="project-name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={selected.name}
            autoFocus
          />
        </div>
        {createError ? <div className="directory-inline-error">{createError}</div> : null}
        <button className="btn primary block directory-create" onClick={() => void create()} disabled={creating}>
          {creating ? <Spinner sm /> : <IconCheck size={17} />}
          创建项目
        </button>
      </div>
    );
  }

  return (
    <div className="directory-browser">
      <div className="directory-toolbar">
        <div className="directory-crumbs" aria-label="当前目录">
          <button
            type="button"
            onClick={() => void loadRootEntries()}
            disabled={levels.length === 0}
          >
            用户目录
          </button>
          {levels.map((entry, index) => (
            <span key={`${entry.directoryRef}-${index}`}>
              <IconChevronRight size={13} />
              <button
                type="button"
                disabled={index === levels.length - 1}
                onClick={() => void loadEntry(entry, levels.slice(0, index + 1))}
              >
                {entry.name}
              </button>
            </span>
          ))}
        </div>
        <label className="directory-search">
          <IconSearch size={17} />
          <input
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            placeholder="搜索当前目录"
            aria-label="搜索当前目录"
          />
        </label>
      </div>

      <div className="directory-list">
        {loading ? (
          <div className="directory-state"><Spinner /></div>
        ) : error ? (
          <div className="directory-state">
            <p>{error}</p>
            <button className="btn ghost sm" onClick={() => void (current ? loadEntry(current, levels) : loadRootEntries())}>
              <IconRefresh size={15} />重试
            </button>
          </div>
        ) : visibleEntries.length === 0 ? (
          <div className="directory-state">
            <p>{filter ? "没有匹配的文件夹" : (emptyHint ?? "这个目录中没有子文件夹")}</p>
          </div>
        ) : (
          visibleEntries.map((entry) => (
            <button
              key={entry.directoryRef}
              className="directory-row"
              type="button"
              onClick={() => void loadEntry(entry, [...levels, entry])}
            >
              <span className={`directory-row-glyph${entry.symlink ? " linked" : ""}`}>
                {entry.gitKind ? <IconGit size={18} /> : <IconFolder size={18} />}
              </span>
              <span className="directory-row-copy">
                <span className="directory-row-name">{entry.name}</span>
                <span className="directory-row-meta">
                  {entry.symlink ? <><IconLink size={12} />软链</> : null}
                  {entry.gitKind ? <span>Git 仓库</span> : null}
                  {entry.projectId ? <span>已添加为项目</span> : null}
                  {!entry.hasChildren && !entry.gitKind && !entry.projectId && !entry.symlink ? <span>空文件夹</span> : null}
                </span>
              </span>
              <IconChevronRight className="directory-row-chevron" size={18} />
            </button>
          ))
        )}
      </div>

      <div className="directory-footer">
        <div className="directory-current">
          <span>当前目录</span>
          <strong>{current?.name ?? "请选择一个用户目录"}</strong>
        </div>
        <button
          className="btn primary"
          type="button"
          onClick={chooseCurrent}
          disabled={!current?.selectable || Boolean(current?.projectId) || loading}
        >
          <IconCheck size={17} />
          {current?.projectId ? "已添加" : "选择这里"}
        </button>
      </div>
    </div>
  );
}
