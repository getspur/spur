import "@testing-library/jest-dom/vitest";
import {
  type RenderResult,
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import {
  dispose as disposeWidgetModel,
  get as getWidgetModel,
  set as setWidgetModel,
} from "@/stores/widgetRegistry";

import NotebookCells from "./NotebookCells";

const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
        addCell: ReturnType<typeof vi.fn>;
        clearResult: ReturnType<typeof vi.fn>;
        setCellType: ReturnType<typeof vi.fn>;
        setCellCodeType: ReturnType<typeof vi.fn>;
      }
    | undefined,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    if (!mocks.notebook) throw new Error("Notebook mock not configured");
    return mocks.notebook;
  },
}));

const WIDGET_VIEW_MIME = "application/vnd.jupyter.widget-view+json";
const AFM_MODEL_ID = "notebook-cells-afm-model";

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

function expectCellAccent(container: RenderResult["container"], color: string) {
  const accent = container.querySelector('span[class*="w-[3px]"]');
  expect(accent).toBeInTheDocument();
  expect(accent).toHaveStyle({ background: color });
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
      setCellCodeType: vi.fn(),
    };
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    disposeWidgetModel(AFM_MODEL_ID);
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
      setCellCodeType: vi.fn(),
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
      setCellCodeType: vi.fn(),
    };

    const { container } = render(<NotebookCells />);

    expect(screen.getByText("AI Agent")).toBeInTheDocument();
    expect(screen.getByText("● LIVE")).toBeInTheDocument();
    expect(screen.getByText(/✦\[\*\]/)).toHaveStyle({ color: "#7C3AED" });
    expectCellAccent(container, "#7C3AED");
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
      setCellCodeType: vi.fn(),
    };

    render(<NotebookCells />);

    expect(screen.getByText("AI Agent")).toBeInTheDocument();
    expect(screen.getByText("manual")).toBeInTheDocument();
  });

  test("updates AFM bound-port model state without reloading the iframe", async () => {
    setWidgetModel(AFM_MODEL_ID, {
      state: { preserved: true },
      esm: "export default { render() {} }",
    });
    mocks.notebook = {
      store: createNotebookStoreForCell({
        frontendMetadata: { binds: ["forecast"] },
        result: {
          status: "success",
          outputs: [
            {
              output_type: "display_data",
              data: {
                [WIDGET_VIEW_MIME]: {
                  version_major: 2,
                  version_minor: 1,
                  model_id: AFM_MODEL_ID,
                },
                "text/plain": "anywidget view",
              },
              metadata: {},
            },
          ],
        },
      }),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
      setCellCodeType: vi.fn(),
    };

    render(<NotebookCells />);

    const iframe = screen.getByTitle(`anywidget ${AFM_MODEL_ID}`);
    const srcDocBefore = iframe.getAttribute("srcdoc");

    await act(async () => {
      mocks.notebook?.store.setState((state: any) => ({
        ...state,
        dagStatus: {
          "cell-1": {
            state: "fresh",
            ranPortVersions: { forecast: 1 },
            executionCount: 7,
          },
        },
        dagPortManifest: { forecast: 2 },
      }));
    });

    expect(getWidgetModel(AFM_MODEL_ID)?.state.__jute_port_bindings).toEqual({
      cellId: "cell-1",
      binds: ["forecast"],
      ports: {
        forecast: {
          currentVersion: 2,
          executionCount: 7,
          ranVersion: 1,
          state: "fresh",
        },
      },
    });
    expect(getWidgetModel(AFM_MODEL_ID)?.state.preserved).toBe(true);
    expect(iframe.getAttribute("srcdoc")).toBe(srcDocBefore);
  });

  test("renders Python chip and accent for plain code cells", () => {
    mocks.notebook = {
      store: createNotebookStoreForCell({}),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
      setCellCodeType: vi.fn(),
    };

    const { container } = render(<NotebookCells />);

    expect(screen.getByText("Python")).toBeInTheDocument();
    expect(screen.getByText("[*]")).toHaveStyle({ color: "#3776AB" });
    expectCellAccent(container, "#3776AB");
    expect(screen.queryByText(/✦\[/)).not.toBeInTheDocument();
  });

  test("renders Rust chip and accent for Rust code cells", () => {
    mocks.notebook = {
      store: createNotebookStoreForCell({ codeType: "rust" }),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType: vi.fn(),
      setCellCodeType: vi.fn(),
    };

    const { container } = render(<NotebookCells />);

    expect(screen.getByText("Rust")).toBeInTheDocument();
    expect(screen.getByText("[*]")).toHaveStyle({ color: "#CE422B" });
    expectCellAccent(container, "#CE422B");
    expect(screen.queryByText(/✦\[/)).not.toBeInTheDocument();
  });

  test("opens language menu from chip and routes selections", () => {
    const setCellCodeType = vi.fn();
    const setCellType = vi.fn();
    mocks.notebook = {
      store: createNotebookStoreForCell({}),
      addCell: vi.fn(),
      clearResult: vi.fn(),
      setCellType,
      setCellCodeType,
    };

    render(<NotebookCells />);

    fireEvent.click(
      screen.getByRole("button", { name: "Change cell language: Python" }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "Rust" }));
    expect(setCellCodeType).toHaveBeenCalledWith("cell-1", "rust");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "Change cell language: Python" }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "Markdown" }));
    expect(setCellType).toHaveBeenCalledWith("cell-1", "markdown");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
});
