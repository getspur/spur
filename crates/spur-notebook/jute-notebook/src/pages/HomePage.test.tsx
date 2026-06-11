import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { useNotebookTabsStore } from "@/stores/notebook";

import HomePage from "./HomePage";

const mocks = vi.hoisted(() => ({
  daemonControl: vi.fn(),
  invoke: vi.fn(),
  listenForRecentNotebookChanges: vi.fn(),
  openDialog: vi.fn(),
  saveDialog: vi.fn(),
  setLocation: vi.fn(),
}));

vi.mock("@/agent/events", () => ({
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

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mocks.openDialog,
  save: mocks.saveDialog,
}));

vi.mock("wouter", () => ({
  useLocation: () => ["/", mocks.setLocation],
}));

function recentNotebook(path: string) {
  return {
    path,
    lastOpened: "2026-06-11T00:00:00Z",
    isScratch: false,
    pinned: false,
  };
}

describe("HomePage", () => {
  beforeEach(() => {
    mocks.daemonControl.mockReset();
    mocks.daemonControl.mockImplementation(async (cmd) => {
      if (cmd.command === "list_recents") {
        return {
          ok: true,
          entries: [recentNotebook("/tmp/analysis.ipynb")],
        };
      }
      if (cmd.command === "open") {
        return { ok: true, path: cmd.path };
      }
      return { ok: true };
    });
    mocks.invoke.mockReset();
    mocks.listenForRecentNotebookChanges.mockReset();
    mocks.listenForRecentNotebookChanges.mockReturnValue(() => undefined);
    mocks.openDialog.mockReset();
    mocks.saveDialog.mockReset();
    mocks.setLocation.mockReset();
    useNotebookTabsStore.setState({
      tabs: [
        {
          id: "/tmp/current.ipynb",
          path: "/tmp/current.ipynb",
          title: "current.ipynb",
          dirty: false,
          kernelState: "idle",
          language: "python3",
          mode: "cells",
        },
      ],
      activeTabId: "/tmp/current.ipynb",
    });
  });

  afterEach(() => {
    cleanup();
    useNotebookTabsStore.setState({ tabs: [], activeTabId: undefined });
  });

  test("opens a recent notebook into the current tabbed notebook route", async () => {
    render(<HomePage />);

    fireEvent.click(
      await screen.findByRole("button", { name: /analysis.ipynb/i }),
    );

    await waitFor(() => {
      expect(mocks.daemonControl).toHaveBeenCalledWith({
        command: "open",
        path: "/tmp/analysis.ipynb",
        activate: false,
      });
    });
    expect(mocks.setLocation).toHaveBeenCalledWith(
      "/notebook?path=%2Ftmp%2Fcurrent.ipynb&path=%2Ftmp%2Fanalysis.ipynb&active=%2Ftmp%2Fanalysis.ipynb",
    );
  });
});
