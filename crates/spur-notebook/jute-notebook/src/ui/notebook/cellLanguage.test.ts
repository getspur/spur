// cellLanguage.test.ts
import { describe, expect, test } from "vitest";
import { cellLanguageId, cellLanguageToken } from "./cellLanguage";

describe("cellLanguage", () => {
  test("spur kernelspec wins regardless of codeType", () => {
    const cell = { cellMetadataOther: { kernelspec: { name: "spur" } }, codeType: "python" };
    expect(cellLanguageId(cell)).toBe("spur");
    expect(cellLanguageToken(cell).label).toBe("AI Agent");
  });
  test("falls back to codeType, then python", () => {
    expect(cellLanguageId({ codeType: "rust" })).toBe("rust");
    expect(cellLanguageId({})).toBe("python");
    expect(cellLanguageToken({ codeType: "go" }).glyph).toBe("Go");
  });
  test("falls back to python for unknown codeType", () => {
    expect(cellLanguageId({ codeType: "ruby" as any })).toBe("python");
    expect(cellLanguageToken({ codeType: "ruby" as any }).label).toBe("Python");
  });
});
