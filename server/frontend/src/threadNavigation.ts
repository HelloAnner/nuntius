import type { Route } from "./stores";

export type ThreadNavigationContext = "project" | "recents";

export function threadRouteForContext(
  context: ThreadNavigationContext,
  threadId: string,
  deviceId: string,
  projectId: string,
): Route {
  return context === "recents"
    ? { name: "recentThread", threadId }
    : { name: "thread", deviceId, projectId, threadId };
}
