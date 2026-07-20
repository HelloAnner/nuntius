import { describe, expect, test } from "bun:test";
import { compareThreadCreation, truncateEnd } from "./format";

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

describe("truncateEnd", () => {
  test("keeps short titles unchanged and shortens long titles from the end", () => {
    expect(truncateEnd("修复登录页", 8)).toBe("修复登录页");
    expect(truncateEnd("梳理并修复登录页面的状态同步问题", 8)).toBe("梳理并修复登录…");
  });

  test("counts unicode characters instead of UTF-16 code units", () => {
    expect(truncateEnd("检查🧪测试流程", 5)).toBe("检查🧪测…");
  });
});
