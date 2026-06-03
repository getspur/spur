import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import NotebookCells from "./NotebookCells";

const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
        addCell: ReturnType<typeof vi.fn>;
        clearResult: ReturnType<typeof vi.fn>;
        setCellType: ReturnType<typeof vi.fn>;
      }
    | undefined,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    if (!mocks.notebook) throw new Error("Notebook mock not configured");
    return mocks.notebook;
  },
}));

vi.mock("./CellInput", () => ({
  default: ({ cellId }: { cellId: string }) => (
    <div data-testid={`cell-input-${cellId}`} />
  ),
}));

function createNotebookStore(
  startedAt: number,
  compileCurrent: string | null = null,
  compilePhase: "compiling" | "running" = "compiling",
) {
  return createStore<any>()(() => ({
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
          result: {
            status: "running",
            timings: { startedAt },
            executionCount: undefined,
            compile: {
              phase: compilePhase,
              current: compileCurrent,
              startedAt,
            },
            outputs: [],
          },
        },
      },
    },
    viewState: {
      selectedCellId: null,
      isLoading: false,
      viewMode: "cells",
    },
    editBuffer: {
      cellSources: {},
    },
    dagStatus: {},
    dagPortManifest: {},
  }));
}

function createNotebookStoreForCell(cell: Record<string, unknown>) {
  return createStore<any>()(() => ({
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
          result: {
            status: "running",
            timings: { startedAt: Date.now() },
            executionCount: undefined,
            outputs: [],
          },
          ...cell,
        },
      },
    },
    viewState: {
      selectedCellId: null,
      isLoading: false,
      viewMode: "cells",
    },
    editBuffer: {
      cellSources: {},
    },
    dagStatus: {},
    dagPortManifest: {},
  }));
}

describe("NotebookCells", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-02T00:00:05.400Z"));
    const startedAt = new Date("2026-06-02T00:00:00.000Z").getTime();
    mocks.notebook = {
      store: createNotebookStore(startedAt),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
    };
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    mocks.notebook = undefined;
  });

  test("renders compile progress in the execution gutter and output rail", () => {
    const { unmount } = render(<NotebookCells />);

    expect(
      screen.getByRole("status", { name: "Cell execution Compiling 5s" }),
    ).toHaveTextContent("5s");
    expect(
      screen.getByRole("status", { name: "Compiling 5s" }),
    ).toHaveTextContent("Compiling");
    expect(screen.queryByText(/Compiling.*⏱/)).not.toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1000);
    });

    expect(
      screen.getByRole("status", { name: "Cell execution Compiling 6s" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("status", { name: "Compiling 6s" }),
    ).toBeInTheDocument();

    unmount();

    vi.setSystemTime(new Date("2026-06-02T00:00:05.400Z"));
    const startedAt = new Date("2026-06-02T00:00:00.000Z").getTime();
    mocks.notebook = {
      store: createNotebookStore(startedAt, "smawk"),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
    };

    render(<NotebookCells />);

    expect(
      screen.getByRole("status", { name: "Cell execution Compiling 5s" }),
    ).toHaveTextContent("5s");
    expect(
      screen.getByRole("status", { name: "Compiling smawk 5s" }),
    ).toHaveTextContent("Compiling smawk");
  });

  test("renders live AI cell header and execution marker accent", () => {
    mocks.notebook = {
      store: createNotebookStoreForCell({
        cellMetadataOther: { kernelspec: { name: "spur" } },
        dagMetadata: { ai_live: true },
      }),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
    };

    render(<NotebookCells />);

    expect(screen.getByText("✦ AI")).toBeInTheDocument();
    expect(screen.getByText("● LIVE")).toBeInTheDocument();
    expect(screen.getByText(/✦\[\*\]/)).toBeInTheDocument();
  });

  test("renders manual AI cell header when AI live metadata is false", () => {
    mocks.notebook = {
      store: createNotebookStoreForCell({
        cellMetadataOther: { kernelspec: { name: "spur" } },
        dagMetadata: { ai_live: false },
      }),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
    };

    render(<NotebookCells />);

    expect(screen.getByText("✦ AI")).toBeInTheDocument();
    expect(screen.getByText("manual")).toBeInTheDocument();
  });

  test("does not render AI header for plain code cells", () => {
    mocks.notebook = {
      store: createNotebookStoreForCell({}),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
    };

    render(<NotebookCells />);

    expect(screen.queryByText("✦ AI")).not.toBeInTheDocument();
    expect(screen.queryByText(/✦\[/)).not.toBeInTheDocument();
  });
});
