import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import NotebookHeader from "./NotebookHeader";
import NotebookView from "./NotebookView";

const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
        addCell: ReturnType<typeof vi.fn>;
        addCellSynced: ReturnType<typeof vi.fn>;
        insertCellAfter: ReturnType<typeof vi.fn>;
        insertCellAfterSynced: ReturnType<typeof vi.fn>;
        deleteCell: ReturnType<typeof vi.fn>;
        clearResult: ReturnType<typeof vi.fn>;
        setCellType: ReturnType<typeof vi.fn>;
        setCellCodeType: ReturnType<typeof vi.fn>;
        execute: ReturnType<typeof vi.fn>;
        refreshKernelSlotInfo: ReturnType<typeof vi.fn>;
        restartKernel: ReturnType<typeof vi.fn>;
        setSelectedCell: ReturnType<typeof vi.fn>;
      }
    | undefined,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    if (!mocks.notebook) throw new Error("Notebook mock not configured");
    return mocks.notebook;
  },
}));

vi.mock("@/ui/dag/DagView", () => ({
  default: () => <div data-testid="dag-view" />,
}));

vi.mock("./AppMode", () => ({
  default: () => <div data-testid="app-mode" />,
}));

vi.mock("./CellInput", () => ({
  default: ({ cellId }: { cellId: string }) => (
    <button
      aria-label={`Select ${cellId}`}
      onClick={() => mocks.notebook?.setSelectedCell(cellId)}
      type="button"
    >
      {cellId}
    </button>
  ),
}));

vi.mock("./NotebookLocation", () => ({
  default: () => <div data-testid="notebook-location" />,
}));

vi.mock("./sidebar/ChatPanel", () => ({
  default: () => <div data-testid="chat-panel" />,
}));

vi.mock("./sidebar/DatasourcePanel", () => ({
  default: () => <div data-testid="datasource-panel" />,
}));

describe("NotebookView", () => {
  afterEach(() => {
    cleanup();
    mocks.notebook = undefined;
  });

  function createNotebookHarness() {
    let nextCellNumber = 2;
    const store = createStore<any>()((set) => ({
      serverState: {
        lastAppliedVersion: 0,
        notebookMetadata: {},
        cellIds: ["cell-1"],
        cells: {
          "cell-1": {
            type: "code",
            initialText: "print('hello')",
            source: "print('hello')",
            version: 1,
          },
        },
      },
      viewState: {
        path: "/Users/kevintruong/.spur/scratch/Flow.ipynb",
        loadError: null,
        selectedCellId: null,
        isLoading: false,
        viewMode: "cells",
        kernelId: "kernel-1",
        kernelGeneration: 3,
      },
      editBuffer: {
        cellSources: {},
      },
      dagStatus: {},
      dagPortManifest: {},
      viewStateActions: {
        setViewMode: (viewMode: "cells" | "dag" | "app") =>
          set((state: any) => ({
            viewState: { ...state.viewState, viewMode },
          })),
      },
    }));

    const addCellSynced = vi.fn((type: "code" | "markdown", source: string) => {
      const cellId = `cell-${nextCellNumber++}`;
      store.setState((state: any) => ({
        ...state,
        serverState: {
          ...state.serverState,
          cellIds: [...state.serverState.cellIds, cellId],
          cells: {
            ...state.serverState.cells,
            [cellId]: { type, initialText: source, source, version: 1 },
          },
        },
      }));
    });
    const insertCellAfterSynced = vi.fn(
      (afterCellId: string, type: "code" | "markdown", source: string) => {
        const cellId = `cell-${nextCellNumber++}`;
        store.setState((state: any) => {
          const insertAt = state.serverState.cellIds.indexOf(afterCellId) + 1;
          const cellIds = [...state.serverState.cellIds];
          cellIds.splice(insertAt, 0, cellId);
          return {
            ...state,
            serverState: {
              ...state.serverState,
              cellIds,
              cells: {
                ...state.serverState.cells,
                [cellId]: { type, initialText: source, source, version: 1 },
              },
            },
          };
        });
      },
    );
    const deleteCell = vi.fn((cellId: string) => {
      store.setState((state: any) => {
        const { [cellId]: _deleted, ...cells } = state.serverState.cells;
        return {
          ...state,
          serverState: {
            ...state.serverState,
            cellIds: state.serverState.cellIds.filter(
              (id: string) => id !== cellId,
            ),
            cells,
          },
          viewState: {
            ...state.viewState,
            selectedCellId:
              state.viewState.selectedCellId === cellId
                ? null
                : state.viewState.selectedCellId,
          },
        };
      });
    });
    const setSelectedCell = vi.fn((cellId: string) => {
      store.setState((state: any) => ({
        ...state,
        viewState: { ...state.viewState, selectedCellId: cellId },
      }));
    });

    mocks.notebook = {
      store,
      addCell: vi.fn(),
      addCellSynced,
      insertCellAfter: vi.fn(),
      insertCellAfterSynced,
      deleteCell,
      clearResult: vi.fn(),
      setCellType: vi.fn(),
      setCellCodeType: vi.fn(),
      execute: vi.fn(),
      refreshKernelSlotInfo: vi.fn(),
      restartKernel: vi.fn(),
      setSelectedCell,
    };

    return mocks.notebook;
  }

  test("renders app mode as an app canvas without notebook document chrome", () => {
    mocks.notebook = {
      store: createStore<any>()(() => ({
        serverState: {
          cellIds: [],
          cells: {},
        },
        viewState: {
          path: "/Users/kevintruong/.spur/scratch/Untitled101.ipynb",
          loadError: null,
          viewMode: "app",
        },
      })),
      addCell: vi.fn(),
      addCellSynced: vi.fn(),
      insertCellAfter: vi.fn(),
      insertCellAfterSynced: vi.fn(),
      deleteCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
      setCellCodeType: vi.fn(),
      execute: vi.fn(),
      refreshKernelSlotInfo: vi.fn(),
      restartKernel: vi.fn(),
      setSelectedCell: vi.fn(),
    };

    const { container } = render(<NotebookView />);

    expect(screen.getByTestId("app-mode")).toBeInTheDocument();
    expect(screen.queryByTestId("notebook-location")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "AI Agent" }),
    ).toBeInTheDocument();
    expect(container.firstElementChild).toHaveClass(
      "grid-cols-[minmax(0,1fr),auto]",
    );
    expect(container.querySelector(".py-16")).not.toBeInTheDocument();
  });

  test("composes the core notebook mode journey across cells and view mode controls", async () => {
    const notebook = createNotebookHarness();

    render(
      <>
        <NotebookHeader kernelName="python3" />
        <NotebookView />
      </>,
    );

    fireEvent.click(
      await screen.findByRole("button", { name: "Select cell-1" }),
    );
    expect(notebook.setSelectedCell).toHaveBeenCalledWith("cell-1");

    fireEvent.click(screen.getByRole("button", { name: "Run selected cell" }));
    expect(notebook.execute).toHaveBeenCalledWith("cell-1");

    fireEvent.click(screen.getByRole("button", { name: "Insert cell below" }));
    expect(notebook.insertCellAfterSynced).toHaveBeenCalledWith(
      "cell-1",
      "code",
      "",
    );
    expect(
      await screen.findByRole("button", { name: "Select cell-2" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "Delete cell" })[0]);
    expect(
      screen.getByRole("dialog", { name: "Delete cell?" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(notebook.deleteCell).not.toHaveBeenCalled();
    expect(
      screen.getByRole("button", { name: "Select cell-1" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "Delete cell" })[0]);
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(notebook.deleteCell).toHaveBeenCalledTimes(1);
    expect(notebook.deleteCell).toHaveBeenCalledWith("cell-1");
    expect(
      screen.queryByRole("button", { name: "Select cell-1" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "DAG" }));
    expect(screen.getByTestId("dag-view")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Select cell-2" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Notebook" }));
    expect(screen.queryByTestId("dag-view")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Select cell-2" }),
    ).toBeInTheDocument();
  });
});
