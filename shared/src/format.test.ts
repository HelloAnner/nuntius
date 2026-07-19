import { describe, expect, test } from "bun:test";
import { compareThreadActivity } from "./format";

describe("compareThreadActivity", () => {
  test("orders threads by authoritative activity time", () => {
    const threads = [
      { id: "thread-a", lastActivityAt: "2026-07-19T10:00:00Z" },
      { id: "thread-b", lastActivityAt: "2026-07-19T10:00:00.500Z" },
    ];

    expect(threads.sort(compareThreadActivity).map((thread) => thread.id)).toEqual([
      "thread-b",
      "thread-a",
    ]);
  });

  test("keeps equal or missing timestamps deterministic", () => {
    const threads = [
      { id: "thread-a", lastActivityAt: null },
      { id: "thread-c", lastActivityAt: "invalid" },
      { id: "thread-b", lastActivityAt: null },
    ];

    expect(threads.sort(compareThreadActivity).map((thread) => thread.id)).toEqual([
      "thread-c",
      "thread-b",
      "thread-a",
    ]);
  });
});
