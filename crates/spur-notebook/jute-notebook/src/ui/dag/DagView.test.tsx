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

type TestCellState = {
  type: string;
  initialText: string;
  source: string;
  version: number;
  dagMetadata?: {
    produces: Array<{ port: string; repr: string; display?: string }>;
    consumes: string[];
    source?: { kind: string; port: string };
  };
};

type TestStoreState = {
  serverState: {
    lastAppliedVersion: number;
    notebookMetadata: Record<string, never>;
    cellIds: string[];
    cells: Record<string, TestCellState>;
  };
  viewState: {
    selectedCellId: string | null;
    isLoading: boolean;
    viewMode: string;
  };
  editBuffer: {
    cellSources: Record<
      string,
      { source: string; version: number; lastEditedBy?: string }
    >;
  };
  dagStatus: Record<
    string,
    {
      state:
        | "fresh"
        | "stale"
        | "running"
        | "failed"
        | "upstream-failed"
        | "never-run";
      ranPortVersions: Record<string, number>;
      executionCount?: number;
    }
  >;
  dagPortManifest: Record<string, number>;
};

const invokeMock = vi.hoisted(() => vi.fn());
const executeMock = vi.hoisted(() => vi.fn());
const storeListeners = vi.hoisted(() => new Set<() => void>());
const storeState = vi.hoisted<TestStoreState>(() => ({
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
  dagPortManifest: {},
}));

function applyDagStatusSnapshot(snapshot: {
  nodes: Array<{
    execution_count?: number | null;
    id: string;
    ranPortVersions?: Record<string, number>;
    ran_port_versions?: Record<string, number>;
    state:
      | string
      | {
          execution_count?: number | null;
        };
  }>;
  port_manifest: Record<string, number>;
}) {
  storeState.dagPortManifest = snapshot.port_manifest;
  for (const node of snapshot.nodes) {
    const state =
      typeof node.state === "string"
        ? node.state
        : node.state.execution_count
          ? "fresh"
          : "never-run";
    storeState.dagStatus[node.id] = {
      state: state as TestStoreState["dagStatus"][string]["state"],
      ranPortVersions:
        node.ranPortVersions ??
        node.ran_port_versions ??
        (state === "running" || state === "fresh"
          ? Object.fromEntries(
              storeState.serverState.cells[node.id].dagMetadata?.consumes.map(
                (port) => [port, snapshot.port_manifest[port]],
              ) ?? [],
            )
          : (storeState.dagStatus[node.id]?.ranPortVersions ?? {})),
      executionCount: node.execution_count ?? undefined,
    };
  }
  storeListeners.forEach((listener) => listener());
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => ({
    applyDagStatusSnapshot,
    execute: executeMock,
    store: {
      getInitialState: () => storeState,
      getState: () => storeState,
      subscribe: (listener: () => void) => {
        storeListeners.add(listener);
        return () => storeListeners.delete(listener);
      },
    },
  }),
}));

vi.mock("../notebook/CellInput", () => ({
  default: ({ cellId }: { cellId: string }) => (
    <textarea
      aria-label="Selected DAG node code"
      defaultValue={storeState.serverState.cells[cellId].initialText}
      onChange={(event) => {
        storeState.editBuffer.cellSources[cellId] = {
          source: event.target.value,
          version: storeState.serverState.cells[cellId].version + 1,
        };
        storeListeners.forEach((listener) => listener());
      }}
    />
  ),
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
    executeMock.mockReset();
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
    storeState.editBuffer.cellSources = {};
    storeState.dagStatus = {};
    storeState.dagPortManifest = {};
    storeListeners.clear();
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

  test("seeds status on mount and selects an inspector node", async () => {
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
    expect(await screen.findByLabelText("Selected DAG node code")).toHaveValue(
      "summary = customers.describe()",
    );
  });

  test("edits and runs the selected inspector node through DAG plumbing", async () => {
    executeMock.mockResolvedValue(undefined);
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "notebook_dag_status") {
        return {
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
        };
      }
      if (command === "daemon_control") {
        return { ok: true, result: { type: "empty", data: {} } };
      }
      if (command === "notebook_run_cell")
        return { cell_id: "consumer-cell", status: "fresh" };
      throw new Error(`unexpected invoke: ${command}`);
    });

    render(<DagView />);

    fireEvent.click(
      screen.getByRole("button", { name: /select consumer-cell/i }),
    );

    const editor = screen.getByLabelText("Selected DAG node code");
    fireEvent.change(editor, {
      target: { value: "summary = customers.head()" },
    });

    expect(storeState.editBuffer.cellSources["consumer-cell"]).toMatchObject({
      source: "summary = customers.head()",
    });
    expect(screen.getByText("Edited")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /run node/i }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("daemon_control", {
        cmd: {
          command: "apply_edit",
          id: "consumer-cell",
          source: "summary = customers.head()",
        },
      });
      expect(invokeMock).toHaveBeenCalledWith("notebook_run_cell", {
        cellId: "consumer-cell",
      });
    });
    expect(executeMock).not.toHaveBeenCalled();
    expect(screen.queryByText("Edited")).not.toBeInTheDocument();
  });

  test("renders downstream stale after a pushed DAG status update", async () => {
    render(<DagView />);

    const edge = await screen.findByTestId(
      "source-cell->consumer-cell:customers",
    );
    expect(edge).toHaveAttribute("data-edge-animated", "false");

    applyDagStatusSnapshot({
      nodes: [
        {
          id: "source-cell",
          state: "fresh",
          ranPortVersions: {},
          execution_count: 2,
        },
        {
          id: "consumer-cell",
          state: "stale",
          ran_port_versions: { customers: 2 },
          execution_count: 1,
        },
      ],
      port_manifest: { customers: 3, summary: 1 },
    });

    await waitFor(() => {
      expect(edge).toHaveAttribute("data-edge-animated", "true");
      expect(edge).toHaveStyle({ stroke: "#d97706" });
    });

    fireEvent.click(
      screen.getByRole("button", { name: /select consumer-cell/i }),
    );
    expect(screen.getByRole("complementary")).toHaveTextContent("stale");
  });

  test("runs downstream from the inspector and stale roots from the DAG header", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "notebook_dag_status") {
        return {
          notebook_version: 3,
          nodes: [
            {
              id: "source-cell",
              state: "fresh",
              execution_count: 1,
            },
            {
              id: "consumer-cell",
              state: "stale",
              execution_count: 1,
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
        };
      }
      if (command === "notebook_run_cascade") return { runs: [] };
      throw new Error(`unexpected invoke: ${command}`);
    });

    render(<DagView />);

    await screen.findByRole("button", { name: /run stale \(1\)/i });

    fireEvent.click(
      screen.getByRole("button", { name: /select source-cell/i }),
    );
    fireEvent.click(screen.getByRole("button", { name: /run downstream/i }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("notebook_run_cascade", {
        cellId: "source-cell",
      });
    });

    fireEvent.click(screen.getByRole("button", { name: /run stale \(1\)/i }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("notebook_run_cascade", {
        cellId: "consumer-cell",
      });
    });
  });
});
