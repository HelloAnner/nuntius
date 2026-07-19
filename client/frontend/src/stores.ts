/* Local console stores: routing, theme, approvals, live stream. */
import { create } from "zustand";
import { useSyncExternalStore } from "react";
import {
  ThreadLiveStore,
  type ApprovalState,
  type ApprovalView,
  type Theme,
} from "@nuntius/shared";

/* ---------- live stream (singleton) ---------- */
export const liveStore = new ThreadLiveStore();
export function useThreadLive(threadId: string | null) {
  useSyncExternalStore(liveStore.subscribe, liveStore.getVersion);
  return threadId ? liveStore.get(threadId) : { turns: [], byId: {} };
}

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
  | { name: "overview" }
  | { name: "projects" }
  | { name: "threads" }
  | { name: "approvals" }
  | { name: "project"; projectId: string }
  | { name: "thread"; projectId: string; threadId: string };

export function routeToPath(r: Route): string {
  switch (r.name) {
    case "overview":
      return "/";
    case "projects":
      return "/projects";
    case "threads":
      return "/threads";
    case "approvals":
      return "/approvals";
    case "project":
      return `/p/${r.projectId}`;
    case "thread":
      return `/p/${r.projectId}/t/${r.threadId}`;
  }
}

export function pathToRoute(path: string): Route {
  const seg = path.split("/").filter(Boolean);
  if (seg.length === 0) return { name: "overview" };
  if (seg.length === 1 && seg[0] === "projects") return { name: "projects" };
  if (seg.length === 1 && seg[0] === "threads") return { name: "threads" };
  if (seg.length === 1 && seg[0] === "approvals") return { name: "approvals" };
  if (seg.length === 2 && seg[0] === "p") {
    return { name: "project", projectId: seg[1] };
  }
  if (seg.length === 4 && seg[0] === "p" && seg[2] === "t") {
    return { name: "thread", projectId: seg[1], threadId: seg[3] };
  }
  return { name: "overview" };
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
const APPROVALS_KEY = "nuntius:local-approvals:v1";

function loadApprovals(): Pick<ApprovalsState, "items" | "order"> {
  try {
    const raw = localStorage.getItem(APPROVALS_KEY);
    if (!raw) return { items: {}, order: [] };
    const parsed = JSON.parse(raw) as { items: Record<string, ApprovalView>; order: string[] };
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
function persist(s: Pick<ApprovalsState, "items" | "order">) {
  try {
    localStorage.setItem(APPROVALS_KEY, JSON.stringify({ items: s.items, order: s.order.slice(-80) }));
  } catch {
    /* full */
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
      persist(next);
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
      persist(next);
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
      persist(next);
      return next;
    }),
}));

export const usePendingApprovalCount = () =>
  useApprovals((s) => s.order.filter((id) => s.items[id]?.state === "pending").length);
