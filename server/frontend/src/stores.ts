/* App-wide stores: session, routing, theme, approvals, commands, live stream. */
import { create } from "zustand";
import { useSyncExternalStore } from "react";
import {
  ThreadLiveStore,
  type ApprovalState,
  type ApprovalView,
  type CommandStatus,
  type Theme,
  type WebSessionView,
} from "@nuntius/shared";

/* ---------- live stream (singleton) ---------- */
export const liveStore = new ThreadLiveStore();
export function useThreadLive(threadId: string | null) {
  useSyncExternalStore(liveStore.subscribe, liveStore.getVersion);
  return threadId ? liveStore.get(threadId) : { turns: [], byId: {} };
}

/* ---------- session ---------- */
interface SessionState {
  session: WebSessionView | null;
  setSession: (s: WebSessionView | null) => void;
}
export const useSession = create<SessionState>((set) => ({
  session: null,
  setSession: (session) => set({ session }),
}));

/* ---------- theme ---------- */
interface ThemeState {
  theme: Theme;
  setTheme: (t: Theme) => void;
}
export const useThemeStore = create<ThemeState>((set) => ({
  theme: (localStorage.getItem("nuntius:theme") as Theme) || "auto",
  setTheme: (theme) => {
    localStorage.setItem("nuntius:theme", theme);
    set({ theme });
  },
}));

/* ---------- router ---------- */
export type Route =
  | { name: "devices" }
  | { name: "recents" }
  | { name: "approvals" }
  | { name: "settings" }
  | { name: "device"; deviceId: string }
  | { name: "project"; deviceId: string; projectId: string }
  | { name: "thread"; deviceId: string; projectId: string; threadId: string };

export function routeToPath(r: Route): string {
  switch (r.name) {
    case "devices":
      return "/";
    case "recents":
      return "/recents";
    case "approvals":
      return "/approvals";
    case "settings":
      return "/settings";
    case "device":
      return `/d/${r.deviceId}`;
    case "project":
      return `/d/${r.deviceId}/p/${r.projectId}`;
    case "thread":
      return `/d/${r.deviceId}/p/${r.projectId}/t/${r.threadId}`;
  }
}

export function pathToRoute(path: string): Route {
  const seg = path.split("/").filter(Boolean);
  if (seg.length === 0) return { name: "devices" };
  if (seg.length === 1 && seg[0] === "recents") return { name: "recents" };
  if (seg.length === 1 && seg[0] === "approvals") return { name: "approvals" };
  if (seg.length === 1 && seg[0] === "settings") return { name: "settings" };
  if (seg.length === 2 && seg[0] === "d") {
    return { name: "device", deviceId: seg[1] };
  }
  if (seg.length === 4 && seg[0] === "d" && seg[2] === "p") {
    return { name: "project", deviceId: seg[1], projectId: seg[3] };
  }
  if (seg.length === 6 && seg[0] === "d" && seg[2] === "p" && seg[4] === "t") {
    return { name: "thread", deviceId: seg[1], projectId: seg[3], threadId: seg[5] };
  }
  return { name: "devices" };
}

function routeFromLocation(): Route {
  const route = pathToRoute(window.location.pathname);
  const canonicalPath = routeToPath(route);
  if (window.location.pathname !== canonicalPath) {
    window.history.replaceState(route, "", canonicalPath);
  }
  return route;
}

interface RouteState {
  route: Route;
  navigate: (r: Route, opts?: { replace?: boolean }) => void;
  back: (fallback: Route) => void;
}
export const useRoute = create<RouteState>((set) => ({
  route: routeFromLocation(),
  navigate: (route, opts) => {
    const path = routeToPath(route);
    if (opts?.replace) window.history.replaceState(route, "", path);
    else window.history.pushState(route, "", path);
    set({ route });
  },
  back: (fallback) => {
    if (window.history.length > 1) window.history.back();
    else {
      window.history.replaceState(fallback, "", routeToPath(fallback));
      set({ route: fallback });
    }
  },
}));
window.addEventListener("popstate", () => {
  useRoute.setState({ route: routeFromLocation() });
});

/* ---------- approvals ---------- */
interface ApprovalsState {
  items: Record<string, ApprovalView>;
  order: string[];
  add: (a: ApprovalView) => void;
  setState: (id: string, state: ApprovalState, decidedAs?: string) => void;
  cancelForThread: (threadId: string) => void;
}
const APPROVALS_KEY = "nuntius:approvals:v1";

function loadApprovals(): Pick<ApprovalsState, "items" | "order"> {
  try {
    const raw = localStorage.getItem(APPROVALS_KEY);
    if (!raw) return { items: {}, order: [] };
    const parsed = JSON.parse(raw) as { items: Record<string, ApprovalView>; order: string[] };
    // prune resolved entries older than 6h
    const cutoff = Date.now() - 6 * 3600_000;
    const items: Record<string, ApprovalView> = {};
    const order: string[] = [];
    for (const id of parsed.order) {
      const a = parsed.items[id];
      if (!a) continue;
      if (a.state !== "pending" && Date.parse(a.occurredAt) < cutoff) continue;
      items[id] = a;
      order.push(id);
    }
    return { items, order };
  } catch {
    return { items: {}, order: [] };
  }
}
function persistApprovals(s: Pick<ApprovalsState, "items" | "order">) {
  try {
    localStorage.setItem(APPROVALS_KEY, JSON.stringify({ items: s.items, order: s.order.slice(-80) }));
  } catch {
    /* full / unavailable */
  }
}

export const useApprovals = create<ApprovalsState>((set) => ({
  ...loadApprovals(),
  add: (a) =>
    set((s) => {
      if (s.items[a.id]) return s;
      const next = {
        items: { ...s.items, [a.id]: a },
        order: [...s.order.filter((x) => x !== a.id), a.id],
      };
      persistApprovals(next);
      return next;
    }),
  setState: (id, state, decidedAs) =>
    set((s) => {
      const cur = s.items[id];
      if (!cur) return s;
      const next = {
        ...s,
        items: { ...s.items, [id]: { ...cur, state, decidedAs: decidedAs ?? cur.decidedAs } },
      };
      persistApprovals(next);
      return next;
    }),
  cancelForThread: (threadId) =>
    set((s) => {
      const items = { ...s.items };
      let changed = false;
      for (const [id, a] of Object.entries(items)) {
        if (a.threadId === threadId && a.state === "pending") {
          items[id] = { ...a, state: "cancelled" };
          changed = true;
        }
      }
      if (!changed) return s;
      const next = { ...s, items };
      persistApprovals(next);
      return next;
    }),
}));

export const usePendingApprovalCount = () =>
  useApprovals((s) => s.order.filter((id) => s.items[id]?.state === "pending").length);

/* ---------- command tracking ---------- */
interface CommandState {
  byId: Record<string, { status: CommandStatus; threadId?: string; kind?: string }>;
  track: (commandId: string, threadId?: string, kind?: string) => void;
  apply: (commandId: string, status: CommandStatus) => void;
}
export const useCommands = create<CommandState>((set) => ({
  byId: {},
  track: (commandId, threadId, kind) =>
    set((s) => ({ byId: { ...s.byId, [commandId]: { status: "accepted", threadId, kind } } })),
  apply: (commandId, status) =>
    set((s) => {
      const cur = s.byId[commandId];
      if (!cur) return s;
      return { byId: { ...s.byId, [commandId]: { ...cur, status } } };
    }),
}));
