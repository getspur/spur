import { describe, expect, test } from "vitest";

import { htmlOutputSandbox, isMermaidLanguageClassName } from "../rendering";

describe("OutputView helpers", () => {
  test("keeps HTML outputs sandboxed while only allowing scripts when enabled", () => {
    expect(htmlOutputSandbox(false)).toBe("");
    expect(htmlOutputSandbox(true)).toBe("allow-scripts");
  });

  test("detects Mermaid fenced code blocks by language class", () => {
    expect(isMermaidLanguageClassName("language-mermaid")).toBe(true);
    expect(isMermaidLanguageClassName("language-mermaid diagram")).toBe(true);
    expect(isMermaidLanguageClassName("language-typescript")).toBe(false);
    expect(isMermaidLanguageClassName(undefined)).toBe(false);
  });
});
