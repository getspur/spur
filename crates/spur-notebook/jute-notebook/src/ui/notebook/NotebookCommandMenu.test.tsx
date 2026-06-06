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

import NotebookCommandMenu from "./NotebookCommandMenu";

const invokeMock = vi.hoisted(() => vi.fn());
const saveDialogMock = vi.hoisted(() => vi.fn());
const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
        saveNow: ReturnType<typeof vi.fn>;
        execute: ReturnType<typeof vi.fn>;
        interruptKernel: ReturnType<typeof vi.fn>;
        restartKernel: ReturnType<typeof vi.fn>;
        setCellType: ReturnType<typeof vi.fn>;
      }
    | undefined,
}));

vi.mock("@/agent/deck", () => ({
  dispatchDeckCommand: vi.fn(),
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    if (!mocks.notebook) throw new Error("Notebook mock not configured");
    return mocks.notebook;
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: saveDialogMock,
}));

function notebookWithPath(path: string | null) {
  return {
    store: createStore<any>()(() => ({
      serverState: { cells: {} },
      viewState: { selectedCellId: null, path },
    })),
    saveNow: vi.fn(() => Promise.resolve()),
    execute: vi.fn(),
    interruptKernel: vi.fn(),
    restartKernel: vi.fn(),
    setCellType: vi.fn(),
  };
}

function openCommandMenu() {
  fireEvent.keyDown(document, { key: "k", metaKey: true });
}

function publishCommandItem() {
  return screen
    .getByText("Publish Spur App...")
    .closest("[cmdk-item], [role='option']");
}

describe("NotebookCommandMenu", () => {
  beforeEach(() => {
    Object.defineProperty(window.HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: vi.fn(),
    });
    vi.stubGlobal(
      "ResizeObserver",
      class ResizeObserver {
        observe() {}
        unobserve() {}
        disconnect() {}
      },
    );
    invokeMock.mockReset();
    invokeMock.mockResolvedValue({
      path: "/tmp/forecast.spurapp",
      manifest: {},
      assetCount: 0,
      preflight: {},
    });
    saveDialogMock.mockReset();
    saveDialogMock.mockResolvedValue("/tmp/forecast.spurapp");
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    mocks.notebook = undefined;
  });

  test("publish command is disabled until the notebook has a path", () => {
    mocks.notebook = notebookWithPath(null);
    render(<NotebookCommandMenu />);

    openCommandMenu();

    expect(publishCommandItem()).toHaveAttribute("aria-disabled", "true");
  });

  test("publish command saves before invoking backend export", async () => {
    const order: string[] = [];
    mocks.notebook = notebookWithPath("/tmp/forecast.ipynb");
    mocks.notebook.saveNow.mockImplementation(async () => {
      order.push("save");
    });
    saveDialogMock.mockImplementation(async () => {
      order.push("dialog");
      return "/tmp/forecast.spurapp";
    });
    invokeMock.mockImplementation(async () => {
      order.push("invoke");
      return {
        path: "/tmp/forecast.spurapp",
        manifest: {},
        assetCount: 0,
        preflight: {},
      };
    });

    render(<NotebookCommandMenu />);
    openCommandMenu();
    fireEvent.click(screen.getByText("Publish Spur App..."));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("publish_spur_app", {
        notebookPath: "/tmp/forecast.ipynb",
        outputPath: "/tmp/forecast.spurapp",
        name: "forecast",
        includePortSnapshots: false,
      }),
    );
    expect(saveDialogMock).toHaveBeenCalledWith({
      title: "Publish Spur App",
      defaultPath: "/tmp/forecast.spurapp",
      filters: [{ name: "Spur App", extensions: ["spurapp"] }],
    });
    expect(order).toEqual(["save", "dialog", "invoke"]);
  });

  test("publish command stops after cancelled save dialog", async () => {
    saveDialogMock.mockResolvedValue(null);
    mocks.notebook = notebookWithPath("/tmp/forecast.ipynb");

    render(<NotebookCommandMenu />);
    openCommandMenu();
    fireEvent.click(screen.getByText("Publish Spur App..."));

    await waitFor(() => expect(mocks.notebook?.saveNow).toHaveBeenCalled());
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
