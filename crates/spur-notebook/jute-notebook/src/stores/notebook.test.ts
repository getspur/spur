import { describe, expect, it } from "vitest";
import { notebookDeltaIsForPath, reconcileNotebookDelta } from "./notebook";

function stubNotebook(path: string | undefined) {
  const calls = { applied: 0, resynced: 0 };
  const notebook = {
    state: {
      viewState: { path },
      serverState: { lastAppliedVersion: 1 },
    },
    applyNotebookDelta: () => {
      calls.applied += 1;
    },
    resyncFromSnapshot: async () => {
      calls.resynced += 1;
    },
  };
  // reconcileNotebookDelta only touches the members above.
  return { notebook: notebook as unknown as import("./notebook").Notebook, calls };
}

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

describe("reconcileNotebookDelta path guard", () => {
  it("drops a foreign-path delta without applying or resyncing", async () => {
    const { notebook, calls } = stubNotebook("/tmp/a.ipynb");
    await reconcileNotebookDelta(notebook, {
      version: 2,
      path: "/tmp/b.ipynb",
      kind: { type: "cellDeleted", id: "c1" },
    } as never);
    expect(calls.applied).toBe(0);
    expect(calls.resynced).toBe(0);
  });

  it("applies a matching-path delta", async () => {
    const { notebook, calls } = stubNotebook("/tmp/a.ipynb");
    await reconcileNotebookDelta(notebook, {
      version: 2,
      path: "/tmp/a.ipynb",
      kind: { type: "cellDeleted", id: "c1" },
    } as never);
    expect(calls.applied).toBe(1);
  });
});
