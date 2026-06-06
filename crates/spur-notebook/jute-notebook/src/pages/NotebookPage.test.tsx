import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import NotebookPage from "./NotebookPage";

const mocks = vi.hoisted(() => ({
  listenForNotebookEvents: vi.fn(),
  loadNotebookFromPath: vi.fn(),
  setActiveAgentNotebook: vi.fn(),
  store: undefined as StoreApi<any> | undefined,
}));

vi.mock("@/agent/bridge", () => ({
  setActiveAgentNotebook: mocks.setActiveAgentNotebook,
}));

vi.mock("@/agent/events", () => ({
  listenForNotebookEvents: mocks.listenForNotebookEvents,
}));

vi.mock("@/stores/notebook", async () => {
  const React = await vi.importActual<typeof import("react")>("react");

  return {
    Notebook: vi.fn().mockImplementation(() => {
      if (!mocks.store) throw new Error("Notebook store mock not configured");
      return {
        loadNotebookFromPath: mocks.loadNotebookFromPath,
        state: mocks.store.getState(),
        store: mocks.store,
      };
    }),
    NotebookContext: React.createContext(undefined),
  };
});

vi.mock("wouter", () => ({
  useSearch: () => "path=%2Ftmp%2Fapp-mode.ipynb",
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

vi.mock("@/ui/notebook/NotebookView", () => ({
  default: () => <div data-testid="notebook-view" />,
}));

function createNotebookStore(viewMode: "cells" | "dag" | "app") {
  return createStore<any>()(() => ({
    serverState: {
      cellIds: [],
      cells: {},
    },
    viewState: {
      loadError: null,
      viewMode,
    },
  }));
}

describe("NotebookPage", () => {
  beforeEach(() => {
    mocks.listenForNotebookEvents.mockReset();
    mocks.loadNotebookFromPath.mockReset();
    mocks.setActiveAgentNotebook.mockReset();
  });

  afterEach(() => {
    cleanup();
    mocks.store = undefined;
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
});
