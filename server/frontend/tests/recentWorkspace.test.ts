import { describe, expect, test } from "bun:test";
import {
  loadLastRecentThreadId,
  recentThreadDisplayTimestamp,
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
  lastActivityAt: string | null = null,
  needsReview = false,
) => ({ id, status, needsReview, createdAt, archived, lastActivityAt });

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

  test("places unseen completed work after running and before idle threads", () => {
    const threads = [
      thread("idle-new", "idle", "2026-07-22T10:00:00Z"),
      thread("review-old", "idle", "2026-07-20T10:00:00Z", false, null, true),
      thread("running-old", "active", "2026-07-19T10:00:00Z"),
    ];

    expect(selectRecentWorkspaceThread(threads, null)?.id).toBe("running-old");
    expect(selectRecentWorkspaceThread(threads.slice(0, 2), null)?.id).toBe("review-old");
  });

  test("sorts by creation time even when an older thread has newer message activity", () => {
    const threads = [
      thread("created-new", "idle", "2026-07-22T10:00:00Z", false, "2026-07-22T10:01:00Z"),
      thread("active-new", "idle", "2026-07-21T10:00:00Z", false, "2026-07-22T12:00:00Z"),
    ];

    expect(selectRecentWorkspaceThread(threads, null)?.id).toBe("created-new");
  });

  test("displays the latest message activity while falling back to creation time", () => {
    expect(recentThreadDisplayTimestamp(
      thread("active", "idle", "2026-07-20T10:00:00Z", false, "2026-07-22T12:00:00Z"),
    )).toBe("2026-07-22T12:00:00Z");
    expect(recentThreadDisplayTimestamp(
      thread("never-used", "idle", "2026-07-20T10:00:00Z"),
    )).toBe("2026-07-20T10:00:00Z");
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
