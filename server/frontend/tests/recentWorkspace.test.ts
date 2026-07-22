import { describe, expect, test } from "bun:test";
import {
  loadLastRecentThreadId,
  saveLastRecentThreadId,
  selectRecentWorkspaceThread,
} from "../src/recentWorkspace";

class MemoryStorage {
  value: string | null = null;

  getItem() {
    return this.value;
  }

  setItem(_key: string, value: string) {
    this.value = value;
  }
}

const thread = (
  id: string,
  status: string,
  createdAt: string,
  archived = false,
) => ({ id, status, createdAt, archived });

describe("recent workspace selection", () => {
  test("restores the last valid thread before applying the fallback ordering", () => {
    const threads = [
      thread("running", "active", "2026-07-22T09:00:00Z"),
      thread("remembered", "idle", "2026-07-21T09:00:00Z"),
    ];

    expect(selectRecentWorkspaceThread(threads, "remembered")?.id).toBe("remembered");
  });

  test("falls back to running first and newest creation time within the same priority", () => {
    const threads = [
      thread("idle-new", "idle", "2026-07-22T10:00:00Z"),
      thread("running-old", "active", "2026-07-20T10:00:00Z"),
      thread("running-new", "running", "2026-07-21T10:00:00Z"),
    ];

    expect(selectRecentWorkspaceThread(threads, null)?.id).toBe("running-new");
  });

  test("skips archived threads and pending archive intents", () => {
    const threads = [
      thread("archived", "active", "2026-07-22T10:00:00Z", true),
      thread("archiving", "active", "2026-07-22T09:00:00Z"),
      thread("available", "idle", "2026-07-22T08:00:00Z"),
    ];

    expect(
      selectRecentWorkspaceThread(threads, "archived", new Set(["archiving"]))?.id,
    ).toBe("available");
  });

  test("returns no selection when the workspace has no available thread", () => {
    expect(selectRecentWorkspaceThread([], "missing")).toBeNull();
  });

  test("persists the last thread and tolerates unavailable storage", () => {
    const storage = new MemoryStorage();
    saveLastRecentThreadId("thread-42", storage);
    expect(loadLastRecentThreadId(storage)).toBe("thread-42");

    const unavailable = {
      getItem: () => {
        throw new Error("storage disabled");
      },
      setItem: () => {
        throw new Error("storage disabled");
      },
    };
    expect(loadLastRecentThreadId(unavailable)).toBeNull();
    expect(() => saveLastRecentThreadId("thread-42", unavailable)).not.toThrow();
  });
});
