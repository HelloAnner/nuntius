import { describe, expect, test } from "bun:test";
import { ThreadLiveStore, orderedLiveItems, type LiveItem, type LiveTurn } from "../stream";
import type { NuntiusEvent } from "../types";
import type { HistoryGroup } from "./ThreadView";
import {
  liveTurnIsInHistory,
  freshLiveTurnsForHistory,
  orderedHistory,
  liveTurnHasTranscript,
  optimisticEchoIsInHistory,
  visibleHistoryItems,
  visibleLiveItems,
} from "./ThreadView";

function item(key: string, kind: LiveItem["kind"], text: string): LiveItem {
  return {
    key,
    kind,
    text,
    title: "",
    status: "completed",
    files: [],
    occurredAt: "2026-07-19T10:00:01.000Z",
    sequence: 2,
  };
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
    startedSequence: 1,
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
      {
        id: "itm_user",
        ordinal: 1,
        kind: "user_message",
        text,
        status: "completed",
        occurredAt: startedAt ?? "2026-07-19T10:00:00.000Z",
      },
      ...agentTexts.map((agentText, index) => ({
        id: `itm_agent_${index}`,
        ordinal: index + 2,
        kind: "agent_message" as const,
        text: agentText,
        status: "completed",
        occurredAt: new Date(
          Date.parse(startedAt ?? "2026-07-19T10:00:00.000Z") + (index + 1) * 1_000,
        ).toISOString(),
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

  test("hides only the persisted assistant item, not every later live reply", () => {
    const liveTurn = turn({
      items: [
        item("persisted", "agent", "已经入库"),
        { ...item("latest", "agent", "最新回复"), sequence: 3 },
      ],
    });

    expect(
      visibleLiveItems(liveTurn, new Map([["已经入库", 1]])).map((entry) => entry.key),
    ).toEqual(["latest"]);
  });

  test("reconciles identical live replies against history one-to-one", () => {
    const liveTurn = turn({
      items: [
        item("first", "agent", "相同回复"),
        { ...item("second", "agent", "相同回复"), sequence: 3 },
      ],
    });

    expect(
      visibleLiveItems(liveTurn, new Map([["相同回复", 1]])).map((entry) => entry.key),
    ).toEqual(["second"]);
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

  test("keeps legitimate identical persisted replies and only removes repeated ids", () => {
    const repeatedText = historyGroup(
      "帮我检查一下",
      "2026-07-19T10:00:20.000Z",
      ["检查完成。", "检查完成。"],
    );
    const duplicateId = { ...repeatedText.items[1] };

    expect(visibleHistoryItems(repeatedText.items).map((entry) => entry.id)).toEqual([
      "itm_user",
      "itm_agent_0",
      "itm_agent_1",
    ]);
    expect(visibleHistoryItems([...repeatedText.items, duplicateId]).map((entry) => entry.id)).toEqual([
      "itm_user",
      "itm_agent_0",
      "itm_agent_1",
    ]);
  });

  test("orders turns and items by recorded time before ordinal fallback", () => {
    const later = historyGroup("later", "2026-07-19T10:00:20.000Z");
    later.turn.id = "later";
    later.turn.ordinal = 1;
    const earlier = historyGroup("earlier", "2026-07-19T10:00:00.000Z");
    earlier.turn.id = "earlier";
    earlier.turn.ordinal = 2;
    earlier.items.push({
      id: "actually-first",
      ordinal: 99,
      kind: "agent_message",
      text: "first",
      status: "completed",
      occurredAt: "2026-07-19T09:59:59.000Z",
    });

    const ordered = orderedHistory([later, earlier]);
    expect(ordered.map((group) => group.turn.id)).toEqual(["earlier", "later"]);
    expect(ordered[0].items.map((entry) => entry.id)).toEqual(["actually-first", "itm_user"]);
  });

  test("one persisted prompt reconciles only one of two identical optimistic sends", () => {
    const first = turn({ id: "local:first" });
    const second = turn({
      id: "local:second",
      startedAt: "2026-07-19T10:00:30.000Z",
      startedSequence: 2,
    });
    const persisted = historyGroup("帮我检查一下", "2026-07-19T10:00:05.000Z");

    expect(freshLiveTurnsForHistory([persisted], [second, first]).map((entry) => entry.id)).toEqual([
      "local:second",
    ]);
  });

  test("does not render an empty turn divider for tool-only live state", () => {
    expect(liveTurnHasTranscript(turn({ userText: null, items: [] }))).toBe(false);
    expect(
      liveTurnHasTranscript(turn({
        userText: null,
        items: [item("reasoning", "reasoning", "internal")],
      })),
    ).toBe(false);
  });
});

function event(overrides: Partial<NuntiusEvent> = {}): NuntiusEvent {
  return {
    eventId: "evt-default",
    userId: null,
    deviceId: "dev-test",
    projectId: "prj-test",
    threadId: "thr-test",
    turnId: "trn-test",
    streamId: "thread:thr-test",
    seq: 1,
    eventType: "turn.started",
    durability: "durable",
    occurredAt: "2026-07-19T10:00:00.000Z",
    payload: { text: "hello" },
    ...overrides,
  };
}

describe("ThreadLiveStore chronology", () => {
  test("does not duplicate a send when the authoritative event beats the receipt", () => {
    const store = new ThreadLiveStore();
    const occurredAt = new Date().toISOString();
    store.apply(event({
      eventId: "evt-start-first",
      turnId: "trn-authoritative",
      occurredAt,
      payload: { text: "same" },
    }));

    expect(store.addOptimistic("thr-test", "cmd-late-receipt", "same")).toBe(
      "trn-authoritative",
    );
    expect(store.get("thr-test").turns).toHaveLength(1);
  });

  test("adopts the current identical send instead of an older failed echo", () => {
    const store = new ThreadLiveStore();
    const first = store.addOptimistic("thr-test", "cmd-first", "same");
    store.applyCommandStatus("cmd-first", "failed");
    const second = store.addOptimistic("thr-test", "cmd-second", "same");
    const occurredAt = new Date().toISOString();
    store.apply(event({
      eventId: "evt-start-current",
      turnId: "trn-current",
      occurredAt,
      payload: { text: "same" },
    }));

    const live = store.get("thr-test");
    expect(live.byId[first]?.status).toBe("failed");
    expect(live.byId[second]).toBeUndefined();
    expect(live.byId["trn-current"]?.startedAt).toBe(occurredAt);
  });

  test("applies a replayed delta event only once", () => {
    const store = new ThreadLiveStore();
    store.apply(event());
    const delta = event({
      eventId: "evt-delta",
      seq: 2,
      eventType: "app_server.item.output_text.delta",
      occurredAt: "2026-07-19T10:00:01.000Z",
      payload: { itemId: "itm-agent", delta: "完成" },
    });
    store.apply(delta);
    store.apply(delta);

    expect(store.get("thr-test").turns[0].items[0].text).toBe("完成");
  });

  test("orders live items by event time even when they arrive out of order", () => {
    const store = new ThreadLiveStore();
    store.apply(event());
    store.apply(event({
      eventId: "evt-later",
      seq: 3,
      eventType: "app_server.item.completed",
      occurredAt: "2026-07-19T10:00:03.000Z",
      payload: { item: { id: "later", type: "agentMessage", text: "later" } },
    }));
    store.apply(event({
      eventId: "evt-earlier",
      seq: 2,
      eventType: "app_server.item.completed",
      occurredAt: "2026-07-19T10:00:02.000Z",
      payload: { item: { id: "earlier", type: "agentMessage", text: "earlier" } },
    }));

    expect(orderedLiveItems(store.get("thr-test").turns[0]).map((entry) => entry.key)).toEqual([
      "earlier",
      "later",
    ]);
  });
});
