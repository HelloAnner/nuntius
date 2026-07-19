/* Live stream aggregation: turns NuntiusEvents (SSE) into per-thread render
 * state shared by the remote and local consoles. Framework-free; apps wrap it
 * with useSyncExternalStore. */

import type {
  AttachmentView,
  ApprovalRequestedPayload,
  CommandStatus,
  NuntiusEvent,
  TurnStartedPayload,
} from "./types";

export type LiveKind =
  | "user"
  | "agent"
  | "reasoning"
  | "command"
  | "file"
  | "tool"
  | "plan"
  | "other";

export type LiveStatus = "running" | "completed" | "failed" | "declined" | "unknown";

export interface LiveFile {
  path: string;
  kind: "add" | "mod" | "del";
}

export interface LiveItem {
  key: string;
  kind: LiveKind;
  title: string;
  text: string;
  status: LiveStatus;
  files: LiveFile[];
  occurredAt: string;
  sequence: number;
}

export type LiveTurnStatus =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "interrupted"
  | "unknown";

export interface LiveTurn {
  id: string;
  /** Preserve React identity when an optimistic id adopts the durable id. */
  renderKey?: string;
  status: LiveTurnStatus;
  userText: string | null;
  userAttachments: AttachmentView[];
  clientMessageId: string | null;
  sendState: CommandStatus | null;
  sendErrorCode: string | null;
  sendErrorMessage: string | null;
  items: LiveItem[];
  itemIndex: Record<string, number>;
  startedAt: string;
  startedSequence: number;
}

export interface ThreadLive {
  turns: LiveTurn[];
  byId: Record<string, LiveTurn>;
}

type Listener = () => void;

const TERMINAL = new Set(["completed", "failed", "interrupted"]);

export class ThreadLiveStore {
  private threads = new Map<string, ThreadLive>();
  private listeners = new Set<Listener>();
  private version = 0;
  private notifyScheduled = false;
  /** commandId -> optimistic echo location */
  private pending = new Map<string, { threadId: string; turnId: string }>();
  private seenEvents = new Set<string>();
  private seenEventOrder: string[] = [];

  subscribe = (fn: Listener): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  getVersion = (): number => this.version;

  private bump() {
    // An immediate state transition supersedes a queued delta repaint.
    this.notifyScheduled = false;
    this.version += 1;
    for (const fn of this.listeners) fn();
  }

  /** Coalesce token deltas to a bounded update rate; Markdown parsing at 60fps
   * is expensive on phones and does not improve perceived streaming quality. */
  private scheduleBump() {
    if (this.notifyScheduled) return;
    this.notifyScheduled = true;
    const flush = () => {
      if (!this.notifyScheduled) return;
      this.notifyScheduled = false;
      this.bump();
    };
    setTimeout(flush, 50);
  }

  get(threadId: string): ThreadLive {
    let t = this.threads.get(threadId);
    if (!t) {
      t = { turns: [], byId: {} };
      this.threads.set(threadId, t);
    }
    return t;
  }

  /** Drop disposable realtime overlays before applying a database snapshot. */
  reset() {
    this.threads.clear();
    this.pending.clear();
    this.seenEvents.clear();
    this.seenEventOrder = [];
    this.bump();
  }

  /** optimistic user echo registered right after a 202 receipt */
  addOptimistic(
    threadId: string,
    commandId: string,
    text: string,
    attachments: AttachmentView[] = [],
    clientMessageId: string | null = null,
  ): string {
    const live = this.get(threadId);
    // The SSE turn can beat the HTTP 202 response. Adopt only a recent,
    // unclaimed authoritative turn with the same prompt so that race does not
    // create a second user bubble.
    const claimed = new Set(
      [...this.pending.values()]
        .filter((location) => location.threadId === threadId)
        .map((location) => location.turnId),
    );
    const now = Date.now();
    const authoritative = live.turns
      .filter(
        (turn) =>
          !turn.id.startsWith("local:") &&
          !claimed.has(turn.id) &&
          turn.userText === text &&
          Number.isFinite(Date.parse(turn.startedAt)) &&
          Math.abs(now - Date.parse(turn.startedAt)) < 120_000,
      )
      .sort((left, right) =>
        Math.abs(now - Date.parse(left.startedAt)) -
        Math.abs(now - Date.parse(right.startedAt)),
      )[0];
    if (authoritative) {
      this.pending.set(commandId, { threadId, turnId: authoritative.id });
      this.bump();
      return authoritative.id;
    }
    const turnId = `local:${commandId}`;
    if (!live.byId[turnId]) {
      const turn: LiveTurn = {
        id: turnId,
        renderKey: turnId,
        status: "running",
        userText: text,
        userAttachments: attachments,
        clientMessageId,
        sendState: "accepted",
        sendErrorCode: null,
        sendErrorMessage: null,
        items: [],
        itemIndex: {},
        startedAt: new Date().toISOString(),
        startedSequence: Number.MAX_SAFE_INTEGER,
      };
      live.byId[turnId] = turn;
      live.turns.push(turn);
      this.pending.set(commandId, { threadId, turnId });
      this.bump();
    }
    return turnId;
  }

  bindCommand(provisionalId: string, commandId: string) {
    const loc = this.pending.get(provisionalId);
    if (!loc) return;
    this.pending.delete(provisionalId);
    this.pending.set(commandId, loc);
  }

  applyCommandStatus(
    commandId: string,
    status: CommandStatus,
    errorCode?: string | null,
    errorMessage?: string | null,
  ) {
    const loc = this.pending.get(commandId);
    if (!loc) return;
    const turn = this.get(loc.threadId).byId[loc.turnId];
    if (turn) {
      turn.sendState = status;
      turn.sendErrorCode = errorCode ?? null;
      turn.sendErrorMessage = errorMessage ?? null;
      if (["failed", "rejected", "expired"].includes(status)) turn.status = "failed";
      if (status === "unknown") turn.status = "unknown";
      if (status === "completed") this.pending.delete(commandId);
      this.bump();
    }
  }

  removeOptimistic(threadId: string, turnId: string) {
    const live = this.get(threadId);
    const turn = live.byId[turnId];
    if (!turn || turn.items.length > 0 || !turn.sendState) return;
    for (const [key, candidate] of Object.entries(live.byId)) {
      if (candidate === turn) delete live.byId[key];
    }
    live.turns = live.turns.filter((candidate) => candidate !== turn);
    for (const [commandId, loc] of this.pending) {
      if (loc.threadId === threadId && loc.turnId === turnId) this.pending.delete(commandId);
    }
    this.bump();
  }

  /** optimistic echo for a steer sent mid-turn; rendered inline in order */
  appendSteerEcho(threadId: string, text: string) {
    const occurredAt = new Date().toISOString();
    const turn =
      this.currentTurn(threadId, null) ?? this.ensureTurn(threadId, null, occurredAt, 0);
    const key = `steer:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`;
    const item = this.ensureItem(turn, key, "user", occurredAt, Number.MAX_SAFE_INTEGER);
    item.text = text;
    item.attachments = attachments;
    item.title = clientMessageId ?? "";
    item.status = "completed";
    this.bump();
  }

  private consumeSteerOptimistic(
    threadId: string,
    text: string,
    clientMessageId: string | null,
  ): { text: string; attachments: AttachmentView[] } | null {
    const live = this.get(threadId);
    const pendingIds = new Set(
      [...this.pending.values()]
        .filter((location) => location.threadId === threadId)
        .map((location) => location.turnId),
    );
    const optimistic = live.turns.find((turn) =>
      turn.id.startsWith("local:")
      && pendingIds.has(turn.id)
      && (clientMessageId
        ? turn.clientMessageId === clientMessageId
        : turn.userText === text),
    );
    if (!optimistic) return null;
    delete live.byId[optimistic.id];
    live.turns = live.turns.filter((turn) => turn !== optimistic);
    for (const [commandId, location] of this.pending) {
      if (location.threadId === threadId && location.turnId === optimistic.id) {
        this.pending.delete(commandId);
      }
    }
    return {
      text: optimistic.userText ?? "",
      attachments: optimistic.userAttachments,
    };
  }

  /** drop optimistic echoes once the authoritative turn event arrives */
  private adoptOptimistic(
    threadId: string,
    realTurnId: string,
    text: string | null,
    occurredAt: string,
    sequence: number,
  ): LiveTurn {
    const live = this.get(threadId);
    if (text) {
      const pendingIds = new Set(
        [...this.pending.values()]
          .filter((location) => location.threadId === threadId)
          .map((location) => location.turnId),
      );
      const candidates = live.turns
        .filter(
          (turn) =>
            turn.id.startsWith("local:") &&
            turn.userText === text &&
            turn.items.length === 0 &&
            !["failed", "rejected", "unknown", "expired"].includes(turn.sendState ?? ""),
        )
        .filter((turn) => {
          const distance = Math.abs(Date.parse(turn.startedAt) - Date.parse(occurredAt));
          return !Number.isFinite(distance) || distance < 120_000;
        })
        .sort((left, right) => {
          const pendingDelta = Number(pendingIds.has(right.id)) - Number(pendingIds.has(left.id));
          if (pendingDelta !== 0) return pendingDelta;
          const leftDistance = Math.abs(Date.parse(left.startedAt) - Date.parse(occurredAt));
          const rightDistance = Math.abs(Date.parse(right.startedAt) - Date.parse(occurredAt));
          if (leftDistance !== rightDistance) return leftDistance - rightDistance;
          return compareLiveTurns(left, right);
        });
      const candidate = candidates[0];
      if (candidate) {
        const optimisticId = candidate.id;
        delete live.byId[optimisticId];
        const idx = live.turns.indexOf(candidate);
        candidate.id = realTurnId;
        candidate.renderKey ??= optimisticId;
        candidate.startedAt = occurredAt;
        candidate.startedSequence = sequence;
        live.byId[realTurnId] = candidate;
        if (idx >= 0) live.turns[idx] = candidate;
        for (const loc of this.pending.values()) {
          if (loc.threadId === threadId && loc.turnId === optimisticId) {
            loc.turnId = realTurnId;
          }
        }
        return candidate;
      }
    }
    let turn = live.byId[realTurnId];
    if (!turn) {
      turn = {
        id: realTurnId,
        status: "running",
        userText: text,
        userAttachments: [],
        clientMessageId,
        sendState: null,
        sendErrorCode: null,
        sendErrorMessage: null,
        items: [],
        itemIndex: {},
        startedAt: occurredAt,
        startedSequence: sequence,
      };
      live.byId[realTurnId] = turn;
      live.turns.push(turn);
    }
    return turn;
  }

  apply(event: NuntiusEvent) {
    // Both the journal/live overlap and transport retries can deliver the same
    // event more than once. Delta events are not idempotent, so dedupe before
    // looking at the payload.
    if (this.seenEvents.has(event.eventId)) return;
    this.seenEvents.add(event.eventId);
    this.seenEventOrder.push(event.eventId);
    if (this.seenEventOrder.length > 5_000) {
      for (const id of this.seenEventOrder.splice(0, 1_000)) this.seenEvents.delete(id);
    }
    const threadId = event.threadId;
    if (!threadId) return;
    const type = event.eventType;
    const payload = (event.payload ?? {}) as Record<string, unknown>;

    if (type === "turn.started") {
      const p = event.payload as TurnStartedPayload;
      const turnId = event.turnId ?? `anon:${event.eventId}`;
      const turn = this.adoptOptimistic(
        threadId,
        turnId,
        p?.text ?? null,
        event.occurredAt,
        event.seq,
      );
      turn.status = "running";
      if (p?.text) turn.userText = p.text;
      turn.startedAt = event.occurredAt;
      turn.startedSequence = event.seq;
      turn.sendState = "completed";
      this.bump();
      return;
    }
    if (type === "turn.steered") {
      const text = typeof payload.text === "string" ? payload.text : "";
      const clientMessageId = typeof payload.clientMessageId === "string"
        ? payload.clientMessageId
        : null;
      const optimistic = this.consumeSteerOptimistic(threadId, text, clientMessageId);
      this.appendSteerEcho(
        threadId,
        text || optimistic?.text || "",
        Array.isArray(payload.attachments)
          ? payload.attachments as AttachmentView[]
          : optimistic?.attachments ?? [],
        clientMessageId,
      );
      return;
    }
    if (type === "approval.requested") {
      const p = event.payload as ApprovalRequestedPayload;
      const turn = this.currentTurn(threadId, event.turnId);
      if (turn) turn.status = "waiting_approval";
      void p;
      this.bump();
      return;
    }
    if (type.startsWith("agent.")) {
      const method = type.slice("agent.".length).toLowerCase();
      const turn = this.ensureTurn(threadId, event.turnId ?? str(payload.turnId));
      if (method === "turn.started") {
        turn.status = "running";
        this.bump();
        return;
      }
      if (method === "turn.ended") {
        const reason = str(payload.reason)?.toLowerCase();
        this.finalizeTurn(
          turn,
          reason === "cancelled"
            ? "interrupted"
            : reason === "failed" || reason === "blocked"
              ? "failed"
              : "completed",
        );
        this.bump();
        return;
      }
      if (method === "event.session.work_changed") {
        if (payload.busy === true) turn.status = "running";
        else this.finalizeTurn(turn, "completed");
        this.bump();
        return;
      }
      if (method === "assistant.delta" || method === "thinking.delta") {
        const delta = str(payload.delta) ?? "";
        if (!delta) return;
        const item = this.ensureItem(
          turn,
          `${event.turnId ?? "current"}:${method}`,
          method === "thinking.delta" ? "reasoning" : "agent",
        );
        item.text += delta;
        item.status = "running";
        this.scheduleBump();
        return;
      }
      if (method === "tool.call.started" || method === "tool.progress" || method === "tool.result") {
        const key = str(payload.toolCallId) ?? `tool:${event.eventId}`;
        const item = this.ensureItem(turn, key, "tool");
        item.title = str(payload.name) ?? (item.title || "工具");
        const detail = method === "tool.call.started"
          ? str(payload.description) ?? printable(payload.args)
          : method === "tool.progress"
            ? printable(payload.update)
            : printable(payload.output) ?? printable(payload.result);
        if (detail) {
          item.text = method === "tool.progress"
            ? `${item.text}${item.text ? "\n" : ""}${detail}`
            : detail;
        }
        item.status = method === "tool.result" ? "completed" : "running";
        this.bump();
        return;
      }
      return;
    }
    if (!type.startsWith("app_server.")) return;

    const method = type.slice("app_server.".length).toLowerCase();

    if (method.startsWith("turn.")) {
      const turn = this.ensureTurn(
        threadId,
        event.turnId ?? str(payload.turnId) ?? str(payload, "turn", "id"),
        event.occurredAt,
        event.seq,
      );
      if (method === "turn.started") {
        turn.status = "running";
      } else if (method === "turn.completed") {
        // codex signals failures as turn/completed carrying turn.error
        const turnStatus = str(payload, "turn", "status")?.toLowerCase();
        const hasError = Boolean(
          payload.turn && typeof payload.turn === "object" && (payload.turn as Record<string, unknown>).error,
        );
        this.finalizeTurn(
          turn,
          hasError || turnStatus === "failed"
            ? "failed"
            : turnStatus === "interrupted"
              ? "interrupted"
              : "completed",
        );
      } else if (method === "turn.failed" || method === "turn.error") {
        this.finalizeTurn(turn, "failed");
      } else if (method.startsWith("turn.interrupt")) {
        this.finalizeTurn(turn, "interrupted");
      }
      this.bump();
      return;
    }

    if (method === "thread.status.changed") {
      // codex 在 Turn 结束（含中断）后将线程置为 idle/systemError；
      // 部分终态没有单独的 turn.completed 通知，以此兜底收敛 live 状态
      const threadState = str(payload, "status", "type")?.toLowerCase();
      if (threadState && threadState !== "active") {
        const turn = this.currentTurn(threadId, null);
        if (turn) {
          this.finalizeTurn(turn, threadState === "idle" ? "completed" : "failed");
          this.bump();
        }
      }
      return;
    }

    if (method.endsWith("/delta") || method.endsWith(".delta")) {
      const delta =
        str(payload.delta) ?? str(payload.text) ?? str(payload.output) ?? "";
      if (!delta) return;
      const turn = this.ensureTurn(
        threadId,
        event.turnId ?? str(payload.turnId),
        event.occurredAt,
        event.seq,
      );
      const key =
        str(payload.itemId) ?? str(payload, "item", "id") ?? `delta:${method}`;
      const kind = kindForDelta(method);
      const item = this.ensureItem(turn, key, kind, event.occurredAt, event.seq);
      item.text += delta;
      item.status = "running";
      this.scheduleBump();
      return;
    }

    if (method === "item.started" || method === "item.completed") {
      const rawItem = (payload.item ?? payload) as Record<string, unknown>;
      const turn = this.ensureTurn(
        threadId,
        event.turnId ?? str(payload.turnId) ?? str(rawItem.turnId),
        event.occurredAt,
        event.seq,
      );
      const key = str(rawItem.id) ?? `item:${event.eventId}`;
      const kind = kindOfItem(rawItem, method);
      const item = this.ensureItem(turn, key, kind, event.occurredAt, event.seq);
      const finalText = textOfItem(rawItem);
      if (finalText) item.text = finalText;
      const title = titleOfItem(rawItem, kind);
      if (title) item.title = title;
      const files = filesOfItem(rawItem);
      if (files.length) item.files = files;
      if (method === "item.completed") {
        item.status = statusOfItem(rawItem);
      } else if (item.status !== "running") {
        item.status = "running";
      }
      if (turn.status !== "waiting_approval") turn.status = "running";
      this.bump();
      return;
    }
  }

  private currentTurn(threadId: string, hint: string | null): LiveTurn | null {
    const live = this.get(threadId);
    if (hint && live.byId[hint]) return live.byId[hint];
    const open = live.turns
      .filter((turn) => !TERMINAL.has(turn.status))
      .sort(compareLiveTurns);
    for (let i = open.length - 1; i >= 0; i--) {
      const t = open[i];
      if (!TERMINAL.has(t.status)) {
        // Alias app-server turn ids onto the open turn: events carry either the
        // local turn id (turn.started) or the app id (item/turn notifications),
        // and both must aggregate into one live turn.
        if (hint) live.byId[hint] = t;
        return t;
      }
    }
    return null;
  }

  private ensureTurn(
    threadId: string,
    hint: string | null,
    occurredAt: string,
    sequence: number,
  ): LiveTurn {
    const existing = this.currentTurn(threadId, hint);
    if (existing) return existing;
    return this.adoptOptimistic(
      threadId,
      hint ?? `anon:${Date.now()}`,
      null,
      occurredAt,
      sequence,
    );
  }

  private ensureItem(
    turn: LiveTurn,
    key: string,
    kind: LiveKind,
    occurredAt: string,
    sequence: number,
  ): LiveItem {
    const idx = turn.itemIndex[key];
    if (idx !== undefined) {
      const item = turn.items[idx];
      // a real item may replace a delta-created placeholder kind
      if (item.kind === "other" && kind !== "other") item.kind = kind;
      if (compareChronology(occurredAt, sequence, key, item.occurredAt, item.sequence, item.key) < 0) {
        item.occurredAt = occurredAt;
        item.sequence = sequence;
      }
      return item;
    }
    const item: LiveItem = {
      key,
      kind,
      title: "",
      text: "",
      status: "running",
      files: [],
      occurredAt,
      sequence,
    };
    turn.itemIndex[key] = turn.items.length;
    turn.items.push(item);
    return item;
  }

  private finalizeTurn(turn: LiveTurn, status: LiveTurnStatus) {
    turn.status = status;
    for (const item of turn.items) {
      if (item.status === "running") {
        item.status = status === "completed" ? "completed" : "failed";
      }
    }
  }

  isTerminal(turn: LiveTurn): boolean {
    return TERMINAL.has(turn.status);
  }
}

export function compareChronology(
  leftAt: string | null,
  leftSequence: number,
  leftId: string,
  rightAt: string | null,
  rightSequence: number,
  rightId: string,
): number {
  const leftTime = leftAt ? Date.parse(leftAt) : Number.NaN;
  const rightTime = rightAt ? Date.parse(rightAt) : Number.NaN;
  if (Number.isFinite(leftTime) && Number.isFinite(rightTime) && leftTime !== rightTime) {
    return leftTime - rightTime;
  }
  if (Number.isFinite(leftTime) !== Number.isFinite(rightTime)) {
    return Number.isFinite(leftTime) ? 1 : -1;
  }
  if (leftSequence !== rightSequence) return leftSequence - rightSequence;
  return leftId.localeCompare(rightId);
}

export function compareLiveTurns(left: LiveTurn, right: LiveTurn): number {
  return compareChronology(
    left.startedAt,
    left.startedSequence,
    left.id,
    right.startedAt,
    right.startedSequence,
    right.id,
  );
}

export function orderedLiveItems(turn: LiveTurn): LiveItem[] {
  return [...turn.items].sort((left, right) =>
    compareChronology(
      left.occurredAt,
      left.sequence,
      left.key,
      right.occurredAt,
      right.sequence,
      right.key,
    ),
  );
}

/* ---------- payload extraction helpers ---------- */

function str(v: unknown, ...path: string[]): string | null {
  let cur: unknown = v;
  for (const key of path) {
    if (cur && typeof cur === "object") cur = (cur as Record<string, unknown>)[key];
    else return null;
  }
  if (typeof cur === "string" && cur) return cur;
  return null;
}

function printable(value: unknown): string | null {
  if (typeof value === "string") return value || null;
  if (value === null || value === undefined) return null;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function kindOfItem(item: Record<string, unknown>, method: string): LiveKind {
  const t = (str(item.type) ?? str(item.kind) ?? method).toLowerCase();
  if (t.includes("usermessage") || t.includes("user_message")) return "user";
  if (t.includes("reason")) return "reasoning";
  if (t.includes("command") || t.includes("exec") || t.includes("shell")) return "command";
  if (t.includes("file") || t.includes("patch")) return "file";
  if (t.includes("mcp") || t.includes("tool")) return "tool";
  if (t.includes("plan")) return "plan";
  if (t.includes("agent") || t.includes("message")) return "agent";
  return "other";
}

function kindForDelta(method: string): LiveKind {
  if (method.includes("reason")) return "reasoning";
  if (method.includes("command") || method.includes("exec")) return "command";
  if (method.includes("file") || method.includes("patch")) return "file";
  if (method.includes("mcp") || method.includes("tool")) return "tool";
  if (method.includes("plan")) return "plan";
  return "agent";
}

function textOfItem(item: Record<string, unknown>): string | null {
  const direct = str(item.text);
  if (direct) return direct;
  const content = item.content;
  if (Array.isArray(content)) {
    const parts = content
      .map((c) => (c && typeof c === "object" ? str(c.text) : null))
      .filter((x): x is string => Boolean(x));
    if (parts.length) return parts.join("\n");
  }
  const output = str(item.output) ?? str(item.aggregatedOutput);
  return output;
}

function titleOfItem(item: Record<string, unknown>, kind: LiveKind): string | null {
  if (kind === "command") {
    const cmd = item.command ?? item.cmd ?? item.argv;
    if (typeof cmd === "string") return cmd;
    if (Array.isArray(cmd)) return cmd.map(String).join(" ");
    if (cmd && typeof cmd === "object") {
      const inner = (cmd as Record<string, unknown>).command;
      if (typeof inner === "string") return inner;
      if (Array.isArray(inner)) return inner.map(String).join(" ");
    }
    return str(item.name) ?? str(item.title);
  }
  if (kind === "tool") return str(item.name) ?? str(item.toolName) ?? str(item.title);
  return str(item.title) ?? str(item.name);
}

function filesOfItem(item: Record<string, unknown>): LiveFile[] {
  const changes = item.changes ?? item.files;
  if (!Array.isArray(changes)) return [];
  const out: LiveFile[] = [];
  for (const c of changes) {
    if (!c || typeof c !== "object") continue;
    const rec = c as Record<string, unknown>;
    const path = str(rec.path) ?? str(rec.file) ?? str(rec.name);
    if (!path) continue;
    const rawKind = (str(rec.kind) ?? str(rec.type) ?? str(rec.status) ?? "").toLowerCase();
    const kind: LiveFile["kind"] = rawKind.includes("add") || rawKind.includes("create")
      ? "add"
      : rawKind.includes("del") || rawKind.includes("remove")
        ? "del"
        : "mod";
    out.push({ path, kind });
  }
  return out;
}

function statusOfItem(item: Record<string, unknown>): LiveStatus {
  const s = (str(item.status) ?? "").toLowerCase();
  if (s.includes("fail") || s.includes("error")) return "failed";
  if (s.includes("declin") || s.includes("denied") || s.includes("reject")) return "declined";
  if (s.includes("progress") || s.includes("running")) return "running";
  if (s.includes("unknown")) return "unknown";
  return "completed";
}

function formatUnknown(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
