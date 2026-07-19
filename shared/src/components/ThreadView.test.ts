import { describe, expect, test } from "bun:test";
import { ThreadLiveStore, type LiveItem, type LiveTurn } from "../stream";
import type { HistoryGroup } from "./ThreadView";
import {
  liveTurnIsInHistory,
  optimisticEchoIsInHistory,
  visibleHistoryItems,
  visibleLiveItems,
} from "./ThreadView";

function item(key: string, kind: LiveItem["kind"], text: string): LiveItem {
  return { key, kind, text, title: "", status: "completed", files: [], attachments: [] };
}

function turn(overrides: Partial<LiveTurn> = {}): LiveTurn {
  return {
    id: "trn_live",
    status: "running",
    userText: "帮我检查一下",
    userAttachments: [],
    clientMessageId: null,
    sendState: "completed",
    sendErrorCode: null,
    sendErrorMessage: null,
    items: [],
    itemIndex: {},
    startedAt: "2026-07-19T10:00:00.000Z",
    ...overrides,
  };
}

function historyGroup(
  text: string,
  startedAt: string | null,
  agentTexts: string[] = [],
): HistoryGroup {
  return {
    turn: {
      id: "trn_history",
      ordinal: 1,
      status: "active",
      startedAt,
      completedAt: null,
    },
    items: [
      { id: "itm_user", kind: "user_message", text, status: "completed", attachments: [] },
      ...agentTexts.map((agentText, index) => ({
        id: `itm_agent_${index}`,
        kind: "agent_message" as const,
        text: agentText,
        status: "completed",
        attachments: [],
      })),
    ],
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

  test("reconciles a returned reply when replay uses a different turn id", () => {
    const liveTurn = turn({
      id: "app-turn-id",
      items: [item("app-agent", "agent", "检查完成，没有发现问题。")],
    });
    const persisted = historyGroup(
      "帮我检查一下",
      "2026-07-19T10:00:20.000Z",
      ["检查完成，没有发现问题。"],
    );

    expect(liveTurnIsInHistory(liveTurn, [persisted])).toBe(true);
    expect(
      liveTurnIsInHistory(
        { ...liveTurn, items: [item("app-agent", "agent", "仍在检查中")] },
        [persisted],
      ),
    ).toBe(false);
    expect(
      liveTurnIsInHistory(
        {
          ...liveTurn,
          items: [
            item("app-agent", "agent", "检查完成，没有发现问题。"),
            item("steer:local", "user", "再检查一次"),
          ],
        },
        [persisted],
      ),
    ).toBe(false);
  });

  test("renders identical persisted assistant rows only once", () => {
    const duplicated = historyGroup(
      "帮我检查一下",
      "2026-07-19T10:00:20.000Z",
      ["检查完成。", "检查完成。"],
    );

    expect(visibleHistoryItems(duplicated.items).map((entry) => entry.id)).toEqual([
      "itm_user",
      "itm_agent_0",
    ]);
  });

  test("replaces an image steer provisional turn with one inline echo", () => {
    const store = new ThreadLiveStore();
    const attachment = {
      id: "att_one",
      originalName: "one.png",
      mimeType: "image/png",
      byteSize: 123,
      sha256: "a".repeat(64),
      width: 20,
      height: 10,
    };
    store.addOptimistic("thr", "pending", "", [attachment], "client-one");
    store.apply({
      eventId: "evt-steer",
      userId: null,
      deviceId: "dev",
      projectId: "prj",
      threadId: "thr",
      turnId: "turn-active",
      streamId: "stream",
      seq: 1,
      eventType: "turn.steered",
      durability: "durable",
      occurredAt: "2026-07-19T10:00:00.000Z",
      payload: { text: "", attachments: [attachment], clientMessageId: "client-one" },
    });

    const live = store.get("thr");
    expect(live.turns).toHaveLength(1);
    expect(live.turns[0].id.startsWith("local:")).toBe(false);
    expect(live.turns[0].items).toHaveLength(1);
    expect(live.turns[0].items[0].attachments).toEqual([attachment]);
  });
});
