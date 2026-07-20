export type RecentStatusFilter = "all" | "running" | "approval" | "idle" | "archived";

export interface RecentFilterPreferences {
  deviceFilter: string;
  projectFilter: string;
  statusFilter: RecentStatusFilter;
}

type PreferenceStorage = Pick<Storage, "getItem" | "setItem">;

const STORAGE_KEY = "nuntius:recents-filters:v1";
const MAX_FILTER_VALUE_LENGTH = 256;
const STATUS_FILTERS = new Set<RecentStatusFilter>([
  "all",
  "running",
  "approval",
  "idle",
  "archived",
]);

export const DEFAULT_RECENT_FILTER_PREFERENCES: RecentFilterPreferences = {
  deviceFilter: "all",
  projectFilter: "all",
  statusFilter: "all",
};

export function loadRecentFilterPreferences(
  storage: PreferenceStorage = localStorage,
): RecentFilterPreferences {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_RECENT_FILTER_PREFERENCES };
    const parsed = JSON.parse(raw) as Partial<Record<keyof RecentFilterPreferences, unknown>>;
    return normalizeRecentFilterPreferences(parsed);
  } catch {
    return { ...DEFAULT_RECENT_FILTER_PREFERENCES };
  }
}

export function saveRecentFilterPreferences(
  preferences: RecentFilterPreferences,
  storage: PreferenceStorage = localStorage,
) {
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
  } catch {
    // In-memory state still works when browser storage is unavailable or full.
  }
}

function normalizeRecentFilterPreferences(
  value: Partial<Record<keyof RecentFilterPreferences, unknown>>,
): RecentFilterPreferences {
  return {
    deviceFilter: normalizeSelection(value.deviceFilter),
    projectFilter: normalizeSelection(value.projectFilter),
    statusFilter: isRecentStatusFilter(value.statusFilter) ? value.statusFilter : "all",
  };
}

function normalizeSelection(value: unknown) {
  return typeof value === "string" && value.length > 0 && value.length <= MAX_FILTER_VALUE_LENGTH
    ? value
    : "all";
}

export function isRecentStatusFilter(value: unknown): value is RecentStatusFilter {
  return typeof value === "string" && STATUS_FILTERS.has(value as RecentStatusFilter);
}
