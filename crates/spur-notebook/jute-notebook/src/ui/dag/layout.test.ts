import { describe, expect, test } from "vitest";

import { layoutDagGraph } from "./layout";
import type { DagGraph } from "./useDagGraph";

function graph(): DagGraph {
  return {
    nodes: [
      {
        id: "root",
        data: {
          id: "root",
          label: "root",
          cellType: "code",
          code: "root = load()",
          codePreview: "root = load()",
          produces: [{ port: "root", repr: "dataframe", version: 1 }],
          consumes: [],
          state: "never-run",
        },
      },
      {
        id: "left",
        data: {
          id: "left",
          label: "left",
          cellType: "code",
          code: "left = root",
          codePreview: "left = root",
          produces: [{ port: "left", repr: "dataframe", version: 1 }],
          consumes: [{ port: "root", version: 1 }],
          state: "never-run",
        },
      },
      {
        id: "right",
        data: {
          id: "right",
          label: "right",
          cellType: "code",
          code: "right = root",
          codePreview: "right = root",
          produces: [{ port: "right", repr: "dataframe", version: 1 }],
          consumes: [{ port: "root", version: 1 }],
          state: "never-run",
        },
      },
      {
        id: "join",
        data: {
          id: "join",
          label: "join",
          cellType: "code",
          code: "join = left + right",
          codePreview: "join = left + right",
          produces: [{ port: "join", repr: "dataframe", version: 1 }],
          consumes: [
            { port: "left", version: 1 },
            { port: "right", version: 1 },
          ],
          state: "never-run",
        },
      },
    ],
    edges: [
      { id: "root->left:root", source: "root", target: "left", port: "root" },
      { id: "root->right:root", source: "root", target: "right", port: "root" },
      { id: "left->join:left", source: "left", target: "join", port: "left" },
      {
        id: "right->join:right",
        source: "right",
        target: "join",
        port: "right",
      },
    ],
  };
}

describe("layoutDagGraph", () => {
  test("lays out diamond graphs top-down", () => {
    const laidOut = layoutDagGraph(graph());
    const positions = Object.fromEntries(
      laidOut.nodes.map((node) => [node.id, node.position]),
    );

    expect(positions.root.y).toBeLessThan(positions.left.y);
    expect(positions.root.y).toBeLessThan(positions.right.y);
    expect(positions.left.y).toBeLessThan(positions.join.y);
    expect(positions.right.y).toBeLessThan(positions.join.y);
  });
});
