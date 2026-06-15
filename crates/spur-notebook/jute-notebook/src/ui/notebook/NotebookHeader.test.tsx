import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import { NOTEBOOK_COMMAND_MENU_OPEN_EVENT } from "./NotebookCommandMenuEvents";
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
  function configureNotebook(options: { appOpenInfo?: unknown } = {}) {
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
        appOpenInfo: options.appOpenInfo,
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
    mocks.notebook = {
      store,
      execute: vi.fn(),
      refreshKernelSlotInfo: vi.fn(),
      restartKernel: vi.fn(),
    };
  }

  beforeEach(() => {
    configureNotebook();
  });

  afterEach(() => {
    cleanup();
  });

  test("labels global notebook controls", () => {
    render(<NotebookHeader kernelName="python3" />);

    for (const name of [
      "Run selected cell",
      "Restart kernel",
      "Kernel stats",
      "Command palette",
      "Scheduled cells",
      "Settings",
    ]) {
      const button = screen.getByRole("button", { name });
      expect(button).toHaveAttribute("aria-label", name);
      expect(button).toHaveAttribute("title", expect.stringContaining(name));
    }
  });

  test("opens the command palette from the visible shortcut trigger", () => {
    const openListener = vi.fn();
    window.addEventListener(NOTEBOOK_COMMAND_MENU_OPEN_EVENT, openListener);
    render(<NotebookHeader kernelName="python3" />);

    fireEvent.click(screen.getByRole("button", { name: "Command palette" }));

    expect(openListener).toHaveBeenCalledTimes(1);
    window.removeEventListener(NOTEBOOK_COMMAND_MENU_OPEN_EVENT, openListener);
  });

  test("switches notebook view mode from the segmented toggle and shortcut", () => {
    configureNotebook({
      appOpenInfo: {
        open_mode: "app",
        app_name: "Demo app",
        app_root: "/tmp/demo-app",
        capabilities: {
          active_output_scripts: true,
          canvas_capture: false,
          artifacts_dir: true,
        },
        skill: "demo",
      },
    });
    render(<NotebookHeader kernelName="python3" />);

    const notebookButton = screen.getByRole("button", { name: "Notebook" });
    const dagButton = screen.getByRole("button", { name: "DAG" });
    const appButton = screen.getByRole("button", { name: "App" });
    expect(notebookButton).toHaveAttribute("aria-pressed", "true");
    expect(dagButton).toHaveAttribute("aria-pressed", "false");
    expect(appButton).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(appButton);
    expect(mocks.notebook?.store.getState().viewState.viewMode).toBe("app");
    expect(appButton).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(dagButton);
    expect(mocks.notebook?.store.getState().viewState.viewMode).toBe("dag");
    expect(dagButton).toHaveAttribute("aria-pressed", "true");

    fireEvent.keyDown(window, { key: "G", metaKey: true, shiftKey: true });
    expect(mocks.notebook?.store.getState().viewState.viewMode).toBe("cells");
    expect(notebookButton).toHaveAttribute("aria-pressed", "true");
  });

  test("does not switch regular notebooks into app mode", () => {
    render(<NotebookHeader kernelName="python3" />);

    const appButton = screen.getByRole("button", { name: "App" });
    expect(appButton).toBeDisabled();
    expect(appButton).toHaveAttribute("aria-pressed", "false");
    expect(appButton).toHaveAttribute("title", "App mode unavailable");

    fireEvent.click(appButton);

    expect(mocks.notebook?.store.getState().viewState.viewMode).toBe("cells");
  });

  test("does not render competing notebook management links", () => {
    const { container } = render(<NotebookHeader kernelName="python3" />);

    expect(container.querySelectorAll('a[href="/"]')).toHaveLength(0);
    expect(container.querySelectorAll('button[title="Settings"]')).toHaveLength(
      1,
    );
  });
});
