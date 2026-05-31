import { describe, expect, test } from "vitest";

import type { NotebookCellState } from "@/stores/notebook";

import { buildDagGraph } from "./useDagGraph";

function cell(
  source: string,
  dagMetadata: NonNullable<NotebookCellState["dagMetadata"]>,
  version: number,
): NotebookCellState {
  return {
    type: "code",
    initialText: source,
    source,
    version,
    dagMetadata,
  };
}

describe("buildDagGraph", () => {
  test("derives diamond edges from produced and consumed ports", () => {
    const graph = buildDagGraph(["plain", "root", "left", "right", "join"], {
      plain: {
        type: "markdown",
        initialText: "# Notes",
        source: "# Notes",
        version: 1,
      },
      root: cell(
        "root = load()",
        {
          produces: [{ port: "root", repr: "dataframe" }],
          consumes: [],
        },
        3,
      ),
      left: cell(
        "left = root.filter(kind='left')",
        {
          produces: [{ port: "left", repr: "dataframe" }],
          consumes: ["root"],
        },
        4,
      ),
      right: cell(
        "right = root.filter(kind='right')",
        {
          produces: [{ port: "right", repr: "dataframe" }],
          consumes: ["root"],
        },
        5,
      ),
      join: cell(
        "joined = left.merge(right)",
        {
          produces: [{ port: "joined", repr: "dataframe" }],
          consumes: ["left", "right"],
        },
        6,
      ),
    });

    expect(graph.nodes.map((node) => node.id)).toEqual([
      "root",
      "left",
      "right",
      "join",
    ]);
    expect(
      graph.edges.map((edge) => [edge.source, edge.target, edge.port]),
    ).toEqual([
      ["left", "join", "left"],
      ["right", "join", "right"],
      ["root", "left", "root"],
      ["root", "right", "root"],
    ]);
    expect(
      graph.nodes.find((node) => node.id === "left")?.data.consumes,
    ).toEqual([{ port: "root", version: 3 }]);
  });
});
