// cellLanguage.test.ts
import { describe, expect, test } from "vitest";
import {
  CELL_LANGUAGE_TOKENS,
  CODE_LANGUAGE_ORDER,
  cellLanguageId,
  cellLanguageToken,
} from "./cellLanguage";

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

describe("sql language token", () => {
  test("exists with DuckDB identity on the shared python3 kernel", () => {
    const token = CELL_LANGUAGE_TOKENS.sql;
    expect(token).toBeDefined();
    expect(token.label).toBe("DuckDB");
    expect(token.glyph).toBe("SQL");
    expect(token.kernelspec).toBe("python3");
    expect(token.accent.toUpperCase()).toBe("#F6BD3B");
  });

  test("ordered after go and before spur", () => {
    const index = CODE_LANGUAGE_ORDER.indexOf("sql");
    expect(index).toBeGreaterThan(CODE_LANGUAGE_ORDER.indexOf("go"));
    expect(index).toBeLessThan(CODE_LANGUAGE_ORDER.indexOf("spur"));
  });

  test("resolves from a sql codeType cell", () => {
    expect(cellLanguageId({ codeType: "sql" })).toBe("sql");
    expect(cellLanguageToken({ codeType: "sql" }).label).toBe("DuckDB");
  });
});
