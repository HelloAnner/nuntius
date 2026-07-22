import { describe, expect, test } from "bun:test";
import {
  compareThreadCreation,
  compareThreadStatusCreation,
  isRunningStatus,
  statusLabel,
  truncateEnd,
} from "./format";

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

describe("runtime status", () => {
  test("normalizes provider-native running states", () => {
    expect(["active", "running", "inProgress"].every(isRunningStatus)).toBe(true);
    expect(isRunningStatus("notLoaded")).toBe(false);
    expect(statusLabel("inProgress")).toBe("运行中");
    expect(statusLabel("stalled")).toBe("长时间无活动");
  });

  test("orders running threads first and uses creation time within each priority", () => {
    const threads = [
      { id: "idle-new", status: "idle", createdAt: "2026-07-22T10:00:00Z" },
      { id: "running-old", status: "active", createdAt: "2026-07-20T10:00:00Z" },
      { id: "idle-old", status: "completed", createdAt: "2026-07-21T10:00:00Z" },
      { id: "running-new", status: "running", createdAt: "2026-07-22T09:00:00Z" },
    ];

    expect(threads.sort(compareThreadStatusCreation).map((thread) => thread.id)).toEqual([
      "running-new",
      "running-old",
      "idle-new",
      "idle-old",
    ]);
  });
});
