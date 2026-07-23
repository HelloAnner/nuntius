/* Learning library: searchable, paged Markdown snapshots saved from AI replies. */
import { useEffect, useState, type CSSProperties } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import {
  Empty,
  IconBook,
  IconChevronLeft,
  IconChevronRight,
  IconSearch,
  IconX,
  Markdown,
  fullTime,
  type SavedItemView,
} from "@nuntius/shared";
import { api } from "../api";
import { ConnIndicator, TopBar } from "../components";
import { useNavigate } from "../hooks";

const PAGE_SIZE = 8;

export function LearningPage() {
  const navigate = useNavigate();
  const [search, setSearch] = useState("");
  const [page, setPage] = useState(0);
  const debouncedSearch = useDebouncedValue(search, 280);
  const savedItems = useQuery({
    queryKey: ["savedItems", debouncedSearch, page],
    queryFn: () => api.savedItems(debouncedSearch, PAGE_SIZE, page * PAGE_SIZE),
    placeholderData: keepPreviousData,
  });
  const total = savedItems.data?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));

  useEffect(() => {
    if (page >= totalPages) setPage(totalPages - 1);
  }, [page, totalPages]);

  const updateSearch = (value: string) => {
    setSearch(value);
    setPage(0);
  };

  return (
    <div className="page learning-page">
      <TopBar
        title="学习"
        subtitle="把值得重读的 AI 回答，留在自己的知识书架"
        trailing={<ConnIndicator />}
      />
      <div className="page-scroll">
        <div className="page-col console-page-col learning-page-col">
          <section className="learning-hero">
            <div className="learning-hero-copy">
              <span className="learning-kicker"><IconBook size={14} /> SAVED KNOWLEDGE</span>
              <h1>从对话里，留下真正有用的部分。</h1>
              <p>这里保存的是回答当时的完整 Markdown 快照，可以随时回来阅读、搜索与复习。</p>
            </div>
            <div className="learning-count" aria-label={`共 ${total} 篇保存内容`}>
              <strong className="num">{savedItems.isLoading ? "—" : total}</strong>
              <span>篇收藏</span>
            </div>
            <label className="learning-search">
              <span className="sr-only">搜索学习内容</span>
              <IconSearch size={18} />
              <input
                value={search}
                onChange={(event) => updateSearch(event.target.value)}
                placeholder="搜索正文或来源会话…"
                autoComplete="off"
              />
              {search ? (
                <button type="button" onClick={() => updateSearch("")} aria-label="清空搜索">
                  <IconX size={15} />
                </button>
              ) : null}
            </label>
          </section>

          <div className="learning-results-head">
            <div>
              <strong>{debouncedSearch ? `“${debouncedSearch}”的结果` : "全部内容"}</strong>
              <span>{savedItems.isFetching && !savedItems.isLoading ? "正在更新…" : `${total} 篇`}</span>
            </div>
            {totalPages > 1 ? <span className="num">{page + 1} / {totalPages}</span> : null}
          </div>

          {savedItems.isLoading ? (
            <LearningSkeletons />
          ) : savedItems.isError ? (
            <Empty
              icon={<IconBook size={24} />}
              headline="学习内容加载失败"
              hint="请检查网络连接后重试"
              action={<button className="btn primary" onClick={() => void savedItems.refetch()}>重新加载</button>}
            />
          ) : (savedItems.data?.items.length ?? 0) === 0 ? (
            <Empty
              icon={debouncedSearch ? <IconSearch size={24} /> : <IconBook size={24} />}
              headline={debouncedSearch ? "没有找到相关内容" : "还没有保存内容"}
              hint={debouncedSearch ? "换一个关键词试试看" : "在会话中点击 AI 回答下方的“保存”"}
              action={debouncedSearch ? <button className="btn ghost" onClick={() => updateSearch("")}>查看全部</button> : undefined}
            />
          ) : (
            <div className="learning-stack">
              {savedItems.data?.items.map((item, index) => (
                <LearningEntry
                  key={item.id}
                  item={item}
                  number={savedItems.data.offset + index + 1}
                  order={index}
                  onOpenSource={() => navigate({ name: "recentThread", threadId: item.sourceThreadId })}
                />
              ))}
            </div>
          )}

          {totalPages > 1 ? (
            <nav className="learning-pagination" aria-label="学习内容分页">
              <button
                className="btn ghost"
                onClick={() => setPage((current) => Math.max(0, current - 1))}
                disabled={page === 0 || savedItems.isFetching}
              >
                <IconChevronLeft size={16} />
                上一页
              </button>
              <span><strong className="num">{page + 1}</strong><small> / {totalPages}</small></span>
              <button
                className="btn ghost"
                onClick={() => setPage((current) => Math.min(totalPages - 1, current + 1))}
                disabled={page >= totalPages - 1 || savedItems.isFetching}
              >
                下一页
                <IconChevronRight size={16} />
              </button>
            </nav>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function LearningEntry({
  item,
  number,
  order,
  onOpenSource,
}: {
  item: SavedItemView;
  number: number;
  order: number;
  onOpenSource: () => void;
}) {
  return (
    <article
      className="learning-entry"
      style={{ "--entry-order": order } as CSSProperties & Record<"--entry-order", number>}
    >
      <header className="learning-entry-head">
        <span className="learning-entry-number num">NOTE {String(number).padStart(2, "0")}</span>
        <div className="learning-entry-source">
          {item.sourceThreadTitle ? (
            <button onClick={onOpenSource} title="打开来源会话">
              {item.sourceThreadTitle}
              <IconChevronRight size={13} />
            </button>
          ) : (
            <span>来源会话已不可用</span>
          )}
          <time dateTime={item.createdAt}>{fullTime(item.createdAt)}</time>
        </div>
      </header>
      <div className="learning-entry-body">
        <Markdown text={item.contentMarkdown} />
      </div>
    </article>
  );
}

function LearningSkeletons() {
  return (
    <div className="learning-stack" aria-label="正在加载学习内容">
      {[0, 1].map((index) => (
        <div className="learning-entry learning-skeleton" key={index}>
          <div className="skeleton skeleton-line" />
          <div className="skeleton skeleton-title" />
          <div className="skeleton skeleton-line" />
          <div className="skeleton skeleton-line short" />
        </div>
      ))}
    </div>
  );
}

function useDebouncedValue(value: string, delay: number) {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(value.trim()), delay);
    return () => window.clearTimeout(timer);
  }, [delay, value]);
  return debounced;
}
