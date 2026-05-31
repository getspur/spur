import { render } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";

import DagView from "./DagView";

const storeState = vi.hoisted(() => ({
  serverState: {
    lastAppliedVersion: 0,
    notebookMetadata: {},
    cellIds: ["plain-cell", "source-cell", "consumer-cell"],
    cells: {
      "plain-cell": {
        type: "markdown",
        initialText: "# Notes",
        source: "# Notes",
        version: 1,
      },
      "source-cell": {
        type: "code",
        initialText: "customers = load()",
        source: "customers = load()",
        version: 1,
        dagMetadata: {
          produces: [{ port: "customers", repr: "dataframe" }],
          consumes: [],
        },
      },
      "consumer-cell": {
        type: "code",
        initialText: "summary = customers.describe()",
        source: "summary = customers.describe()",
        version: 1,
        dagMetadata: {
          produces: [{ port: "summary", repr: "dataframe" }],
          consumes: ["customers"],
          source: { kind: "cell", port: "customers" },
        },
      },
    },
  },
  viewState: {
    selectedCellId: null,
    isLoading: false,
    viewMode: "dag",
  },
  editBuffer: {
    cellSources: {},
  },
  dagStatus: {},
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => ({
    store: {
      getInitialState: () => storeState,
      getState: () => storeState,
      subscribe: () => () => {},
    },
  }),
}));

describe("DagView", () => {
  test("lists only cells with DAG metadata", () => {
    const { container } = render(<DagView />);

    expect(container).not.toHaveTextContent("plain-cell");
    expect(container).toHaveTextContent("source-cell");
    expect(container).toHaveTextContent("customers");
    expect(container).toHaveTextContent("consumer-cell");
    expect(container).toHaveTextContent("cell:customers");
    expect(container).toHaveTextContent("summary");
  });
});
