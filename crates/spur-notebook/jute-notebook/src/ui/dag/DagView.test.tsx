import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import type { CSSProperties, ReactNode } from "react";
import { beforeEach, describe, expect, test, vi } from "vitest";

import DagView from "./DagView";

const invokeMock = vi.hoisted(() => vi.fn());
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

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
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
      animated?: boolean;
      id: string;
      label?: string;
      source: string;
      style?: CSSProperties;
      target: string;
    }>;
    nodes: Array<{ id: string; data: unknown; type?: string }>;
    nodeTypes: Record<
      string,
      (props: { data: unknown; id?: string; selected: boolean }) => ReactNode
    >;
  }) => (
    <div data-testid="react-flow">
      {nodes.map((node) => {
        const NodeComponent = nodeTypes[node.type ?? ""];
        return (
          <div key={node.id} data-node-id={node.id}>
            {NodeComponent({ data: node.data, selected: false, id: node.id })}
          </div>
        );
      })}
      {edges.map((edge) => (
        <div
          key={edge.id}
          data-edge={`${edge.source}->${edge.target}:${edge.label}`}
          data-edge-animated={String(Boolean(edge.animated))}
          data-testid={`${edge.source}->${edge.target}:${edge.label}`}
          style={edge.style}
        >
          {edge.label}
        </div>
      ))}
      {children}
    </div>
  ),
}));

describe("DagView", () => {
  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue({
      notebook_version: 3,
      nodes: [
        {
          id: "source-cell",
          state: {
            kind: "code",
            version: 1,
            execution_count: 1,
          },
          dag: {
            produces: [{ port: "customers", repr: "dataframe" }],
            consumes: [],
          },
        },
        {
          id: "consumer-cell",
          state: {
            kind: "code",
            version: 1,
            execution_count: 1,
          },
          dag: {
            produces: [{ port: "summary", repr: "dataframe" }],
            consumes: ["customers"],
            source: { kind: "cell", port: "customers" },
          },
        },
      ],
      edges: [
        {
          producer: "source-cell",
          consumer: "consumer-cell",
          port: "customers",
        },
      ],
      port_manifest: { customers: 2, summary: 1 },
    });
  });

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

  test("seeds status on mount and selects a read-only inspector node", async () => {
    render(<DagView />);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("notebook_dag_status", {});
    });

    const edge = await screen.findByTestId(
      "source-cell->consumer-cell:customers",
    );
    expect(edge).toHaveAttribute("data-edge-animated", "false");
    expect(edge).toHaveStyle({ stroke: "#94a3b8" });

    fireEvent.click(
      screen.getByRole("button", { name: /select consumer-cell/i }),
    );

    expect(screen.getByRole("complementary")).toHaveTextContent(
      "consumer-cell",
    );
    expect(screen.getByRole("complementary")).toHaveTextContent("fresh");
    expect(screen.getByRole("complementary")).toHaveTextContent("customers");
    expect(screen.getByRole("complementary")).toHaveTextContent("v2");
    expect(screen.getByLabelText("Selected DAG node code")).toHaveAttribute(
      "readonly",
    );
    expect(screen.getByLabelText("Selected DAG node code")).toHaveValue(
      "summary = customers.describe()",
    );
  });
});
