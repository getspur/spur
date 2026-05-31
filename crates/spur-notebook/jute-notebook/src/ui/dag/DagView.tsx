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
import { useMemo } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { useNotebook } from "@/stores/notebook";

import DagNode from "./DagNode";
import { type PositionedDagGraphNode, layoutDagGraph } from "./layout";
import { type DagNodeData, buildDagGraph } from "./useDagGraph";

type FlowDagNode = Node<DagNodeData, "dagCell">;

const nodeTypes = {
  dagCell: DagFlowNode,
} satisfies NodeTypes;

export default function DagView() {
  const notebook = useNotebook();
  const [cellIds, cells] = useStore(
    notebook.store,
    useShallow((state) => [state.serverState.cellIds, state.serverState.cells]),
  );
  const graph = useMemo(
    () => layoutDagGraph(buildDagGraph(cellIds, cells)),
    [cellIds, cells],
  );
  const nodes = useMemo(() => toFlowNodes(graph.nodes), [graph.nodes]);
  const edges = useMemo(() => toFlowEdges(graph.edges), [graph.edges]);

  if (nodes.length === 0) {
    return (
      <section className="mx-auto mt-8 w-full max-w-4xl px-6 text-sm text-gray-500">
        No DAG cells found.
      </section>
    );
  }

  return (
    <section className="h-[calc(100vh-9rem)] min-h-[520px] w-full px-4 py-4">
      <div className="h-full overflow-hidden rounded border border-gray-200 bg-gray-50">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          fitView
          fitViewOptions={{ padding: 0.2 }}
          minZoom={0.2}
          maxZoom={1.5}
        >
          <Background gap={20} color="#e5e7eb" />
          <MiniMap pannable zoomable nodeColor="#d1d5db" />
          <Controls showInteractive={false} />
        </ReactFlow>
      </div>
    </section>
  );
}

function DagFlowNode({ data, selected }: NodeProps<FlowDagNode>) {
  return (
    <>
      <Handle type="target" position={Position.Top} isConnectable={false} />
      <DagNode data={data} selected={selected} />
      <Handle type="source" position={Position.Bottom} isConnectable={false} />
    </>
  );
}

function toFlowNodes(nodes: PositionedDagGraphNode[]): FlowDagNode[] {
  return nodes.map((node) => ({
    id: node.id,
    type: "dagCell",
    position: node.position,
    data: node.data,
    draggable: false,
  }));
}

function toFlowEdges(
  edges: Array<{ id: string; source: string; target: string; port: string }>,
): Edge[] {
  return edges.map((edge) => ({
    id: edge.id,
    source: edge.source,
    target: edge.target,
    label: edge.port,
    type: "smoothstep",
    markerEnd: {
      type: MarkerType.ArrowClosed,
    },
    style: {
      stroke: "#94a3b8",
      strokeWidth: 1.5,
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
