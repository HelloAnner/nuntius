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
import type { AttachmentView, CommandStatus } from "../types";
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
  attachments: AttachmentView[];
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
const EMPTY_TEXTS = new Set<string>();

/**
 * Keep the live transcript stable while App Server events and persisted history
 * overlap. A turn already renders its initial prompt from `userText`; some App
 * Server versions also emit that prompt as a user item, which must not become a
 * second bubble. Mid-turn steering echoes use their own key and remain visible.
 */
export function visibleLiveItems(
  turn: LiveTurn,
  historyHasAgent = false,
  historyUsers: ReadonlySet<string> = EMPTY_TEXTS,
): LiveItem[] {
  let skippedInitialEcho = false;
  return turn.items.filter((item) => {
    if (item.kind === "agent") return !historyHasAgent;
    if (
      item.kind !== "user" ||
      (!item.text && item.attachments.length === 0) ||
      (item.text && historyUsers.has(item.text))
    ) return false;
    if (
      !skippedInitialEcho &&
      !item.key.startsWith("steer:") &&
      turn.userText !== null &&
      item.text === turn.userText
    ) {
      skippedInitialEcho = true;
      return false;
    }
    return true;
  });
}

/** Only reconcile an orphan local echo with the corresponding recent turn. */
export function optimisticEchoIsInHistory(turn: LiveTurn, history: HistoryGroup[]): boolean {
  if (
    !turn.id.startsWith("local:") ||
    (!turn.userText && turn.userAttachments.length === 0) ||
    turn.items.length > 0
  ) return false;
  const optimisticAt = Date.parse(turn.startedAt);
  return history.some((group, index) => {
    const hasMessage = group.items.some((item) => {
      if (item.kind !== "user_message" || item.text !== (turn.userText ?? "")) return false;
      const expected = turn.userAttachments.map((attachment) => attachment.id).join(",");
      const actual = item.attachments.map((attachment) => attachment.id).join(",");
      return expected === actual;
    });
    if (!hasMessage) return false;
    const historyAt = group.turn.startedAt ? Date.parse(group.turn.startedAt) : Number.NaN;
    if (Number.isFinite(optimisticAt) && Number.isFinite(historyAt)) {
      return Math.abs(optimisticAt - historyAt) < 120_000;
    }
    // Timestamps can be absent in imported history. In that case only the
    // newest turn is a safe fallback; an older identical prompt is unrelated.
    return index === history.length - 1;
  });
}

function normalizedMessage(text: string): string {
  return text.replace(/\r\n/g, "\n").trim();
}

function attachmentIds(attachments: AttachmentView[]): string {
  return attachments.map((attachment) => attachment.id).join(",");
}

/** Hide duplicate persisted assistant rows without changing the durable history. */
export function visibleHistoryItems(items: RenderItem[]): RenderItem[] {
  const seenAgentMessages = new Set<string>();
  return items.filter((item) => {
    if (item.kind !== "agent_message") return true;
    const signature = normalizedMessage(item.text);
    if (!signature || seenAgentMessages.has(signature)) return false;
    seenAgentMessages.add(signature);
    return true;
  });
}

/**
 * A reconnect snapshot can persist a reply before its replayed live event is
 * applied. Those two layers may use different turn ids, so reconcile completed
 * assistant text together with the prompt and timestamp instead of relying on
 * identity alone.
 */
export function liveTurnIsInHistory(turn: LiveTurn, history: HistoryGroup[]): boolean {
  const liveItems = visibleLiveItems(turn);
  const agentMessages = liveItems
    .filter((item) => item.kind === "agent")
    .map((item) => normalizedMessage(item.text))
    .filter(Boolean);
  const userMessages = liveItems.filter((item) => item.kind === "user");
  if (agentMessages.length === 0) return false;

  const hasPrompt = turn.userText !== null || turn.userAttachments.length > 0;
  const liveAt = Date.parse(turn.startedAt);
  return history.some((group, index) => {
    const items = visibleHistoryItems(group.items);
    const persistedAgents = new Set(
      items
        .filter((item) => item.kind === "agent_message")
        .map((item) => normalizedMessage(item.text))
        .filter(Boolean),
    );
    if (!agentMessages.every((message) => persistedAgents.has(message))) return false;
    const allSteersPersisted = userMessages.every((liveItem) =>
      items.some(
        (item) =>
          item.kind === "user_message" &&
          item.text === liveItem.text &&
          attachmentIds(item.attachments) === attachmentIds(liveItem.attachments),
      ),
    );
    if (!allSteersPersisted) return false;

    if (!hasPrompt) return index === history.length - 1;
    const promptMatches = items.some(
      (item) =>
        item.kind === "user_message" &&
        item.text === (turn.userText ?? "") &&
        attachmentIds(item.attachments) === attachmentIds(turn.userAttachments),
    );
    if (!promptMatches) return false;

    const historyAt = group.turn.startedAt ? Date.parse(group.turn.startedAt) : Number.NaN;
    if (Number.isFinite(liveAt) && Number.isFinite(historyAt)) {
      return Math.abs(liveAt - historyAt) < 120_000;
    }
    return index === history.length - 1;
  });
}

export function ThreadView({
  history,
  loading,
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
  runtimeStatus,
  runtimeConnected,
  busy,
  onSend,
  onUpload,
  onDeleteAttachment,
  onRetry,
  onInterrupt,
}: {
  history: HistoryGroup[];
  loading?: boolean;
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
  runtimeStatus: string | null;
  runtimeConnected: boolean;
  busy?: boolean;
  onSend: (text: string, attachments: AttachmentView[], clientMessageId: string) => void;
  onUpload?: (file: File, onProgress: (progress: number) => void) => Promise<AttachmentView>;
  onDeleteAttachment?: (attachmentId: string) => Promise<void>;
  onRetry?: (turnId: string, text: string, attachments: AttachmentView[]) => void;
  onInterrupt: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const prependAnchorRef = useRef<{
    firstTurnId: string | null;
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
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
    return visibleLiveItems(turn, historyHasAgent, historyUsers);
  };

  const freshLiveTurns = live.turns.filter((t) => {
    if (historyTurnIds.has(t.id)) return false;
    // orphan optimistic echo: history already persisted this user message
    // under the authoritative turn id (e.g. the live event was missed)
    if (optimisticEchoIsInHistory(t, history)) return false;
    if (liveTurnIsInHistory(t, history)) return false;
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

  const loadOlder = useCallback(() => {
    const el = scrollRef.current;
    if (el) {
      prependAnchorRef.current = {
        firstTurnId: history[0]?.turn.id ?? null,
        scrollHeight: el.scrollHeight,
        scrollTop: el.scrollTop,
      };
    }
    onLoadOlder?.();
  }, [history, onLoadOlder]);

  useLayoutEffect(() => {
    const anchor = prependAnchorRef.current;
    const el = scrollRef.current;
    if (!anchor || !el || history[0]?.turn.id === anchor.firstTurnId) return;
    el.scrollTop = anchor.scrollTop + (el.scrollHeight - anchor.scrollHeight);
    prependAnchorRef.current = null;
    setFollowing(false);
  }, [history, setFollowing]);

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
    if (loading) return;
    const el = scrollRef.current;
    if (!el) return;
    const key = `nuntius:scroll:${draftKey}`;
    let restored = false;
    try {
      const raw = sessionStorage.getItem(key);
      if (raw) {
        const saved = JSON.parse(raw) as { top?: number; following?: boolean };
        if (saved.following === false && typeof saved.top === "number") {
          el.scrollTop = Math.max(0, Math.min(saved.top, el.scrollHeight - el.clientHeight));
          setFollowing(false);
          restored = true;
        }
      }
    } catch {
      /* unavailable or malformed session storage */
    }
    if (!restored) followLatest(false);
    return () => {
      try {
        sessionStorage.setItem(
          key,
          JSON.stringify({ top: el.scrollTop, following: stickRef.current }),
        );
      } catch {
        /* unavailable session storage */
      }
    };
  }, [draftKey, followLatest, loading, setFollowing]);

  const sendAndFollow = useCallback((text: string, attachments: AttachmentView[], clientMessageId: string) => {
    followLatest(false);
    onSend(text, attachments, clientMessageId);
  }, [followLatest, onSend]);

  const renderLiveTurn = (turn: LiveTurn) => {
    const stateErr = turn.sendState && SEND_ERROR.includes(turn.sendState);
    return (
      <section key={turn.renderKey ?? turn.id}>
        <TurnMeta status={turn.status} startedAt={turn.startedAt} />
        {turn.userText || turn.userAttachments.length > 0 ? (
          <UserBubble
            text={turn.userText ?? ""}
            attachments={turn.userAttachments}
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
                ? () => onRetry(turn.id, turn.userText ?? "", turn.userAttachments)
                : undefined
            }
          />
        ) : null}
        {visibleLiveItems(turn).map((item) =>
          item.kind === "agent" ? (
            <AgentMessage
              key={item.key}
              text={item.text}
              streaming={item.status === "running"}
            />
          ) : item.kind === "user" ? (
            <UserBubble key={item.key} text={item.text} attachments={item.attachments} />
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
          {loading ? (
            <div className="thread-loading" role="status" aria-label="正在加载会话记录">
              <div className="skeleton thread-loading-user" />
              <div className="skeleton thread-loading-agent" />
              <div className="skeleton thread-loading-agent short" />
            </div>
          ) : null}
          {hasMoreHistory || loadingMore ? (
            <div style={{ display: "flex", justifyContent: "center", padding: "6px 0 14px" }}>
              <button
                className="btn ghost sm"
                onClick={loadOlder}
                disabled={loadingMore}
              >
                {loadingMore ? <Spinner sm /> : null}
                加载更早的记录
              </button>
            </div>
          ) : null}

          {history.map((group) => {
            const items = visibleHistoryItems(group.items);
            const hasAgent = items.some((i) => i.kind === "agent_message");
            const groupUsers = new Set(
              items.filter((item) => item.kind === "user_message").map((item) => item.text),
            );
            const extras = liveExtrasFor(group.turn.id, hasAgent, groupUsers);
            return (
              <section key={group.turn.id}>
                <TurnMeta
                  status={group.turn.status}
                  startedAt={group.turn.startedAt}
                  ordinal={group.turn.ordinal}
                />
                {items.map((item) => {
                  if (item.kind === "user_message") {
                    return <UserBubble key={item.id} text={item.text} attachments={item.attachments} />;
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
                    <UserBubble key={item.key} text={item.text} attachments={item.attachments} />
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

          {!loading && history.length === 0 && live.turns.length === 0 ? (
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
        runtimeStatus={runtimeStatus}
        runtimeConnected={runtimeConnected}
        busy={busy}
        onSend={sendAndFollow}
        onUpload={onUpload}
        onDeleteAttachment={onDeleteAttachment}
        onInterrupt={onInterrupt}
      />
    </div>
  );
}

function TurnMeta({
  status,
  startedAt,
  ordinal,
}: {
  status: string;
  startedAt: string | null;
  ordinal?: number;
}) {
  const active = status === "active" || status === "running";
  const quiet = status === "completed" || status === "idle";
  const time = clockTime(startedAt);
  const spoken = [ordinal ? `第 ${ordinal} 轮` : null, statusLabel(status), time].filter(Boolean).join("，");
  return (
    <div className={`turn-meta num${active ? " active" : ""}`} aria-label={spoken}>
      {active ? <span className="live-dot" aria-hidden="true" /> : null}
      {!quiet && !active ? <span>{statusLabel(status)}</span> : null}
      {time ? <span>{time}</span> : <span aria-hidden="true">·</span>}
    </div>
  );
}
