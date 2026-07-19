/* ThreadView: the shared conversation surface. Merges paged server/local
 * history with the live SSE overlay and renders the composer. */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { CommandStatus } from "../types";
import type { LiveItem, LiveTurn, ThreadLive } from "../stream";
import { clockTime, statusLabel } from "../format";
import { AgentMessage, ApprovalCard, UserBubble, type ApprovalView } from "./items";
import { Composer } from "./Composer";
import { IconArrowDown } from "./icons";
import { Spinner } from "./ui";

export interface RenderItem {
  id: string;
  kind: string;
  text: string;
  status: string;
  truncated?: boolean;
}

export interface HistoryGroup {
  turn: {
    id: string;
    ordinal: number;
    status: string;
    startedAt: string | null;
    completedAt: string | null;
  };
  items: RenderItem[];
}

const SEND_ERROR: CommandStatus[] = ["failed", "rejected", "unknown", "expired"];

export function ThreadView({
  history,
  live,
  approvals,
  onDecide,
  approvalsLocked,
  hasMoreHistory,
  loadingMore,
  onLoadOlder,
  headerOverlay,
  draftKey,
  canSend,
  lockedReason,
  running,
  busy,
  onSend,
  onRetry,
  onInterrupt,
}: {
  history: HistoryGroup[];
  live: ThreadLive;
  approvals: ApprovalView[];
  onDecide: (id: string, decision: string) => void;
  approvalsLocked?: boolean;
  hasMoreHistory?: boolean;
  loadingMore?: boolean;
  onLoadOlder?: () => void;
  headerOverlay?: ReactNode;
  draftKey: string;
  canSend: boolean;
  lockedReason?: string | null;
  running: boolean;
  busy?: boolean;
  onSend: (text: string) => void;
  onRetry?: (turnId: string, text: string) => void;
  onInterrupt: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const [stick, setStick] = useState(true);

  const historyTurnIds = useMemo(
    () => new Set(history.map((g) => g.turn.id)),
    [history],
  );

  /** Only正文消息 overlay history; tool/reasoning activity stays out of the transcript. */
  const liveExtrasFor = (
    turnId: string,
    historyHasAgent: boolean,
    historyUsers: Set<string>,
  ): LiveItem[] => {
    const turn = live.byId[turnId];
    if (!turn) return [];
    return turn.items.filter(
      (item) =>
        (item.kind === "user" && !historyUsers.has(item.text)) ||
        (item.kind === "agent" && !historyHasAgent),
    );
  };

  const historyUserTexts = useMemo(
    () =>
      new Set(
        history.flatMap((g) =>
          g.items.filter((i) => i.kind === "user_message").map((i) => i.text),
        ),
      ),
    [history],
  );

  const freshLiveTurns = live.turns.filter((t) => {
    if (historyTurnIds.has(t.id)) return false;
    // orphan optimistic echo: history already persisted this user message
    // under the authoritative turn id (e.g. the live event was missed)
    if (
      t.id.startsWith("local:") &&
      t.userText &&
      historyUserTexts.has(t.userText) &&
      t.items.length === 0
    ) {
      return false;
    }
    return true;
  });

  /* ---- scroll management ---- */
  const scrollToBottom = useCallback((smooth = false) => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  }, []);

  const setFollowing = useCallback((following: boolean) => {
    stickRef.current = following;
    setStick((current) => (current === following ? current : following));
  }, []);

  const followLatest = useCallback((smooth = false) => {
    setFollowing(true);
    scrollToBottom(smooth);
  }, [scrollToBottom, setFollowing]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    setFollowing(el.scrollHeight - el.scrollTop - el.clientHeight < 140);
  }, [setFollowing]);

  // Streaming Markdown, tool cards, syntax highlighting and approvals can all
  // change height without changing the number of rendered items. Observing the
  // actual content box keeps the latest response visible in every case.
  useEffect(() => {
    const content = contentRef.current;
    const scroller = scrollRef.current;
    if (!content || !scroller || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (stickRef.current) scrollToBottom(false);
    });
    observer.observe(content);
    // The composer grows and shrinks independently of the transcript. Watching
    // the viewport as well keeps its bottom edge pinned while typing/sending.
    observer.observe(scroller);
    return () => observer.disconnect();
  }, [draftKey, scrollToBottom]);

  useLayoutEffect(() => {
    followLatest(false);
  }, [draftKey, followLatest]);

  const sendAndFollow = useCallback((text: string) => {
    followLatest(false);
    onSend(text);
  }, [followLatest, onSend]);

  const renderLiveTurn = (turn: LiveTurn) => {
    const stateErr = turn.sendState && SEND_ERROR.includes(turn.sendState);
    return (
      <section key={turn.id}>
        <div className="turn-meta num">
          {statusLabel(turn.status)}
          {turn.startedAt ? ` · ${clockTime(turn.startedAt)}` : ""}
        </div>
        {turn.userText ? (
          <UserBubble
            text={turn.userText}
            state={turn.sendState}
            stateLabel={
              turn.sendState && turn.sendState !== "completed"
                ? statusLabel(turn.sendState)
                : null
            }
            stateError={Boolean(stateErr)}
            errorMessage={stateErr ? turn.sendErrorMessage : null}
            onRetry={
              stateErr && onRetry
                ? () => onRetry(turn.id, turn.userText ?? "")
                : undefined
            }
          />
        ) : null}
        {turn.items.filter((item) => item.kind === "agent" || item.kind === "user").map((item) =>
          item.kind === "agent" ? (
            <AgentMessage
              key={item.key}
              text={item.text}
              streaming={item.status === "running"}
            />
          ) : item.kind === "user" ? (
            <UserBubble key={item.key} text={item.text} />
          ) : null,
        )}
      </section>
    );
  };

  return (
    <div className="thread-view">
      <div className="thread-scroll" ref={scrollRef} onScroll={onScroll}>
        <div className="thread-col" ref={contentRef}>
          {headerOverlay}
          {hasMoreHistory || loadingMore ? (
            <div style={{ display: "flex", justifyContent: "center", padding: "6px 0 14px" }}>
              <button
                className="btn ghost sm"
                onClick={onLoadOlder}
                disabled={loadingMore}
              >
                {loadingMore ? <Spinner sm /> : null}
                加载更早的记录
              </button>
            </div>
          ) : null}

          {history.map((group) => {
            const hasAgent = group.items.some((i) => i.kind === "agent_message");
            const groupUsers = new Set(
              group.items.filter((item) => item.kind === "user_message").map((item) => item.text),
            );
            const extras = liveExtrasFor(group.turn.id, hasAgent, groupUsers);
            return (
              <section key={group.turn.id}>
                <div className="turn-meta num">
                  第 {group.turn.ordinal} 轮 · {statusLabel(group.turn.status)}
                  {group.turn.startedAt ? ` · ${clockTime(group.turn.startedAt)}` : ""}
                </div>
                {group.items.map((item) => {
                  if (item.kind === "user_message") {
                    return <UserBubble key={item.id} text={item.text} />;
                  }
                  if (item.kind === "agent_message") {
                    return <AgentMessage key={item.id} text={item.text} />;
                  }
                  return null;
                })}
                {extras.map((item) =>
                  item.kind === "agent" ? (
                    <AgentMessage
                      key={item.key}
                      text={item.text}
                      streaming={item.status === "running"}
                    />
                  ) : item.kind === "user" ? (
                    <UserBubble key={item.key} text={item.text} />
                  ) : null,
                )}
              </section>
            );
          })}

          {freshLiveTurns.map(renderLiveTurn)}

          {approvals.map((a) => (
            <ApprovalCard
              key={a.id}
              approval={a}
              onDecide={(d) => onDecide(a.id, d)}
              locked={approvalsLocked}
            />
          ))}

          {history.length === 0 && live.turns.length === 0 ? (
            <div
              style={{
                textAlign: "center",
                color: "var(--ink-3)",
                fontSize: 13.5,
                padding: "48px 24px",
                fontStyle: "italic",
              }}
            >
              暂无消息
            </div>
          ) : null}
          <div style={{ height: 8 }} />
        </div>
      </div>

      {!stick ? (
        <button className="to-bottom" onClick={() => followLatest(true)}>
          <IconArrowDown size={14} />
          回到底部
        </button>
      ) : null}

      <Composer
        draftKey={draftKey}
        canSend={canSend}
        lockedReason={lockedReason}
        running={running}
        busy={busy}
        onSend={sendAndFollow}
        onInterrupt={onInterrupt}
      />
    </div>
  );
}
