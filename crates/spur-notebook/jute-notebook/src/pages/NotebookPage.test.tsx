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

vi.mock("@/ui/notebook/NotebookTabsBasicControls", () => ({
  default: ({ activeTabId, tabs, onSwitchTab }: any) => (
    <div data-testid="notebook-tabs-controls">
      {tabs.map((tab: any) => (
        <button
          aria-pressed={tab.id === activeTabId}
          key={tab.id}
          onClick={() => onSwitchTab(tab.id)}
          type="button"
        >
          {tab.title}
        </button>
      ))}
    </div>
  ),
}));

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

    fireEvent.click(screen.getByRole("button", { name: "analysis.ipynb" }));

    await waitFor(() => {
      expect(mocks.daemonControl).toHaveBeenCalledWith({
        command: "set_focus",
        notebook_id: "/tmp/analysis.ipynb",
      });
    });
    expect(mocks.setActiveAgentNotebook).toHaveBeenLastCalledWith(
      expect.objectContaining({ store: second }),
    );
    expect(activeNotebookView()).toHaveAttribute("data-edit-buffer", "");
    expect(activeNotebookView()).toHaveAttribute("data-kernel-id", "kernel-b");

    fireEvent.click(screen.getByRole("button", { name: "app-mode.ipynb" }));

    expect(activeNotebookView()).toHaveAttribute("data-edit-buffer", "cellA");
    expect(activeNotebookView()).toHaveAttribute("data-kernel-id", "kernel-a");
  });
});
