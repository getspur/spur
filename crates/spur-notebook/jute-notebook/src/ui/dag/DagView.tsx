import {
  Background,
  Controls,
  type Edge,
  Handle,
  MarkerType,
  MiniMap,
  type Node,
  type NodeProps,
  type NodeTypes,
  Position,
  ReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { useNotebook } from "@/stores/notebook";

import DagInspector from "./DagInspector";
import DagNode from "./DagNode";
import HiddenCellsChip from "./HiddenCellsChip";
import { loadNotebookDagStatus, runNotebookCascade } from "./dagStatus";
import { type PositionedDagGraphNode, layoutDagGraph } from "./layout";
import { type DagGraph, type DagNodeData, buildDagGraph } from "./useDagGraph";

type FlowDagNode = Node<DagNodeData, "dagCell">;

const nodeTypes = {
  dagCell: DagFlowNode,
} satisfies NodeTypes;

export default function DagView() {
  const notebook = useNotebook();
  const [selectedNodeId, setSelectedNodeId] = useState<string | undefined>();
  const [isRunningStale, setIsRunningStale] = useState(false);
  const [runError, setRunError] = useState<string | undefined>();
  const [cellIds, cells] = useStore(
    notebook.store,
    useShallow((state) => [state.serverState.cellIds, state.serverState.cells]),
  );
  const [dagStatus, portManifest] = useStore(
    notebook.store,
    useShallow((state) => [state.dagStatus, state.dagPortManifest]),
  );
  const selectNode = useCallback((id: string) => setSelectedNodeId(id), []);
  const graph = useMemo(
    () =>
      layoutDagGraph(
        buildDagGraph(cellIds, cells, {
          dagStatus,
          portManifest,
        }),
      ),
    [cellIds, cells, dagStatus, portManifest],
  );
  const nodes = useMemo(
    () => toFlowNodes(graph.nodes, selectNode),
    [graph.nodes, selectNode],
  );
  const edges = useMemo(() => toFlowEdges(graph.edges), [graph.edges]);
  const selectedNode = useMemo(
    () => graph.nodes.find((node) => node.id === selectedNodeId)?.data,
    [graph.nodes, selectedNodeId],
  );
  const staleNodeIds = useMemo(() => staleNodes(graph), [graph]);
  const staleRootIds = useMemo(() => staleRoots(graph), [graph]);
  const staleCount = staleNodeIds.length;
  const dagNodeCount = nodes.length;

  useEffect(() => {
    if (dagNodeCount === 0) return;
    let cancelled = false;

    void loadNotebookDagStatus()
      .then((snapshot) => {
        if (cancelled) return;
        notebook.applyDagStatusSnapshot(snapshot);
      })
      .catch((error) => {
        console.warn("Failed to seed notebook DAG status", error);
      });

    return () => {
      cancelled = true;
    };
  }, [dagNodeCount]);

  const runStale = useCallback(async () => {
    if (staleRootIds.length === 0) return;
    setIsRunningStale(true);
    try {
      for (const cellId of staleRootIds) {
        await runNotebookCascade(cellId);
      }
    } catch (error) {
      setRunError(errorMessage(error));
    } finally {
      setIsRunningStale(false);
    }
  }, [staleRootIds]);

  if (nodes.length === 0) {
    return (
      <section className="mx-auto mt-8 w-full max-w-4xl px-6">
        <div className="mb-4 flex justify-end">
          <HiddenCellsChip cellIds={cellIds} cells={cells} />
        </div>
        <div className="rounded border border-dashed border-gray-300 bg-white px-6 py-8 text-sm">
          <h1 className="text-base font-semibold text-gray-950">
            No DAG cells yet
          </h1>
          <p className="mt-2 max-w-xl text-gray-500">
            Add produces or consumes metadata to a code cell and it will appear
            here as part of the notebook data flow.
          </p>
        </div>
      </section>
    );
  }

  return (
    <section className="h-[calc(100vh-9rem)] min-h-[520px] w-full px-4 py-4">
      <div className="relative flex h-full flex-col overflow-hidden rounded border border-gray-200 bg-gray-50">
        <header className="flex shrink-0 items-center justify-between border-b border-gray-200 bg-white px-4 py-3">
          <div>
            <h1 className="text-sm font-semibold text-gray-950">Data Flow</h1>
            <p className="text-xs text-gray-500">{nodes.length} DAG nodes</p>
          </div>
          <div className="flex items-center gap-2">
            <HiddenCellsChip cellIds={cellIds} cells={cells} />
            <button
              type="button"
              className="inline-flex items-center rounded border border-gray-200 bg-white px-3 py-1.5 text-xs font-medium text-gray-800 transition-colors hover:border-gray-300 hover:bg-gray-50 disabled:cursor-not-allowed disabled:opacity-60"
              disabled={staleCount === 0 || isRunningStale}
              onClick={() => {
                void runStale();
              }}
            >
              {isRunningStale ? "Running stale" : `Run stale (${staleCount})`}
            </button>
          </div>
        </header>
        <div className="flex min-h-0 flex-1">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            nodeTypes={nodeTypes}
            onNodeClick={(_, node) => setSelectedNodeId(node.id)}
            fitView
            fitViewOptions={{ padding: 0.2 }}
            minZoom={0.2}
            maxZoom={1.5}
            className="min-w-0 flex-1"
          >
            <Background gap={20} color="#e5e7eb" />
            <MiniMap pannable zoomable nodeColor="#d1d5db" />
            <Controls showInteractive={false} />
          </ReactFlow>
          <DagInspector
            node={selectedNode}
            onRunError={(error) => setRunError(errorMessage(error))}
            portManifest={portManifest}
            status={selectedNode ? dagStatus[selectedNode.id] : undefined}
          />
        </div>
        {runError ? (
          <div
            className="absolute bottom-6 right-6 max-w-sm rounded border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-800 shadow-lg"
            role="alert"
          >
            {runError}
          </div>
        ) : null}
      </div>
    </section>
  );
}

function DagFlowNode({ data, selected }: NodeProps<FlowDagNode>) {
  return (
    <>
      <Handle type="target" position={Position.Top} isConnectable={false} />
      <DagNode data={data} onSelect={data.onSelect} selected={selected} />
      <Handle type="source" position={Position.Bottom} isConnectable={false} />
    </>
  );
}

function toFlowNodes(
  nodes: PositionedDagGraphNode[],
  onSelect: (id: string) => void,
): FlowDagNode[] {
  return nodes.map((node) => ({
    id: node.id,
    type: "dagCell",
    position: node.position,
    data: { ...node.data, onSelect },
    draggable: false,
  }));
}

function toFlowEdges(
  edges: Array<{
    id: string;
    source: string;
    stale?: boolean;
    target: string;
    port: string;
  }>,
): Edge[] {
  return edges.map((edge) => ({
    id: edge.id,
    source: edge.source,
    target: edge.target,
    label: edge.port,
    type: "smoothstep",
    className: edge.stale ? "dag-edge-stale" : undefined,
    markerEnd: {
      type: MarkerType.ArrowClosed,
    },
    animated: Boolean(edge.stale),
    style: {
      stroke: edge.stale ? "#d97706" : "#94a3b8",
      strokeWidth: 1.5,
      strokeDasharray: edge.stale ? "6 4" : undefined,
    },
    labelStyle: {
      fill: "#475569",
      fontSize: 11,
      fontWeight: 600,
    },
    labelBgStyle: {
      fill: "#f8fafc",
      fillOpacity: 0.9,
    },
  }));
}

function staleNodes(graph: DagGraph): string[] {
  return graph.nodes
    .filter((node) => node.data.state === "stale")
    .map((node) => node.id);
}

function staleRoots(graph: DagGraph): string[] {
  const stale = new Set(staleNodes(graph));
  const staleWithStaleParent = new Set(
    graph.edges
      .filter((edge) => stale.has(edge.source) && stale.has(edge.target))
      .map((edge) => edge.target),
  );

  return topologicalOrder(graph)
    .filter((cellId) => stale.has(cellId))
    .filter((cellId) => !staleWithStaleParent.has(cellId));
}

function topologicalOrder(graph: DagGraph): string[] {
  const ids = graph.nodes.map((node) => node.id);
  const idSet = new Set(ids);
  const indegree = new Map(ids.map((id) => [id, 0]));
  const outgoing = new Map<string, string[]>();

  for (const edge of graph.edges) {
    if (!idSet.has(edge.source) || !idSet.has(edge.target)) continue;
    indegree.set(edge.target, (indegree.get(edge.target) ?? 0) + 1);
    outgoing.set(edge.source, [
      ...(outgoing.get(edge.source) ?? []),
      edge.target,
    ]);
  }

  const ready = ids.filter((id) => (indegree.get(id) ?? 0) === 0);
  const ordered: string[] = [];

  while (ready.length > 0) {
    const id = ready.shift();
    if (!id) break;
    ordered.push(id);
    for (const target of outgoing.get(id) ?? []) {
      const nextIndegree = (indegree.get(target) ?? 0) - 1;
      indegree.set(target, nextIndegree);
      if (nextIndegree === 0) ready.push(target);
    }
  }

  return ordered.length === ids.length
    ? ordered
    : [...ordered, ...ids.filter((id) => !ordered.includes(id))];
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "DAG run action failed";
}
