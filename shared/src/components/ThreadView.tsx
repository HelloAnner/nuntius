/* ThreadView: the shared conversation surface. Merges paged server/local
 * history with the live SSE overlay and renders the composer. */
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { CommandStatus } from "../types";
import type { LiveItem, LiveTurn, ThreadLive } from "../stream";
import { clockTime, statusLabel } from "../format";
import { AgentMessage, ApprovalCard, UserBubble, WorkItemView, type ApprovalView } from "./items";
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

const HISTORY_KIND_LABELS: Record<string, string> = {
  user_message: "用户消息",
  agent_message: "Agent 消息",
  command_execution: "命令执行",
  file_change: "文件变更",
  reasoning: "思考过程",
  tool_call: "工具调用",
  mcp_tool_call: "MCP 工具",
  plan: "计划",
  error: "错误",
};

function historyItemToLive(item: RenderItem): LiveItem {
  return {
    key: item.id,
    kind: "other",
    title: HISTORY_KIND_LABELS[item.kind] ?? item.kind,
    text: item.text,
    status:
      item.status === "failed"
        ? "failed"
        : item.status === "running"
          ? "running"
          : "completed",
    files: [],
  };
}

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
  onSteer,
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
  onSteer: (text: string) => void;
  onInterrupt: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [stick, setStick] = useState(true);

  const historyTurnIds = useMemo(
    () => new Set(history.map((g) => g.turn.id)),
    [history],
  );

  /** live work-items attached to a turn that history already renders */
  const liveExtrasFor = (turnId: string, historyHasAgent: boolean): LiveItem[] => {
    const turn = live.byId[turnId];
    if (!turn) return [];
    return turn.items.filter((it) =>
      it.kind === "agent" ? !historyHasAgent : true,
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

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    setStick(el.scrollHeight - el.scrollTop - el.clientHeight < 140);
  };

  const contentKey = `${history.length}:${live.turns.length}:${live.turns
    .map((t) => t.items.reduce((n, i) => n + i.text.length, 0))
    .join(",")}:${approvals.length}`;

  useEffect(() => {
    if (stick) scrollToBottom(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentKey]);

  useEffect(() => {
    scrollToBottom(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draftKey]);

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
          />
        ) : null}
        {turn.items.map((item) =>
          item.kind === "agent" ? (
            <AgentMessage
              key={item.key}
              text={item.text}
              streaming={item.status === "running"}
            />
          ) : item.kind === "user" ? (
            <UserBubble key={item.key} text={item.text} />
          ) : (
            <WorkItemView key={item.key} item={item} />
          ),
        )}
      </section>
    );
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, position: "relative" }}>
      <div className="thread-scroll" ref={scrollRef} onScroll={onScroll}>
        <div className="thread-col">
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
            const extras = liveExtrasFor(group.turn.id, hasAgent);
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
                  return <WorkItemView key={item.id} item={historyItemToLive(item)} />;
                })}
                {extras.map((item) =>
                  item.kind === "agent" ? (
                    <AgentMessage
                      key={item.key}
                      text={item.text}
                      streaming={item.status === "running"}
                    />
                  ) : (
                    <WorkItemView key={item.key} item={item} />
                  ),
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
              还没有记录。说点什么，让这台电脑开始工作。
            </div>
          ) : null}
          <div style={{ height: 8 }} />
        </div>
      </div>

      {!stick ? (
        <button className="to-bottom" onClick={() => scrollToBottom(true)}>
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
        onSend={onSend}
        onSteer={onSteer}
        onInterrupt={onInterrupt}
      />
    </div>
  );
}
