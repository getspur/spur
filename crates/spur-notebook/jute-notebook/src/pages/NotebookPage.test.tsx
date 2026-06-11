import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import NotebookPage from "./NotebookPage";

const mocks = vi.hoisted(() => ({
  listenForNotebookEvents: vi.fn(),
  listenForRecentNotebookChanges: vi.fn(),
  loadNotebookFromPath: vi.fn(),
  setActiveAgentNotebook: vi.fn(),
  daemonControl: vi.fn(),
  store: undefined as StoreApi<any> | undefined,
  stores: [] as StoreApi<any>[],
  NotebookContext: undefined as any,
  search: "path=%2Ftmp%2Fapp-mode.ipynb",
}));

vi.mock("@/agent/bridge", () => ({
  setActiveAgentNotebook: mocks.setActiveAgentNotebook,
}));

vi.mock("@/agent/events", () => ({
  listenForNotebookEvents: mocks.listenForNotebookEvents,
  listenForRecentNotebookChanges: mocks.listenForRecentNotebookChanges,
}));

vi.mock("@/daemon/control", () => ({
  daemonControl: mocks.daemonControl,
}));

vi.mock("@/stores/notebook", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  const actual =
    await vi.importActual<typeof import("@/stores/notebook")>(
      "@/stores/notebook",
    );
  const NotebookContext = React.createContext(undefined);
  mocks.NotebookContext = NotebookContext;

  return {
    Notebook: vi.fn().mockImplementation(() => {
      const store = mocks.stores.shift() ?? mocks.store;
      if (!store) throw new Error("Notebook store mock not configured");
      return {
        loadNotebookFromPath: mocks.loadNotebookFromPath,
        state: store.getState(),
        store,
      };
    }),
    NotebookContext,
    useNotebook: () => React.useContext(NotebookContext),
    useNotebookTabsStore: actual.useNotebookTabsStore,
  };
});

vi.mock("wouter", () => ({
  useSearch: () => mocks.search,
}));

vi.mock("@/ui/notebook/AppGrantPrompt", () => ({
  AppGrantPromptContainer: () => null,
  ScriptsDisabledBanner: () => null,
}));

vi.mock("@/ui/notebook/HtmlScriptsNotice", () => ({
  default: () => <div data-testid="html-scripts-notice" />,
}));

vi.mock("@/ui/notebook/NotebookCommandMenu", () => ({
  default: () => <div data-testid="notebook-command-menu" />,
}));

vi.mock("@/ui/notebook/NotebookFooter", () => ({
  default: () => <div data-testid="notebook-footer" />,
}));

vi.mock("@/ui/notebook/NotebookHeader", () => ({
  default: () => <div data-testid="notebook-header" />,
}));

vi.mock("@/ui/notebook/NotebookView", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  return {
    default: () => {
      const notebook = React.useContext(mocks.NotebookContext) as
        | { store: StoreApi<any> }
        | undefined;
      const state = notebook?.store.getState();
      return (
        <div
          data-edit-buffer={Object.keys(
            state?.editBuffer?.cellSources ?? {},
          ).join(",")}
          data-kernel-id={state?.viewState?.kernelId ?? ""}
          data-testid="notebook-view"
        />
      );
    },
  };
});

function createNotebookStore(
  viewMode: "cells" | "dag" | "app",
  options: {
    path?: string;
    cellSources?: Record<string, unknown>;
    kernelId?: string;
  } = {},
) {
  return createStore<any>()(() => ({
    serverState: {
      cellIds: [],
      cells: {},
    },
    viewState: {
      loadError: null,
      viewMode,
      path: options.path,
      kernelId: options.kernelId,
    },
    editBuffer: {
      cellSources: options.cellSources ?? {},
    },
    dagStatus: {},
  }));
}

function tabButton(name: string) {
  return screen.getByRole("tab", { name: new RegExp(name) });
}

function closeButton(name: string) {
  return screen.getByRole("button", { name: `Close ${name}` });
}

function activeNotebookView() {
  const active = screen
    .getAllByTestId("notebook-view")
    .find((element) => element.closest("[aria-hidden='true']") === null);
  if (!active) throw new Error("active notebook view not found");
  return active;
}

describe("NotebookPage", () => {
  beforeEach(() => {
    mocks.listenForNotebookEvents.mockReset();
    mocks.listenForNotebookEvents.mockReturnValue(() => undefined);
    mocks.listenForRecentNotebookChanges.mockReset();
    mocks.listenForRecentNotebookChanges.mockReturnValue(() => undefined);
    mocks.loadNotebookFromPath.mockReset();
    mocks.setActiveAgentNotebook.mockReset();
    mocks.daemonControl.mockReset();
    mocks.daemonControl.mockResolvedValue({ ok: true });
    mocks.stores = [];
    mocks.search = "path=%2Ftmp%2Fapp-mode.ipynb";
  });

  afterEach(() => {
    cleanup();
    mocks.store = undefined;
    mocks.stores = [];
  });

  test("renders app mode with mode tabs but without footer or command chrome", () => {
    mocks.store = createNotebookStore("app");

    render(<NotebookPage />);

    expect(screen.getByTestId("notebook-view")).toBeInTheDocument();
    expect(screen.getByTestId("notebook-header")).toBeInTheDocument();
    expect(screen.queryByTestId("notebook-footer")).not.toBeInTheDocument();
    expect(
      screen.queryByTestId("notebook-command-menu"),
    ).not.toBeInTheDocument();
    expect(screen.queryByTestId("html-scripts-notice")).not.toBeInTheDocument();
  });

  test("keeps tab notebooks mounted and focuses the active notebook on switch", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/app-mode.ipynb",
      cellSources: { cellA: { source: "edited", version: 2 } },
      kernelId: "kernel-a",
    });
    const second = createNotebookStore("dag", {
      path: "/tmp/analysis.ipynb",
      kernelId: "kernel-b",
    });
    mocks.stores = [first, second];
    mocks.search = "path=%2Ftmp%2Fapp-mode.ipynb&path=%2Ftmp%2Fanalysis.ipynb";

    render(<NotebookPage />);

    await waitFor(() => {
      expect(mocks.loadNotebookFromPath).toHaveBeenCalledWith(
        "/tmp/app-mode.ipynb",
      );
      expect(mocks.loadNotebookFromPath).toHaveBeenCalledWith(
        "/tmp/analysis.ipynb",
      );
    });

    expect(activeNotebookView()).toHaveAttribute("data-edit-buffer", "cellA");
    expect(activeNotebookView()).toHaveAttribute("data-kernel-id", "kernel-a");

    fireEvent.click(tabButton("analysis.ipynb"));

    await waitFor(() => {
      expect(mocks.daemonControl).toHaveBeenCalledWith({
        command: "set_focus",
        notebook_id: "/tmp/analysis.ipynb",
      });
    });
    expect(mocks.setActiveAgentNotebook).toHaveBeenLastCalledWith(
      expect.objectContaining({ store: second }),
      "/tmp/analysis.ipynb",
    );
    expect(activeNotebookView()).toHaveAttribute("data-edit-buffer", "");
    expect(activeNotebookView()).toHaveAttribute("data-kernel-id", "kernel-b");

    fireEvent.click(tabButton("app-mode.ipynb"));

    expect(activeNotebookView()).toHaveAttribute("data-edit-buffer", "cellA");
    expect(activeNotebookView()).toHaveAttribute("data-kernel-id", "kernel-a");
  });

  test("prompts before closing a dirty tab and targets teardown to that tab", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/app-mode.ipynb",
      cellSources: { cellA: { source: "edited", version: 2 } },
      kernelId: "kernel-a",
    });
    const second = createNotebookStore("cells", {
      path: "/tmp/analysis.ipynb",
      kernelId: "kernel-b",
    });
    mocks.stores = [first, second];
    mocks.search = "path=%2Ftmp%2Fapp-mode.ipynb&path=%2Ftmp%2Fanalysis.ipynb";

    render(<NotebookPage />);

    fireEvent.click(tabButton("analysis.ipynb"));
    await waitFor(() => {
      expect(tabButton("analysis.ipynb")).toHaveAttribute(
        "aria-selected",
        "true",
      );
    });

    fireEvent.click(closeButton("app-mode.ipynb"));

    expect(
      screen.getByRole("dialog", { name: "Close app-mode.ipynb?" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/unsaved changes/i)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Close tab" }));

    await waitFor(() => {
      expect(screen.queryByRole("tab", { name: /app-mode.ipynb/ })).toBeNull();
    });
    expect(mocks.daemonControl).toHaveBeenCalledWith({
      command: "set_focus",
      notebook_id: "/tmp/app-mode.ipynb",
    });
    expect(mocks.daemonControl).toHaveBeenCalledWith({ command: "close" });
    const setFocusIndex = mocks.daemonControl.mock.calls.findIndex(
      ([cmd]) =>
        cmd.command === "set_focus" &&
        cmd.notebook_id === "/tmp/app-mode.ipynb",
    );
    const closeIndex = mocks.daemonControl.mock.calls.findIndex(
      ([cmd]) => cmd.command === "close",
    );
    expect(setFocusIndex).toBeGreaterThanOrEqual(0);
    expect(closeIndex).toBeGreaterThan(setFocusIndex);
    expect(tabButton("analysis.ipynb")).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  test("prompts before closing a running tab", () => {
    const store = createNotebookStore("cells", {
      path: "/tmp/running.ipynb",
    });
    store.setState({
      ...store.getState(),
      dagStatus: {
        cellA: { state: "running", ranPortVersions: {} },
      },
    });
    mocks.store = store;
    mocks.search = "path=%2Ftmp%2Frunning.ipynb";

    render(<NotebookPage />);

    fireEvent.click(closeButton("running.ipynb"));

    expect(
      screen.getByRole("dialog", { name: "Close running.ipynb?" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/running kernel/i)).toBeInTheDocument();
  });

  test("supports tab keyboard shortcuts", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
      kernelId: "kernel-a",
    });
    const second = createNotebookStore("cells", {
      path: "/tmp/two.ipynb",
      kernelId: "kernel-b",
    });
    const untitled = createNotebookStore("cells");
    mocks.stores = [first, second, untitled];
    mocks.search = "path=%2Ftmp%2Fone.ipynb&path=%2Ftmp%2Ftwo.ipynb";

    render(<NotebookPage />);

    fireEvent.keyDown(window, { key: "2", metaKey: true });
    await waitFor(() => {
      expect(tabButton("two.ipynb")).toHaveAttribute("aria-selected", "true");
    });

    fireEvent.keyDown(window, {
      key: "ArrowLeft",
      metaKey: true,
      altKey: true,
    });
    expect(tabButton("one.ipynb")).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(window, { key: "t", metaKey: true });
    await waitFor(() => {
      expect(screen.getAllByRole("tab")).toHaveLength(3);
    });
    expect(tabButton("Untitled")).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(window, { key: "w", metaKey: true });
    await waitFor(() => {
      expect(screen.getAllByRole("tab")).toHaveLength(2);
    });
    expect(screen.queryByRole("tab", { name: /Untitled/ })).toBeNull();
  });

  test("opens and focuses the daemon current notebook announced by recents", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
      kernelId: "kernel-a",
    });
    const scratch = createNotebookStore("cells", {
      path: "/Users/kevintruong/.spur/scratch/Untitled112.ipynb",
      kernelId: "kernel-scratch",
    });
    let applyRecents:
      | ((entries: Array<{ path: string; isCurrent: boolean }>) => void)
      | undefined;
    mocks.stores = [first, scratch];
    mocks.search = "path=%2Ftmp%2Fone.ipynb";
    mocks.listenForRecentNotebookChanges.mockImplementation((callback) => {
      applyRecents = callback;
      return () => undefined;
    });

    render(<NotebookPage />);

    await waitFor(() => {
      expect(tabButton("one.ipynb")).toHaveAttribute("aria-selected", "true");
    });

    applyRecents?.([
      {
        path: "/Users/kevintruong/.spur/scratch/Untitled112.ipynb",
        isCurrent: true,
      },
    ]);

    await waitFor(() => {
      expect(tabButton("Untitled112.ipynb")).toHaveAttribute(
        "aria-selected",
        "true",
      );
    });
    expect(mocks.loadNotebookFromPath).toHaveBeenCalledWith(
      "/Users/kevintruong/.spur/scratch/Untitled112.ipynb",
    );
    expect(activeNotebookView()).toHaveAttribute(
      "data-kernel-id",
      "kernel-scratch",
    );
  });
});
