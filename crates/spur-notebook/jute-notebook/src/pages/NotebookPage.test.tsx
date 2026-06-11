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
  openDialog: vi.fn(),
  setLocation: vi.fn(),
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

vi.mock("@/daemon/control", async () => {
  const actual =
    await vi.importActual<typeof import("@/daemon/control")>(
      "@/daemon/control",
    );
  return {
    ...actual,
    daemonControl: mocks.daemonControl,
  };
});

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mocks.openDialog,
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
  useLocation: () => ["/notebook", mocks.setLocation],
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
    mocks.openDialog.mockReset();
    mocks.openDialog.mockResolvedValue(null);
    mocks.setLocation.mockReset();
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

  test("preserves an existing tab notebook store when route search adds another tab", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
      cellSources: { cellA: { source: "edited", version: 2 } },
      kernelId: "kernel-a",
    });
    const replacementFirst = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
    });
    const second = createNotebookStore("cells", {
      path: "/tmp/two.ipynb",
      kernelId: "kernel-b",
    });
    mocks.stores = [first, replacementFirst, second];
    mocks.search = "path=%2Ftmp%2Fone.ipynb";

    const { rerender } = render(<NotebookPage />);

    await waitFor(() => {
      expect(activeNotebookView()).toHaveAttribute("data-edit-buffer", "cellA");
    });

    mocks.search = "path=%2Ftmp%2Fone.ipynb&path=%2Ftmp%2Ftwo.ipynb";
    rerender(<NotebookPage />);

    await waitFor(() => {
      expect(tabButton("two.ipynb")).toBeInTheDocument();
    });
    expect(tabButton("one.ipynb")).toHaveAttribute("aria-selected", "true");
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
    mocks.daemonControl.mockClear();

    fireEvent.click(closeButton("app-mode.ipynb"));

    expect(
      screen.getByRole("dialog", { name: "Close app-mode.ipynb?" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/unsaved changes/i)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Close tab" }));

    await waitFor(() => {
      expect(screen.queryByRole("tab", { name: /app-mode.ipynb/ })).toBeNull();
    });
    expect(mocks.daemonControl).toHaveBeenCalledTimes(1);
    expect(mocks.daemonControl).toHaveBeenCalledWith({
      command: "close_notebook",
      notebook_id: "/tmp/app-mode.ipynb",
    });
    expect(mocks.daemonControl).not.toHaveBeenCalledWith({ command: "close" });
    expect(tabButton("analysis.ipynb")).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  test("prompts before closing a running tab", async () => {
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

    fireEvent.click(screen.getByRole("button", { name: "Close tab" }));

    await waitFor(() => {
      expect(screen.queryByRole("tab", { name: /running.ipynb/ })).toBeNull();
    });
    expect(mocks.daemonControl).toHaveBeenCalledWith({
      command: "close_notebook",
      notebook_id: "/tmp/running.ipynb",
    });
    expect(mocks.daemonControl).not.toHaveBeenCalledWith({ command: "close" });
  });

  test("closing the active tab tears down that tab and selects a neighbor", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
      kernelId: "kernel-a",
    });
    const second = createNotebookStore("cells", {
      path: "/tmp/two.ipynb",
      kernelId: "kernel-b",
    });
    mocks.stores = [first, second];
    mocks.search = "path=%2Ftmp%2Fone.ipynb&path=%2Ftmp%2Ftwo.ipynb";

    render(<NotebookPage />);

    await waitFor(() => {
      expect(tabButton("one.ipynb")).toHaveAttribute("aria-selected", "true");
      expect(mocks.daemonControl).toHaveBeenCalledWith({
        command: "set_focus",
        notebook_id: "/tmp/one.ipynb",
      });
    });
    mocks.daemonControl.mockClear();

    fireEvent.click(closeButton("one.ipynb"));

    await waitFor(() => {
      expect(screen.queryByRole("tab", { name: /one.ipynb/ })).toBeNull();
    });
    expect(tabButton("two.ipynb")).toHaveAttribute("aria-selected", "true");
    expect(activeNotebookView()).toHaveAttribute("data-kernel-id", "kernel-b");
    expect(mocks.daemonControl.mock.calls[0]?.[0]).toEqual({
      command: "close_notebook",
      notebook_id: "/tmp/one.ipynb",
    });
    expect(mocks.daemonControl).toHaveBeenCalledWith({
      command: "set_focus",
      notebook_id: "/tmp/two.ipynb",
    });
    expect(mocks.daemonControl).not.toHaveBeenCalledWith({ command: "close" });
  });

  test("materializes an empty notebook route as daemon-backed scratch", async () => {
    const placeholder = createNotebookStore("cells");
    const scratch = createNotebookStore("cells", {
      path: "/tmp/scratch.ipynb",
    });
    mocks.stores = [placeholder, scratch];
    mocks.search = "";
    mocks.daemonControl.mockImplementation(async (cmd) =>
      cmd.command === "new"
        ? { ok: true, path: "/tmp/scratch.ipynb" }
        : { ok: true },
    );

    render(<NotebookPage />);

    await waitFor(() => {
      expect(mocks.daemonControl).toHaveBeenCalledWith({
        command: "new",
        activate: false,
      });
    });
    await waitFor(() => {
      expect(tabButton("scratch.ipynb")).toHaveAttribute(
        "aria-selected",
        "true",
      );
    });
    expect(screen.queryByRole("tab", { name: /Untitled/ })).toBeNull();
    expect(mocks.setLocation).toHaveBeenCalledWith(
      "/notebook?path=%2Ftmp%2Fscratch.ipynb&active=%2Ftmp%2Fscratch.ipynb",
    );
  });

  test("creates a daemon-backed notebook tab from the tab strip", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
    });
    const scratch = createNotebookStore("cells", {
      path: "/tmp/scratch-1.ipynb",
    });
    mocks.stores = [first, scratch];
    mocks.search = "path=%2Ftmp%2Fone.ipynb";
    mocks.daemonControl.mockImplementation(async (cmd) =>
      cmd.command === "new"
        ? { ok: true, path: "/tmp/scratch-1.ipynb" }
        : { ok: true },
    );

    render(<NotebookPage />);

    fireEvent.click(screen.getByRole("button", { name: "New tab" }));

    await waitFor(() => {
      expect(mocks.daemonControl).toHaveBeenCalledWith({
        command: "new",
        activate: false,
      });
    });
    await waitFor(() => {
      expect(tabButton("scratch-1.ipynb")).toHaveAttribute(
        "aria-selected",
        "true",
      );
    });
    expect(mocks.loadNotebookFromPath).toHaveBeenCalledWith(
      "/tmp/scratch-1.ipynb",
    );
    expect(mocks.setLocation).toHaveBeenCalledWith(
      "/notebook?path=%2Ftmp%2Fone.ipynb&path=%2Ftmp%2Fscratch-1.ipynb&active=%2Ftmp%2Fscratch-1.ipynb",
    );
  });

  test("opens a picked notebook into the tab set", async () => {
    const first = createNotebookStore("cells", {
      path: "/tmp/one.ipynb",
    });
    const analysis = createNotebookStore("cells", {
      path: "/tmp/analysis.ipynb",
    });
    mocks.stores = [first, analysis];
    mocks.search = "path=%2Ftmp%2Fone.ipynb";
    mocks.openDialog.mockResolvedValue("/tmp/analysis.ipynb");
    mocks.daemonControl.mockImplementation(async (cmd) =>
      cmd.command === "open"
        ? { ok: true, path: "/tmp/analysis.ipynb" }
        : { ok: true },
    );

    render(<NotebookPage />);

    fireEvent.click(screen.getByRole("button", { name: "Tab overflow" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Open notebook..." }));

    await waitFor(() => {
      expect(mocks.openDialog).toHaveBeenCalledWith({
        multiple: false,
        directory: false,
        filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
      });
    });
    expect(mocks.daemonControl).toHaveBeenCalledWith({
      command: "open",
      path: "/tmp/analysis.ipynb",
      activate: false,
    });
    await waitFor(() => {
      expect(tabButton("analysis.ipynb")).toHaveAttribute(
        "aria-selected",
        "true",
      );
    });
    expect(mocks.setLocation).toHaveBeenCalledWith(
      "/notebook?path=%2Ftmp%2Fone.ipynb&path=%2Ftmp%2Fanalysis.ipynb&active=%2Ftmp%2Fanalysis.ipynb",
    );
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
    const untitled = createNotebookStore("cells", {
      path: "/tmp/untitled.ipynb",
    });
    mocks.stores = [first, second, untitled];
    mocks.search = "path=%2Ftmp%2Fone.ipynb&path=%2Ftmp%2Ftwo.ipynb";
    mocks.daemonControl.mockImplementation(async (cmd) =>
      cmd.command === "new"
        ? { ok: true, path: "/tmp/untitled.ipynb" }
        : { ok: true },
    );

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
    expect(tabButton("untitled.ipynb")).toHaveAttribute(
      "aria-selected",
      "true",
    );

    fireEvent.keyDown(window, { key: "w", metaKey: true });
    await waitFor(() => {
      expect(screen.getAllByRole("tab")).toHaveLength(2);
    });
    expect(screen.queryByRole("tab", { name: /untitled.ipynb/ })).toBeNull();
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
