import { describe, expect, test, vi } from "vitest";

import type { DaemonCell, NotebookDelta, NotebookRoot } from "@/bindings";

import {
  Notebook,
  type NotebookStoreState,
  reconcileNotebookDelta,
  selectCell,
} from "../notebook";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class<T> {
    onmessage?: (message: T) => void;
  },
  invoke: invokeMock,
}));

describe("reconcileNotebookDelta", () => {
  test("upserts the inline cell from a CellWritten delta without refetching", async () => {
    const cellId = "cell-1";
    const updatedCell: DaemonCell = {
      id: cellId,
      kind: "code",
      version: 2,
      source: "answer = 42",
      lastEditedBy: "brain",
      execCount: null,
      status: "idle",
      outputs: [],
    };

    invokeMock.mockImplementation(async (command: string) => {
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
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook(notebookRoot(cellId, "answer = 0"));
    await notebook.kernelStartPromise;

    const delta: NotebookDelta = {
      version: 2,
      kind: { type: "cellWritten", cell: updatedCell },
    };
    await reconcileNotebookDelta(notebook, delta);

    const selectSource = (state: NotebookStoreState) =>
      selectCell(state, cellId)?.source;
    expect(selectSource(notebook.store.getState())).toBe("answer = 42");
    expect(
      notebook.store.getState().serverState.cells[cellId]?.lastEditedBy,
    ).toBe("brain");
    // No cell-refetch invoke: the mock throws on any unexpected command, so the
    // reducer applying the inline cell is what keeps this test green.
    expect(notebook.store.getState().serverState.cells[cellId]?.version).toBe(
      2,
    );
    expect(notebook.store.getState().editBuffer.cellSources[cellId]).toBe(
      undefined,
    );
  });

  test("keeps local source edits in the edit buffer overlay", async () => {
    const cellId = "cell-1";

    invokeMock.mockImplementation(async (command: string) => {
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
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook(notebookRoot(cellId, "answer = 0"));
    await notebook.kernelStartPromise;

    notebook.updateCellSource(cellId, "answer = 42");

    const state = notebook.store.getState();
    expect(state.serverState.cells[cellId]?.source).toBe("answer = 0");
    expect(state.editBuffer.cellSources[cellId]).toEqual({
      source: "answer = 42",
      version: 2,
      lastEditedBy: undefined,
    });
    expect(selectCell(state, cellId)?.source).toBe("answer = 42");
    expect(notebook.export().cells[0]?.source).toBe("answer = 42");
    expect(notebook.getCellSnapshotById(cellId)?.metadata.spur?.version).toBe(
      2,
    );
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
