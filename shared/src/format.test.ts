import { describe, expect, test } from "bun:test";
import { compareThreadCreation } from "./format";

describe("compareThreadCreation", () => {
  test("orders threads by creation time, newest first", () => {
    const threads = [
      { id: "thread-a", createdAt: "2026-07-19T10:00:00Z" },
      { id: "thread-b", createdAt: "2026-07-19T10:00:00.500Z" },
    ];

    expect(threads.sort(compareThreadCreation).map((thread) => thread.id)).toEqual([
      "thread-b",
      "thread-a",
    ]);
  });

  test("keeps equal or missing timestamps deterministic", () => {
    const threads = [
      { id: "thread-a", createdAt: null },
      { id: "thread-c", createdAt: "invalid" },
      { id: "thread-b", createdAt: null },
    ];

    expect(threads.sort(compareThreadCreation).map((thread) => thread.id)).toEqual([
      "thread-c",
      "thread-b",
      "thread-a",
    ]);
  });
});
