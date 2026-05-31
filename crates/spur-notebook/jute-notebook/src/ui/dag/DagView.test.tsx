import { render } from "@testing-library/react";
import type { ReactNode } from "react";
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

vi.mock("@xyflow/react", () => ({
  Background: () => <div data-testid="dag-background" />,
  Controls: () => <div data-testid="dag-controls" />,
  Handle: () => null,
  MarkerType: { ArrowClosed: "arrowclosed" },
  MiniMap: () => <div data-testid="dag-minimap" />,
  Position: { Bottom: "bottom", Top: "top" },
  ReactFlow: ({
    children,
    edges,
    nodes,
    nodeTypes,
  }: {
    children: ReactNode;
    edges: Array<{
      id: string;
      label?: string;
      source: string;
      target: string;
    }>;
    nodes: Array<{ id: string; data: unknown; type?: string }>;
    nodeTypes: Record<
      string,
      (props: { data: unknown; selected: boolean }) => ReactNode
    >;
  }) => (
    <div data-testid="react-flow">
      {nodes.map((node) => {
        const NodeComponent = nodeTypes[node.type ?? ""];
        return (
          <div key={node.id} data-node-id={node.id}>
            {NodeComponent({ data: node.data, selected: false })}
          </div>
        );
      })}
      {edges.map((edge) => (
        <div
          key={edge.id}
          data-edge={`${edge.source}->${edge.target}:${edge.label}`}
        >
          {edge.label}
        </div>
      ))}
      {children}
    </div>
  ),
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
    expect(
      container.querySelector(
        '[data-edge="source-cell->consumer-cell:customers"]',
      ),
    ).not.toBeNull();
  });
});
