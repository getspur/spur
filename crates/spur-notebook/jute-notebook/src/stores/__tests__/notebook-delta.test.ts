import { afterEach, describe, expect, test, vi } from "vitest";

import { dispatchAgentRequest } from "@/agent/handlers";
import type { DaemonCell, NotebookDelta, NotebookRoot } from "@/bindings";
import { cellToSlide } from "@/ui/deck/cellToSlide";

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

afterEach(() => {
  invokeMock.mockReset();
  vi.restoreAllMocks();
});

describe("Notebook kernel startup", () => {
  test("starts the Deno kernel from notebook kernelspec metadata", async () => {
    const cellId = "deno-cell";

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "start_kernel") {
        expect(args).toEqual({ specName: "deno" });
        return "kernel-deno";
      }
      if (command === "kernel_slot_info") {
        expect(args).toEqual({ kernelId: "kernel-deno" });
        return {
          kernel_id: "kernel-deno",
          spec_name: "deno",
          generation: 1,
          status: "idle",
          cpu_pct: 0,
          mem_mb: 0,
        };
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook({
      ...notebookRoot(cellId, "console.log('hello')"),
      metadata: {
        kernelspec: {
          name: "deno",
          display_name: "Deno",
        },
      },
    });
    await notebook.kernelStartPromise;

    expect(invokeMock).toHaveBeenCalledWith("start_kernel", {
      specName: "deno",
    });
    expect(notebook.store.getState().viewState.kernelSpecName).toBe("deno");
  });

  test("defaults to the Python kernel when notebook kernelspec metadata is absent", async () => {
    const cellId = "python-cell";

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "start_kernel") {
        expect(args).toEqual({ specName: "python3" });
        return "kernel-python";
      }
      if (command === "kernel_slot_info") {
        expect(args).toEqual({ kernelId: "kernel-python" });
        return {
          kernel_id: "kernel-python",
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
    notebook.loadNotebook(notebookRoot(cellId, "answer = 42"));
    await notebook.kernelStartPromise;

    expect(invokeMock).toHaveBeenCalledWith("start_kernel", {
      specName: "python3",
    });
    expect(notebook.store.getState().viewState.kernelSpecName).toBe("python3");
  });

  test.each([
    ["evcxr", "evcxr"],
    ["gonb", "gonb"],
    ["ruby", "python3"],
  ])(
    "normalizes kernelspec metadata name %s to %s",
    async (metadataName, expectedSpecName) => {
      const cellId = `${metadataName}-cell`;

      invokeMock.mockImplementation(async (command: string, args?: unknown) => {
        if (command === "start_kernel") {
          expect(args).toEqual({ specName: expectedSpecName });
          return `kernel-${expectedSpecName}`;
        }
        if (command === "kernel_slot_info") {
          expect(args).toEqual({ kernelId: `kernel-${expectedSpecName}` });
          return {
            kernel_id: `kernel-${expectedSpecName}`,
            spec_name: expectedSpecName,
            generation: 1,
            status: "idle",
            cpu_pct: 0,
            mem_mb: 0,
          };
        }
        throw new Error(`unexpected invoke: ${command}`);
      });

      const notebook = new Notebook();
      notebook.loadNotebook({
        ...notebookRoot(cellId, "answer = 42"),
        metadata: {
          kernelspec: {
            name: metadataName,
            display_name: metadataName,
          },
        },
      });
      await notebook.kernelStartPromise;

      expect(invokeMock).toHaveBeenCalledWith("start_kernel", {
        specName: expectedSpecName,
      });
      expect(notebook.store.getState().viewState.kernelSpecName).toBe(
        expectedSpecName,
      );
    },
  );
});

describe("reconcileNotebookDelta", () => {
  test("upserts the inline cell from a CellWritten delta without refetching", async () => {
    const cellId = "cell-1";
    const snapshotRoot = notebookRootFromCells([
      { id: cellId, source: "answer = 42", version: 2 },
    ]);
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
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

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
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
      if (command === "daemon_control") {
        expect(args).toEqual({ cmd: { command: "snapshot" } });
        return {
          ok: true,
          result: {
            type: "snapshot",
            data: { root: snapshotRoot, version: 2 },
          },
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

    expect(invokeMock).not.toHaveBeenCalledWith("daemon_control", {
      cmd: { command: "snapshot" },
    });
    expect(warnSpy).not.toHaveBeenCalled();

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

  test("requests a snapshot resync instead of applying a gapped CellInserted delta", async () => {
    const firstCellId = "cell-1";
    const missedCellId = "cell-2";
    const gappedCellId = "cell-3";
    const authoritativeRoot = notebookRootFromCells([
      { id: firstCellId, source: "answer = 1", version: 2 },
      { id: missedCellId, source: "missed = true", version: 1 },
      { id: gappedCellId, source: "authoritative = 3", version: 1 },
    ]);
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
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
      if (command === "daemon_control") {
        expect(args).toEqual({ cmd: { command: "snapshot" } });
        return {
          ok: true,
          result: {
            type: "snapshot",
            data: { root: authoritativeRoot, version: 3 },
          },
        };
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook(notebookRoot(firstCellId, "answer = 0"));
    await notebook.kernelStartPromise;

    await reconcileNotebookDelta(notebook, {
      version: 1,
      kind: {
        type: "cellWritten",
        cell: daemonCell(firstCellId, "answer = 1", 2),
      },
    });

    await reconcileNotebookDelta(notebook, {
      version: 3,
      kind: {
        type: "cellInserted",
        cell: daemonCell(gappedCellId, "gapped payload should not apply", 1),
        after_id: missedCellId,
      },
    });

    const state = notebook.store.getState();
    expect(invokeMock).toHaveBeenCalledWith("daemon_control", {
      cmd: { command: "snapshot" },
    });
    expect(warnSpy).toHaveBeenCalledWith(
      "Notebook delta version gap detected; requesting snapshot resync",
      expect.objectContaining({
        lastAppliedVersion: 1,
        receivedVersion: 3,
        kind: "cellInserted",
      }),
    );
    expect(state.serverState.cellIds).toEqual([
      firstCellId,
      missedCellId,
      gappedCellId,
    ]);
    expect(state.serverState.cells[missedCellId]?.source).toBe("missed = true");
    expect(state.serverState.cells[gappedCellId]?.source).toBe(
      "authoritative = 3",
    );
    expect(state.serverState.lastAppliedVersion).toBe(3);
  });

  test("loads notebook jute_deck theme for slide theme resolution", async () => {
    const cellId = "slide-1";

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
    notebook.loadNotebook({
      metadata: { jute_deck: { theme: "spur-brand" } },
      nbformat_minor: 5,
      nbformat: 4,
      cells: [
        {
          cell_type: "markdown",
          id: cellId,
          metadata: { spur: { version: 1 } },
          source: "# Deck title",
        },
      ],
    });
    await notebook.kernelStartPromise;

    const state = notebook.store.getState();
    const cell = state.serverState.cells[cellId];
    const deck = state.serverState.notebookMetadata.jute_deck;
    expect(cell?.juteDeckMetadata).toBeUndefined();
    expect(
      cellToSlide(
        {
          id: cellId,
          type: cell?.type,
          source: cell?.source,
          metadata: { jute_deck: cell?.juteDeckMetadata },
          outputs: [],
        },
        deck,
      )?.theme,
    ).toBe("spur-brand");
  });

  test("loads DAG metadata and switches notebook view mode", async () => {
    const cellId = "dag-cell-1";

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
    notebook.loadNotebook({
      metadata: {},
      nbformat_minor: 5,
      nbformat: 4,
      cells: [
        {
          cell_type: "code",
          id: cellId,
          metadata: {
            spur: {
              version: 1,
              dag: {
                produces: [{ port: "customers", repr: "dataframe" }],
                consumes: ["raw_customers"],
                source: { kind: "cell", port: "raw_customers" },
              },
            },
          },
          source: "customers = raw_customers.copy()",
          execution_count: null,
          outputs: [],
        },
      ],
    });
    await notebook.kernelStartPromise;

    const initialState = notebook.store.getState();
    expect(initialState.serverState.cells[cellId]?.dagMetadata).toEqual({
      produces: [{ port: "customers", repr: "dataframe" }],
      consumes: ["raw_customers"],
      source: { kind: "cell", port: "raw_customers" },
    });
    expect(initialState.viewState.viewMode).toBe("cells");
    expect(initialState.dagStatus).toEqual({});

    initialState.viewStateActions.setViewMode("dag");
    expect(notebook.store.getState().viewState.viewMode).toBe("dag");

    notebook.store.getState().viewStateActions.setViewMode("cells");
    expect(notebook.store.getState().viewState.viewMode).toBe("cells");
  });

  test("agent set_cell_metadata with spur dag updates frontend export", async () => {
    const cellId = "dag-cell-1";
    const dag = {
      produces: [{ port: "customers", repr: "dataframe" }],
      consumes: ["raw_customers"],
      source: { kind: "cell", port: "raw_customers" },
    };

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
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
      if (command === "daemon_control") {
        expect(args).toEqual({
          cmd: {
            command: "set_cell_metadata",
            id: cellId,
            patch: { spur: { dag } },
            expected_version: 1,
          },
        });
        return {
          ok: true,
          result: {
            type: "delta",
            data: {
              version: 2,
              kind: {
                type: "cellWritten",
                cell: {
                  id: cellId,
                  kind: "code",
                  version: 2,
                  lastEditedBy: "brain",
                  source: "customers = raw_customers.copy()",
                  execCount: null,
                  status: "idle",
                  outputs: [],
                  dagMetadata: dag,
                },
              },
            },
          },
        };
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook(
      notebookRoot(cellId, "customers = raw_customers.copy()"),
    );
    await notebook.kernelStartPromise;

    const result = await dispatchAgentRequest(notebook, {
      requestId: "request-1",
      method: "notebook.set_cell_metadata",
      params: {
        id: cellId,
        patch: { spur: { dag } },
        expected_version: 1,
      },
    });

    expect(result).toEqual({ ok: true, version: 2 });
    expect(
      notebook.store.getState().serverState.cells[cellId]?.dagMetadata,
    ).toEqual(dag);
    expect(notebook.export().cells[0]?.metadata.spur?.dag).toEqual(dag);
  });

  test("setCellCodeType converts to code and updates code_type locally", async () => {
    const cellId = "markdown-cell";

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
    notebook.loadNotebook({
      metadata: {},
      nbformat_minor: 5,
      nbformat: 4,
      cells: [
        {
          cell_type: "markdown",
          id: cellId,
          metadata: {
            spur: {
              version: 2,
              last_edited_by: "brain",
              code_type: "rust",
            },
          },
          source: "print('hello')",
        },
      ],
    });
    await notebook.kernelStartPromise;

    notebook.setCellCodeType(cellId, "rust");

    const cell = selectCell(notebook.store.getState(), cellId);
    expect(cell).toMatchObject({
      type: "code",
      codeType: "rust",
      version: 3,
      lastEditedBy: "brain",
    });
    expect(notebook.export().cells[0]).toMatchObject({
      cell_type: "code",
      metadata: { spur: { code_type: "rust", version: 3 } },
    });
  });
});

function notebookRoot(cellId: string, source: string): NotebookRoot {
  return notebookRootFromCells([{ id: cellId, source, version: 1 }]);
}

function notebookRootFromCells(
  cells: Array<{ id: string; source: string; version: number }>,
): NotebookRoot {
  return {
    metadata: {},
    nbformat_minor: 5,
    nbformat: 4,
    cells: cells.map((cell) => ({
      cell_type: "code",
      id: cell.id,
      metadata: { spur: { version: cell.version } },
      source: cell.source,
      execution_count: null,
      outputs: [],
    })),
  };
}

function daemonCell(id: string, source: string, version: number): DaemonCell {
  return {
    id,
    kind: "code",
    version,
    source,
    lastEditedBy: "brain",
    execCount: null,
    status: "idle",
    outputs: [],
  };
}
