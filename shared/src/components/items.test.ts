import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { AgentMessage } from "./items";

describe("AgentMessage streaming state", () => {
  test("signals activity without adding a layout cursor", () => {
    const html = renderToStaticMarkup(
      createElement(AgentMessage, { text: "正在分析", streaming: true }),
    );

    expect(html).toContain("msg-agent streaming");
    expect(html).toContain('aria-busy="true"');
    expect(html).not.toContain("caret");
  });
});
