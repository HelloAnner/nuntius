/* ThreadView: the shared conversation surface. Merges paged server/local
 * history with the live SSE overlay and renders the composer. */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { AttachmentView, CommandStatus } from "../types";
import {
  compareChronology,
  compareLiveTurns,
  liveTurnChronology,
  orderedLiveItems,
  type LiveItem,
  type LiveTurn,
  type ThreadLive,
} from "../stream";
import { clockTime, isRunningStatus, statusLabel } from "../format";
import { AgentMessage, ApprovalCard, UserBubble, type ApprovalView } from "./items";
import { Composer } from "./Composer";
import { IconArrowDown } from "./icons";
import { Spinner } from "./ui";

export interface RenderItem {
  id: string;
  ordinal: number;
  kind: string;
  text: string;
  status: string;
  occurredAt: string;
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
const EMPTY_COUNTS = new Map<string, number>();
const BOTTOM_FOLLOW_EPSILON = 24;

/** Treat only the final visual line as "at bottom". A wider threshold makes a
 * reader who has deliberately moved upward vulnerable to the next resize. */
export function isThreadNearBottom(
  scrollHeight: number,
  scrollTop: number,
  clientHeight: number,
): boolean {
  const distance = scrollHeight - Math.max(0, scrollTop) - clientHeight;
  return Math.max(0, distance) <= BOTTOM_FOLLOW_EPSILON;
}

/**
 * Keep the live transcript stable while App Server events and persisted history
 * overlap. A turn already renders its initial prompt from `userText`; some App
 * Server versions also emit that prompt as a user item, which must not become a
 * second bubble. Mid-turn steering echoes use their own key and remain visible.
 */
export function visibleLiveItems(
  turn: LiveTurn,
  historyAgents: ReadonlyMap<string, number> = EMPTY_COUNTS,
  historyUsers: ReadonlyMap<string, number> = EMPTY_COUNTS,
): LiveItem[] {
  let skippedInitialEcho = false;
  const remainingAgents = new Map(historyAgents);
  const remainingUsers = new Map(historyUsers);
  return orderedLiveItems(turn).filter((item) => {
    if (item.kind === "agent") {
      const signature = normalizedMessage(item.text);
      if (!signature) return false;
      const persisted = remainingAgents.get(signature) ?? 0;
      if (persisted > 0) {
        remainingAgents.set(signature, persisted - 1);
        return false;
      }
      return true;
    }
    if (item.kind !== "user" || (!item.text && item.attachments.length === 0)) return false;
    if (item.text) {
      const persisted = remainingUsers.get(item.text) ?? 0;
      if (persisted > 0) {
        remainingUsers.set(item.text, persisted - 1);
        return false;
      }
    }
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

/** Keep durable items in their recorded chronology and never collapse two
 * legitimate identical replies merely because their text happens to match. */
export function visibleHistoryItems(items: RenderItem[]): RenderItem[] {
  const seenIds = new Set<string>();
  return [...items]
    .sort((left, right) =>
      compareChronology(
        left.occurredAt,
        left.ordinal,
        left.id,
        right.occurredAt,
        right.ordinal,
        right.id,
      ),
    )
    .filter((item) => {
      if (seenIds.has(item.id)) return false;
      seenIds.add(item.id);
      return true;
    });
}

export function historyGroupChronology(group: HistoryGroup): {
  occurredAt: string | null;
  sequence: number;
  id: string;
} {
  let anchor = {
    occurredAt: group.turn.startedAt,
    sequence: group.turn.ordinal,
    id: group.turn.id,
  };
  if (
    compareChronology(
      group.turn.completedAt,
      group.turn.ordinal,
      `${group.turn.id}:completed`,
      anchor.occurredAt,
      anchor.sequence,
      anchor.id,
    ) < 0
  ) {
    anchor = {
      occurredAt: group.turn.completedAt,
      sequence: group.turn.ordinal,
      id: `${group.turn.id}:completed`,
    };
  }
  for (const item of group.items) {
    if (
      compareChronology(
        item.occurredAt,
        item.ordinal,
        item.id,
        anchor.occurredAt,
        anchor.sequence,
        anchor.id,
      ) < 0
    ) {
      anchor = {
        occurredAt: item.occurredAt,
        sequence: item.ordinal,
        id: item.id,
      };
    }
  }
  return anchor;
}

function historyGroupAt(group: HistoryGroup): string | null {
  return historyGroupChronology(group).occurredAt;
}

export function orderedHistory(history: HistoryGroup[]): HistoryGroup[] {
  const seen = new Set<string>();
  return history
    .filter((group) => {
      if (seen.has(group.turn.id)) return false;
      seen.add(group.turn.id);
      return true;
    })
    .map((group) => ({ ...group, items: visibleHistoryItems(group.items) }))
    .sort((left, right) => {
      const leftAnchor = historyGroupChronology(left);
      const rightAnchor = historyGroupChronology(right);
      return compareChronology(
        leftAnchor.occurredAt,
        leftAnchor.sequence,
        leftAnchor.id,
        rightAnchor.occurredAt,
        rightAnchor.sequence,
        rightAnchor.id,
      );
    });
}

export interface TimelineSortKey {
  occurredAt: string | null;
  sequence: number;
  sortId: string;
}

/** Canonical render order shared by history, SSE overlays, optimistic prompts,
 * and approvals. Never rely on fetch or event arrival order. */
export function orderedTimeline<T extends TimelineSortKey>(entries: T[]): T[] {
  return [...entries].sort((left, right) =>
    compareChronology(
      left.occurredAt,
      left.sequence,
      left.sortId,
      right.occurredAt,
      right.sequence,
      right.sortId,
    ),
  );
}

type TimelineEntry = TimelineSortKey & (
  | {
      kind: "turn";
      key: string;
      status: string;
      ordinal?: number;
    }
  | {
      kind: "history-item";
      key: string;
      item: RenderItem;
    }
  | {
      kind: "live-prompt";
      key: string;
      turn: LiveTurn;
    }
  | {
      kind: "live-item";
      key: string;
      item: LiveItem;
    }
  | {
      kind: "approval";
      key: string;
      approval: ApprovalView;
    }
);

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

/** Reconcile the two layers one-to-one. The previous per-turn `some()` check
 * let one persisted prompt hide multiple identical optimistic sends. */
export function freshLiveTurnsForHistory(
  history: HistoryGroup[],
  turns: LiveTurn[],
): LiveTurn[] {
  const durable = orderedHistory(history);
  const claimed = new Set<number>();
  const fresh: LiveTurn[] = [];

  for (const turn of [...turns].sort(compareLiveTurns)) {
    const exact = durable.findIndex((group) => group.turn.id === turn.id);
    if (exact >= 0) {
      claimed.add(exact);
      continue;
    }
    const candidates = durable
      .map((group, index) => ({ group, index }))
      .filter(({ group, index }) =>
        !claimed.has(index) &&
        (optimisticEchoIsInHistory(turn, [group]) || liveTurnIsInHistory(turn, [group])),
      );
    if (candidates.length === 0) {
      fresh.push(turn);
      continue;
    }
    const liveTime = Date.parse(turn.startedAt);
    candidates.sort((left, right) => {
      const leftTime = Date.parse(historyGroupAt(left.group) ?? "");
      const rightTime = Date.parse(historyGroupAt(right.group) ?? "");
      const leftDistance = Number.isFinite(liveTime) && Number.isFinite(leftTime)
        ? Math.abs(liveTime - leftTime)
        : Number.POSITIVE_INFINITY;
      const rightDistance = Number.isFinite(liveTime) && Number.isFinite(rightTime)
        ? Math.abs(liveTime - rightTime)
        : Number.POSITIVE_INFINITY;
      if (leftDistance !== rightDistance) return leftDistance - rightDistance;
      return right.index - left.index;
    });
    claimed.add(candidates[0].index);
  }
  return fresh;
}

export function liveTurnHasTranscript(turn: LiveTurn): boolean {
  return Boolean(
    turn.userText ||
    turn.userAttachments.length > 0 ||
    visibleLiveItems(turn).length > 0
  );
}

/** Do not stack a loading skeleton above an optimistic/live message. The
 * skeleton disappearing underneath existing content causes a visible jump. */
export function shouldRenderThreadLoading(loading: boolean, transcriptCount: number): boolean {
  return loading && transcriptCount === 0;
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

  const durableHistory = orderedHistory(history);
  const unmatchedLiveTurns = freshLiveTurnsForHistory(durableHistory, live.turns);
  const latestActiveHistory = [...durableHistory]
    .reverse()
    .find((group) => isRunningStatus(group.turn.status));
  // App Server item events do not always carry the local turn id. An unbound
  // live turn without its own prompt is an overlay for the durable active turn,
  // not a new conversation turn with another divider.
  const activeOrphans = latestActiveHistory
    ? unmatchedLiveTurns.filter((turn) => turn.userText === null && turn.items.length > 0)
    : [];
  const activeOrphanIds = new Set(activeOrphans.map((turn) => turn.id));
  const freshLiveTurns = unmatchedLiveTurns.filter(
    (turn) =>
      !activeOrphanIds.has(turn.id) &&
      liveTurnHasTranscript(turn),
  );

  /** Only正文消息 overlay history; tool/reasoning activity stays out of the transcript. */
  const liveExtrasFor = (
    turnId: string,
    historyAgents: Map<string, number>,
    historyUsers: Map<string, number>,
  ): Array<{ ownerId: string; item: LiveItem }> => {
    const turns = [
      live.byId[turnId],
      ...(latestActiveHistory?.turn.id === turnId ? activeOrphans : []),
    ].filter((turn): turn is LiveTurn => Boolean(turn));
    const seen = new Set<string>();
    return turns
      .flatMap((turn) =>
        visibleLiveItems(turn, historyAgents, historyUsers).map((item) => ({
          ownerId: turn.id,
          item,
        })),
      )
      .filter(({ item }) => {
        const signature = `${item.kind}:${normalizedMessage(item.text)}:${item.key}`;
        if (seen.has(signature)) return false;
        seen.add(signature);
        return true;
      })
      .sort((left, right) =>
        compareChronology(
          left.item.occurredAt,
          left.item.sequence,
          `${left.ownerId}:${left.item.key}`,
          right.item.occurredAt,
          right.item.sequence,
          `${right.ownerId}:${right.item.key}`,
        ),
      );
  };
  const timelineEntries: TimelineEntry[] = [];
  for (const group of durableHistory) {
    const groupAt = historyGroupAt(group);
    const renderKey = live.byId[group.turn.id]?.renderKey ?? group.turn.id;
    timelineEntries.push({
      kind: "turn",
      key: `turn:${renderKey}`,
      status: group.turn.status,
      ordinal: group.turn.ordinal,
      occurredAt: group.turn.startedAt ?? groupAt,
      sequence: Number.MIN_SAFE_INTEGER,
      sortId: `0:turn:${group.turn.id}`,
    });

    const groupAgents = new Map<string, number>();
    const groupUsers = new Map<string, number>();
    for (const item of group.items) {
      if (item.kind === "agent_message") {
        const signature = normalizedMessage(item.text);
        if (signature) groupAgents.set(signature, (groupAgents.get(signature) ?? 0) + 1);
      } else if (item.kind === "user_message") {
        groupUsers.set(item.text, (groupUsers.get(item.text) ?? 0) + 1);
      }
      if (item.kind !== "user_message" && item.kind !== "agent_message") continue;
      timelineEntries.push({
        kind: "history-item",
        key: `history:${group.turn.id}:${item.id}`,
        item,
        occurredAt: item.occurredAt,
        sequence: item.ordinal,
        sortId: `1:history:${group.turn.id}:${item.id}`,
      });
    }

    for (const { ownerId, item } of liveExtrasFor(group.turn.id, groupAgents, groupUsers)) {
      timelineEntries.push({
        kind: "live-item",
        key: `live:${renderKey}:${ownerId}:${item.key}`,
        item,
        occurredAt: item.occurredAt,
        sequence: item.sequence,
        sortId: `2:live:${group.turn.id}:${ownerId}:${item.key}`,
      });
    }
  }

  for (const turn of freshLiveTurns) {
    const renderKey = turn.renderKey ?? turn.id;
    const turnAnchor = liveTurnChronology(turn);
    timelineEntries.push({
      kind: "turn",
      key: `turn:${renderKey}`,
      status: turn.status,
      occurredAt: turn.startedAt || turnAnchor.occurredAt,
      sequence: Number.MIN_SAFE_INTEGER,
      sortId: `0:turn:${turn.id}`,
    });
    if (turn.userText || turn.userAttachments.length > 0) {
      timelineEntries.push({
        kind: "live-prompt",
        key: `prompt:${renderKey}`,
        turn,
        occurredAt: turn.startedAt || turnAnchor.occurredAt,
        sequence: Number.MIN_SAFE_INTEGER,
        sortId: `1:prompt:${turn.id}`,
      });
    }
    for (const item of visibleLiveItems(turn)) {
      timelineEntries.push({
        kind: "live-item",
        key: `live:${renderKey}:${item.key}`,
        item,
        occurredAt: item.occurredAt,
        sequence: item.sequence,
        sortId: `2:live:${turn.id}:${item.key}`,
      });
    }
  }

  for (const approval of approvals) {
    timelineEntries.push({
      kind: "approval",
      key: `approval:${approval.id}`,
      approval,
      occurredAt: approval.occurredAt,
      sequence: Number.MAX_SAFE_INTEGER,
      sortId: `3:approval:${approval.id}`,
    });
  }
  const timeline = orderedTimeline(timelineEntries);
  const transcriptCount = timeline.filter((entry) => entry.kind !== "turn").length;

  /* ---- scroll management ---- */
  const scrollToBottom = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    // Direct assignment is deterministic and cannot leave the follow state in
    // an intermediate position as a smooth scroll emits multiple events.
    el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight);
  }, []);

  const setFollowing = useCallback((following: boolean) => {
    stickRef.current = following;
    setStick((current) => (current === following ? current : following));
  }, []);

  const followLatest = useCallback(() => {
    setFollowing(true);
    scrollToBottom();
  }, [scrollToBottom, setFollowing]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    setFollowing(isThreadNearBottom(el.scrollHeight, el.scrollTop, el.clientHeight));
  }, [setFollowing]);

  const loadOlder = useCallback(() => {
    const el = scrollRef.current;
    if (el) {
      prependAnchorRef.current = {
        firstTurnId: durableHistory[0]?.turn.id ?? null,
        scrollHeight: el.scrollHeight,
        scrollTop: el.scrollTop,
      };
    }
    // Loading UI and prepended records both change the content box. Detach
    // before either can be mistaken for new content that should be followed.
    setFollowing(false);
    onLoadOlder?.();
  }, [durableHistory, onLoadOlder, setFollowing]);

  useLayoutEffect(() => {
    const anchor = prependAnchorRef.current;
    const el = scrollRef.current;
    if (!anchor || !el || durableHistory[0]?.turn.id === anchor.firstTurnId) return;
    el.scrollTop = anchor.scrollTop + (el.scrollHeight - anchor.scrollHeight);
    prependAnchorRef.current = null;
    setFollowing(false);
  }, [durableHistory, setFollowing]);

  // React-owned transcript changes are known during the commit. Correct the
  // bottom anchor before the browser paints instead of waiting one frame for a
  // ResizeObserver delivery.
  useLayoutEffect(() => {
    if (stickRef.current) scrollToBottom();
  });

  // Streaming Markdown, tool cards, syntax highlighting and approvals can all
  // change height without changing the number of rendered items. Observing the
  // actual content box keeps the latest response visible in every case.
  useEffect(() => {
    const content = contentRef.current;
    const scroller = scrollRef.current;
    if (!content || !scroller || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (stickRef.current) scrollToBottom();
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
    if (!restored) followLatest();
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
    followLatest();
    onSend(text, attachments, clientMessageId);
  }, [followLatest, onSend]);

  return (
    <div className="thread-view">
      <div className="thread-scroll" ref={scrollRef} onScroll={onScroll}>
        <div className="thread-col" ref={contentRef}>
          {headerOverlay}
          {shouldRenderThreadLoading(Boolean(loading), transcriptCount) ? (
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

          {timeline.map((entry) => {
            if (entry.kind === "turn") {
              return (
                <TurnMeta
                  key={entry.key}
                  status={entry.status}
                  startedAt={entry.occurredAt}
                  ordinal={entry.ordinal}
                />
              );
            }
            if (entry.kind === "history-item") {
              return entry.item.kind === "user_message" ? (
                <UserBubble
                  key={entry.key}
                  text={entry.item.text}
                  attachments={entry.item.attachments}
                />
              ) : (
                <AgentMessage key={entry.key} text={entry.item.text} />
              );
            }
            if (entry.kind === "live-prompt") {
              const stateErr = Boolean(
                entry.turn.sendState && SEND_ERROR.includes(entry.turn.sendState),
              );
              return (
                <UserBubble
                  key={entry.key}
                  text={entry.turn.userText ?? ""}
                  attachments={entry.turn.userAttachments}
                  state={entry.turn.sendState}
                  stateLabel={
                    entry.turn.sendState && entry.turn.sendState !== "completed"
                      ? statusLabel(entry.turn.sendState)
                      : null
                  }
                  stateError={stateErr}
                  errorMessage={stateErr ? entry.turn.sendErrorMessage : null}
                  onRetry={
                    stateErr && onRetry
                      ? () => onRetry(
                          entry.turn.id,
                          entry.turn.userText ?? "",
                          entry.turn.userAttachments,
                        )
                      : undefined
                  }
                />
              );
            }
            if (entry.kind === "live-item") {
              return entry.item.kind === "agent" ? (
                <AgentMessage
                  key={entry.key}
                  text={entry.item.text}
                  streaming={entry.item.status === "running"}
                />
              ) : (
                <UserBubble
                  key={entry.key}
                  text={entry.item.text}
                  attachments={entry.item.attachments}
                />
              );
            }
            return (
              <ApprovalCard
                key={entry.key}
                approval={entry.approval}
                onDecide={(decision) => onDecide(entry.approval.id, decision)}
                locked={approvalsLocked}
              />
            );
          })}

          {!loading && timeline.length === 0 ? (
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
        <button className="to-bottom" onClick={followLatest}>
          <IconArrowDown size={14} />
          回到底部
        </button>
      ) : null}

      <Composer
        draftKey={draftKey}
        canSend={canSend}
        lockedReason={lockedReason}
        running={running || runtimeStatus === "stalled"}
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
  const active = isRunningStatus(status);
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
