import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import {
  dispose as disposeWidgetModel,
  get as getWidgetModel,
  set as setWidgetModel,
} from "@/stores/widgetRegistry";

import AppMode from "./AppMode";

const mocks = vi.hoisted(() => ({
  notebook: undefined as
    | {
        store: StoreApi<any>;
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
const AFM_MODEL_ID = "app-mode-afm-model";

function widgetResult(modelId: string) {
  return {
    status: "success",
    outputs: [
      {
        output_type: "display_data",
        data: {
          [WIDGET_VIEW_MIME]: {
            version_major: 2,
            version_minor: 1,
            model_id: modelId,
          },
          "text/plain": "anywidget view",
        },
        metadata: {},
      },
    ],
  };
}

function createAppModeStore() {
  return createStore<any>()(() => ({
    serverState: {
      lastAppliedVersion: 0,
      notebookMetadata: {},
      cellIds: ["plain-cell", "frontend-second", "frontend-first"],
      cells: {
        "frontend-first": {
          type: "code",
          initialText: "",
          source: "",
          version: 1,
          frontendMetadata: { emits: ["summary"] },
          result: widgetResult("frontend-first-model"),
        },
        "frontend-second": {
          type: "code",
          initialText: "",
          source: "",
          version: 1,
          frontendMetadata: { binds: ["forecast"] },
          result: widgetResult(AFM_MODEL_ID),
        },
        "plain-cell": {
          type: "code",
          initialText: "",
          source: "",
          version: 1,
          result: {
            status: "success",
            outputs: [{ output_type: "stream", text: "plain output" }],
          },
        },
      },
    },
    viewState: {
      selectedCellId: null,
      isLoading: false,
      viewMode: "app",
    },
    editBuffer: {
      cellSources: {},
    },
    dagStatus: {
      "frontend-first": {
        state: "running",
        ranPortVersions: {},
        executionCount: 3,
      },
      "frontend-second": {
        state: "fresh",
        ranPortVersions: { forecast: 1 },
        executionCount: 2,
      },
    },
    dagPortManifest: { forecast: 1 },
  }));
}

describe("AppMode", () => {
  afterEach(() => {
    cleanup();
    disposeWidgetModel(AFM_MODEL_ID);
    disposeWidgetModel("frontend-first-model");
    mocks.notebook = undefined;
  });

  test("renders frontend cells only in document order with a status strip", async () => {
    setWidgetModel(AFM_MODEL_ID, {
      state: { preserved: true },
      esm: "export default { render() {} }",
    });
    mocks.notebook = {
      store: createAppModeStore(),
    };

    render(<AppMode />);

    expect(
      screen.getByRole("status", { name: "App mode status" }),
    ).toHaveTextContent("2 frontend cells");
    expect(
      screen.getByRole("status", { name: "App mode status" }),
    ).toHaveTextContent("1 running");
    expect(screen.queryByText("plain output")).not.toBeInTheDocument();

    expect(
      screen
        .getAllByRole("region", { name: /Frontend cell/ })
        .map((region) => region.getAttribute("aria-label")),
    ).toEqual([
      "Frontend cell frontend-second",
      "Frontend cell frontend-first",
    ]);
    expect(screen.getByTitle(`anywidget ${AFM_MODEL_ID}`)).toBeInTheDocument();
    expect(screen.getByText("frontend-first-model")).toBeInTheDocument();

    await act(async () => {
      mocks.notebook?.store.setState((state: any) => ({
        ...state,
        dagStatus: {
          ...state.dagStatus,
          "frontend-second": {
            state: "stale",
            ranPortVersions: { forecast: 1 },
            executionCount: 4,
          },
        },
        dagPortManifest: { forecast: 2 },
      }));
    });

    await waitFor(() =>
      expect(getWidgetModel(AFM_MODEL_ID)?.state.__jute_port_bindings).toEqual({
        cellId: "frontend-second",
        binds: ["forecast"],
        ports: {
          forecast: {
            currentVersion: 2,
            executionCount: 4,
            ranVersion: 1,
            state: "stale",
          },
        },
      }),
    );
    expect(getWidgetModel(AFM_MODEL_ID)?.state.preserved).toBe(true);
  });
});
