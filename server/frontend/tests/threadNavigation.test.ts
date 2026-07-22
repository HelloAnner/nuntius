import { describe, expect, test } from "bun:test";
import { threadRouteForContext } from "../src/threadNavigation";

describe("thread creation navigation", () => {
  test("keeps a thread created from recents inside the recent workspace", () => {
    expect(threadRouteForContext("recents", "thread-1", "device-1", "project-1")).toEqual({
      name: "recentThread",
      threadId: "thread-1",
    });
  });

  test("keeps a thread created from a project inside the project workspace", () => {
    expect(threadRouteForContext("project", "thread-1", "device-1", "project-1")).toEqual({
      name: "thread",
      deviceId: "device-1",
      projectId: "project-1",
      threadId: "thread-1",
    });
  });
});
