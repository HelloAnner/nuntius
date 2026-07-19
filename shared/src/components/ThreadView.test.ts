import { describe, expect, test } from "bun:test";
import type { LiveItem, LiveTurn } from "../stream";
import type { HistoryGroup } from "./ThreadView";
import { optimisticEchoIsInHistory, visibleLiveItems } from "./ThreadView";

function item(key: string, kind: LiveItem["kind"], text: string): LiveItem {
  return { key, kind, text, title: "", status: "completed", files: [] };
}

function turn(overrides: Partial<LiveTurn> = {}): LiveTurn {
  return {
    id: "trn_live",
    status: "running",
    userText: "帮我检查一下",
    sendState: "completed",
    sendErrorCode: null,
    sendErrorMessage: null,
    items: [],
    itemIndex: {},
    startedAt: "2026-07-19T10:00:00.000Z",
    ...overrides,
  };
}

function historyGroup(text: string, startedAt: string | null): HistoryGroup {
  return {
    turn: {
      id: "trn_history",
      ordinal: 1,
      status: "active",
      startedAt,
      completedAt: null,
    },
    items: [{ id: "itm_user", kind: "user_message", text, status: "completed" }],
  };
}

describe("ThreadView live/history reconciliation", () => {
  test("does not render the App Server echo of the turn's initial prompt twice", () => {
    const liveTurn = turn({
      items: [
        item("app-user", "user", "帮我检查一下"),
        item("app-agent", "agent", "正在检查"),
      ],
    });

    expect(visibleLiveItems(liveTurn).map((entry) => entry.key)).toEqual(["app-agent"]);
  });

  test("keeps a mid-turn steering echo even when its text matches the initial prompt", () => {
    const liveTurn = turn({
      items: [
        item("app-user", "user", "帮我检查一下"),
        item("steer:local", "user", "帮我检查一下"),
      ],
    });

    expect(visibleLiveItems(liveTurn).map((entry) => entry.key)).toEqual(["steer:local"]);
  });

  test("only reconciles an orphan optimistic echo with a recent matching history turn", () => {
    const optimistic = turn({ id: "local:pending:1", items: [] });
    const recent = historyGroup("帮我检查一下", "2026-07-19T10:00:20.000Z");
    const old = historyGroup("帮我检查一下", "2026-07-19T09:00:00.000Z");

    expect(optimisticEchoIsInHistory(optimistic, [recent])).toBe(true);
    expect(optimisticEchoIsInHistory(optimistic, [old])).toBe(false);
  });
});
