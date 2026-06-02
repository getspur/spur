import "@testing-library/jest-dom/vitest";
import { act, render, screen } from "@testing-library/react";
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

function createNotebookStore(startedAt: number) {
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
              phase: "compiling",
              current: null,
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
    vi.useRealTimers();
    mocks.notebook = undefined;
  });

  test("renders compiling chip with elapsed seconds from compile start", () => {
    render(<NotebookCells />);

    expect(screen.getByText("Compiling… ⏱ 5s")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1000);
    });

    expect(screen.getByText("Compiling… ⏱ 6s")).toBeInTheDocument();
  });
});
