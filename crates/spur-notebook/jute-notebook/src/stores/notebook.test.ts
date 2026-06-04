import { describe, expect, it } from "vitest";
import { notebookDeltaIsForPath } from "./notebook";

describe("notebookDeltaIsForPath", () => {
  it("applies a delta whose path matches the open notebook", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", "/tmp/a.ipynb")).toBe(true);
  });

  it("drops a delta whose path belongs to a different notebook", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", "/tmp/b.ipynb")).toBe(false);
  });

  it("ignores a trailing slash difference", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb/", "/tmp/a.ipynb")).toBe(true);
  });

  it("applies when the delta has no path (scratch / pre-path builds)", () => {
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", null)).toBe(true);
    expect(notebookDeltaIsForPath("/tmp/a.ipynb", undefined)).toBe(true);
  });

  it("applies when the notebook has no path yet", () => {
    expect(notebookDeltaIsForPath(undefined, "/tmp/a.ipynb")).toBe(true);
  });
});
