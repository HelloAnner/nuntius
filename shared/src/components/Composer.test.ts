import { describe, expect, test } from "bun:test";
import { clipboardImageFiles } from "./Composer";

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
