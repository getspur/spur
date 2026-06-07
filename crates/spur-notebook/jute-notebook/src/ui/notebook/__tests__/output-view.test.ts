import { describe, expect, test } from "vitest";

import {
  htmlOutputSandbox,
  isMermaidLanguageClassName,
  withVideoCapture,
} from "../rendering";

describe("OutputView helpers", () => {
  test("keeps HTML outputs sandboxed while only allowing scripts when enabled", () => {
    expect(htmlOutputSandbox(false)).toBe("");
    expect(htmlOutputSandbox(true)).toBe("allow-scripts allow-same-origin");
  });

  test("injects video capture only for opted-in canvas HTML", () => {
    const plain = "<canvas></canvas>";
    expect(withVideoCapture(plain)).toBe(plain);

    const captured = withVideoCapture(
      '<canvas data-capture="true" data-capture-duration-sec="2" data-capture-fps="24"></canvas>',
    );

    expect(captured).toContain("captureStream");
    expect(captured).toContain("MediaRecorder");
    expect(captured).toContain("FileReader");
    expect(captured).toContain("jute-video-capture");
    expect(captured).toContain("window.name");
  });

  test("detects Mermaid fenced code blocks by language class", () => {
    expect(isMermaidLanguageClassName("language-mermaid")).toBe(true);
    expect(isMermaidLanguageClassName("language-mermaid diagram")).toBe(true);
    expect(isMermaidLanguageClassName("language-typescript")).toBe(false);
    expect(isMermaidLanguageClassName(undefined)).toBe(false);
  });
});
