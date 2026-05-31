import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import NotebookHeader from "./NotebookHeader";

const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
        execute: ReturnType<typeof vi.fn>;
        refreshKernelSlotInfo: ReturnType<typeof vi.fn>;
        restartKernel: ReturnType<typeof vi.fn>;
      }
    | undefined,
}));

vi.mock("@/stores/notebook", () => {
  return {
    useNotebook: () => {
      if (!mocks.notebook) throw new Error("Notebook mock not configured");
      return mocks.notebook;
    },
  };
});

describe("NotebookHeader", () => {
  beforeEach(() => {
    const store = createStore<any>()((set) => ({
      serverState: {
        lastAppliedVersion: 0,
        notebookMetadata: {},
        cellIds: [],
        cells: {},
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
      viewStateActions: {
        setViewMode: (viewMode: "cells" | "dag") =>
          set((state: any) => ({
            viewState: { ...state.viewState, viewMode },
          })),
      },
    }));
    mocks.notebook = {
      store,
      execute: vi.fn(),
      refreshKernelSlotInfo: vi.fn(),
      restartKernel: vi.fn(),
    };
  });

  test("switches notebook view mode from the segmented toggle and shortcut", () => {
    render(<NotebookHeader kernelName="python3" />);

    const notebookButton = screen.getByRole("button", { name: "Notebook" });
    const dagButton = screen.getByRole("button", { name: "DAG" });
    expect(notebookButton).toHaveAttribute("aria-pressed", "true");
    expect(dagButton).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(dagButton);
    expect(mocks.notebook?.store.getState().viewState.viewMode).toBe("dag");
    expect(dagButton).toHaveAttribute("aria-pressed", "true");

    fireEvent.keyDown(window, { key: "G", metaKey: true, shiftKey: true });
    expect(mocks.notebook?.store.getState().viewState.viewMode).toBe("cells");
    expect(notebookButton).toHaveAttribute("aria-pressed", "true");
  });
});
