import { describe, expect, test } from "bun:test";
import {
  clipboardImageFiles,
  loadComposerDraft,
  resolveComposerDraft,
  saveComposerDraft,
} from "./Composer";

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
  };
}

describe("conversation drafts", () => {
  test("keeps unsent text isolated by thread", () => {
    const storage = memoryStorage();

    saveComposerDraft("thread-a", "message for A", storage);
    saveComposerDraft("thread-b", "message for B", storage);

    expect(loadComposerDraft("thread-a", storage)).toBe("message for A");
    expect(loadComposerDraft("thread-b", storage)).toBe("message for B");
  });

  test("resolves the selected thread instead of reusing the mounted input state", () => {
    const storage = memoryStorage();
    saveComposerDraft("thread-b", "saved B", storage);

    expect(resolveComposerDraft(
      { draftKey: "thread-a", text: "unsent A" },
      "thread-b",
      storage,
    )).toEqual({ draftKey: "thread-b", text: "saved B" });
  });

  test("clearing one submitted draft leaves other threads untouched", () => {
    const storage = memoryStorage();
    saveComposerDraft("thread-a", "message for A", storage);
    saveComposerDraft("thread-b", "message for B", storage);

    saveComposerDraft("thread-a", "", storage);

    expect(loadComposerDraft("thread-a", storage)).toBe("");
    expect(loadComposerDraft("thread-b", storage)).toBe("message for B");
  });

  test("falls back to tab memory when browser storage is unavailable", () => {
    const unavailable = {
      getItem: () => { throw new Error("blocked"); },
      setItem: () => { throw new Error("blocked"); },
      removeItem: () => { throw new Error("blocked"); },
    };

    saveComposerDraft("thread-storage-blocked", "kept in memory", unavailable);

    expect(loadComposerDraft("thread-storage-blocked", unavailable)).toBe("kept in memory");
  });
});

function clipboardSource({
  items = [],
  files = [],
}: {
  items?: Array<{ kind: string; type: string; getAsFile: () => File | null }>;
  files?: File[];
}) {
  return { items, files } as unknown as Pick<DataTransfer, "items" | "files">;
}

describe("clipboard image paste", () => {
  test("extracts image file items without including pasted text", () => {
    const image = new File(["png"], "screenshot.png", { type: "image/png" });
    const files = clipboardImageFiles(clipboardSource({
      items: [
        { kind: "string", type: "text/plain", getAsFile: () => null },
        { kind: "file", type: "image/png", getAsFile: () => image },
      ],
    }));

    expect(files).toEqual([image]);
  });

  test("falls back to clipboard files when item access is unavailable", () => {
    const image = new File(["jpeg"], "photo.jpg", { type: "image/jpeg" });
    const text = new File(["hello"], "note.txt", { type: "text/plain" });

    expect(clipboardImageFiles(clipboardSource({ files: [text, image] }))).toEqual([image]);
  });

  test("returns no images for a normal text paste", () => {
    expect(clipboardImageFiles(clipboardSource({
      items: [{ kind: "string", type: "text/plain", getAsFile: () => null }],
    }))).toEqual([]);
  });
});
