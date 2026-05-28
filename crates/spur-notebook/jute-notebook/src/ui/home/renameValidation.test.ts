import { describe, expect, test } from "vitest";

import { validateRename } from "./renameValidation";

describe("validateRename", () => {
  test("trims whitespace", () => {
    expect(validateRename("  Analysis.ipynb  ")).toEqual({
      ok: true,
      fileName: "Analysis.ipynb",
    });
  });

  test("rejects empty names", () => {
    expect(validateRename("   ")).toEqual({
      ok: false,
      error: "Notebook name must not be empty.",
    });
  });

  test("rejects forward slash path separators", () => {
    expect(validateRename("reports/Analysis.ipynb")).toEqual({
      ok: false,
      error: "Notebook name must not contain path separators.",
    });
  });

  test("rejects backslash path separators", () => {
    expect(validateRename("reports\\Analysis.ipynb")).toEqual({
      ok: false,
      error: "Notebook name must not contain path separators.",
    });
  });

  test("appends .ipynb when missing and preserves existing extension case-insensitively", () => {
    expect(validateRename("Analysis")).toEqual({
      ok: true,
      fileName: "Analysis.ipynb",
    });
    expect(validateRename("Analysis.IPYNB")).toEqual({
      ok: true,
      fileName: "Analysis.IPYNB",
    });
  });
});
