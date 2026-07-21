/* Resizable thread-sidebar width preference, persisted per browser. */

export const THREAD_SIDEBAR_DEFAULT_WIDTH = 300;
export const THREAD_SIDEBAR_MIN_WIDTH = 220;
export const THREAD_SIDEBAR_MAX_WIDTH = 520;

const STORAGE_KEY = "nuntius:thread-sidebar-width:v1";

type PreferenceStorage = Pick<Storage, "getItem" | "setItem">;

export function clampThreadSidebarWidth(width: number): number {
  if (!Number.isFinite(width)) return THREAD_SIDEBAR_DEFAULT_WIDTH;
  return Math.min(
    THREAD_SIDEBAR_MAX_WIDTH,
    Math.max(THREAD_SIDEBAR_MIN_WIDTH, Math.round(width)),
  );
}

export function loadThreadSidebarWidth(
  storage: PreferenceStorage = localStorage,
): number {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return THREAD_SIDEBAR_DEFAULT_WIDTH;
    return clampThreadSidebarWidth(Number.parseFloat(raw));
  } catch {
    return THREAD_SIDEBAR_DEFAULT_WIDTH;
  }
}

export function saveThreadSidebarWidth(
  width: number,
  storage: PreferenceStorage = localStorage,
): void {
  try {
    storage.setItem(STORAGE_KEY, String(clampThreadSidebarWidth(width)));
  } catch {
    /* storage unavailable: keep the width session-only */
  }
}
