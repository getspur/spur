import { afterEach, describe, expect, test, vi } from "vitest";

import type { DaemonCell, NotebookDelta, NotebookRoot } from "@/bindings";

import {
  Notebook,
  type NotebookStoreState,
  __resetNotebookRuntimeConfigCacheForTesting,
  reconcileNotebookDelta,
} from "../notebook";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class<T> {
    onmessage?: (message: T) => void;
  },
  invoke: invokeMock,
}));

describe("reconcileNotebookDelta", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    invokeMock.mockReset();
    __resetNotebookRuntimeConfigCacheForTesting();
  });

  test("updates a cell source after a CellWritten delta", async () => {
    const cellId = "cell-1";
    const updatedCell: DaemonCell = {
      id: cellId,
      kind: "code",
      version: 2,
      source: "answer = 42",
      execCount: null,
      status: "idle",
      outputs: [],
    };

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "notebook_runtime_config") return { inProcStore: true };
      if (command === "start_kernel") return "kernel-1";
      if (command === "kernel_slot_info") {
        return {
          kernel_id: "kernel-1",
          spec_name: "python3",
          generation: 1,
          status: "idle",
          cpu_pct: 0,
          mem_mb: 0,
        };
      }
      if (command === "read_notebook_store_cell") {
        expect(args).toEqual({ id: cellId });
        return updatedCell;
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook(notebookRoot(cellId, "answer = 0"));
    await notebook.kernelStartPromise;

    const delta: NotebookDelta = {
      version: 2,
      kind: { type: "cellWritten", id: cellId },
    };
    await reconcileNotebookDelta(notebook, delta);

    const selectSource = (state: NotebookStoreState) =>
      state.cells[cellId]?.source;
    expect(selectSource(notebook.store.getState())).toBe("answer = 42");
  });
});

function notebookRoot(cellId: string, source: string): NotebookRoot {
  return {
    metadata: {},
    nbformat_minor: 5,
    nbformat: 4,
    cells: [
      {
        cell_type: "code",
        id: cellId,
        metadata: { spur: { version: 1 } },
        source,
        execution_count: null,
        outputs: [],
      },
    ],
  };
}
