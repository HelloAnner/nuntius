import { describe, expect, test } from "bun:test";
import {
  DEFAULT_RECENT_FILTER_PREFERENCES,
  loadRecentFilterPreferences,
  saveRecentFilterPreferences,
  type RecentFilterPreferences,
} from "../src/recentsFilters";

class MemoryStorage {
  value: string | null = null;

  getItem() {
    return this.value;
  }

  setItem(_key: string, value: string) {
    this.value = value;
  }
}

describe("recent filter preferences", () => {
  test("round-trips the mobile scope and status filters", () => {
    const storage = new MemoryStorage();
    const preferences: RecentFilterPreferences = {
      deviceFilter: "dev-study",
      projectFilter: "dev-study:project-nuntius",
      statusFilter: "running",
    };

    saveRecentFilterPreferences(preferences, storage);

    expect(loadRecentFilterPreferences(storage)).toEqual(preferences);
  });

  test("falls back safely for malformed or unsupported stored values", () => {
    const storage = new MemoryStorage();
    storage.value = JSON.stringify({
      deviceFilter: 42,
      projectFilter: "",
      statusFilter: "unknown",
    });

    expect(loadRecentFilterPreferences(storage)).toEqual(DEFAULT_RECENT_FILTER_PREFERENCES);

    storage.value = "not-json";
    expect(loadRecentFilterPreferences(storage)).toEqual(DEFAULT_RECENT_FILTER_PREFERENCES);
  });

  test("keeps working when browser storage is unavailable", () => {
    const unavailable = {
      getItem: () => {
        throw new Error("storage disabled");
      },
      setItem: () => {
        throw new Error("storage disabled");
      },
    };

    expect(loadRecentFilterPreferences(unavailable)).toEqual(DEFAULT_RECENT_FILTER_PREFERENCES);
    expect(() => saveRecentFilterPreferences(DEFAULT_RECENT_FILTER_PREFERENCES, unavailable)).not.toThrow();
  });
});
