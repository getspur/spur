import * as dagre from "@dagrejs/dagre";

import type { DagGraph, DagGraphNode } from "./useDagGraph";

export const DAG_NODE_WIDTH = 224;
export const DAG_NODE_HEIGHT = 92;

export type DagNodePosition = {
  x: number;
  y: number;
};

export type PositionedDagGraphNode = DagGraphNode & {
  position: DagNodePosition;
};

export type PositionedDagGraph = Omit<DagGraph, "nodes"> & {
  nodes: PositionedDagGraphNode[];
};

export function layoutDagGraph(graph: DagGraph): PositionedDagGraph {
  const dagreGraph = new dagre.graphlib.Graph();
  dagreGraph.setDefaultEdgeLabel(() => ({}));
  dagreGraph.setGraph({
    rankdir: "TB",
    nodesep: 56,
    ranksep: 84,
    marginx: 24,
    marginy: 24,
  });

  for (const node of graph.nodes) {
    dagreGraph.setNode(node.id, {
      width: DAG_NODE_WIDTH,
      height: DAG_NODE_HEIGHT,
    });
  }

  for (const edge of graph.edges) {
    dagreGraph.setEdge(edge.source, edge.target);
  }

  dagre.layout(dagreGraph);

  return {
    nodes: graph.nodes.map((node) => {
      const position = dagreGraph.node(node.id) as DagNodePosition | undefined;
      const x = position ? position.x - DAG_NODE_WIDTH / 2 : 0;
      const y = position ? position.y - DAG_NODE_HEIGHT / 2 : 0;

      return {
        ...node,
        position: { x, y },
      };
    }),
    edges: graph.edges,
  };
}
