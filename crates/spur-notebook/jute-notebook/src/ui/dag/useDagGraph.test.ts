import { describe, expect, test } from "vitest";

import type { NotebookCellState } from "@/stores/notebook";

import { buildDagGraph } from "./useDagGraph";

function cell(
  source: string,
  dagMetadata: NonNullable<NotebookCellState["dagMetadata"]>,
  version: number,
  overrides: Partial<NotebookCellState> = {},
): NotebookCellState {
  return {
    type: "code",
    initialText: source,
    source,
    version,
    dagMetadata,
    ...overrides,
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

  test("derives diamond edges when legacy metadata omits empty arrays", () => {
    const graph = buildDagGraph(["root", "left", "right", "join"], {
      root: cell(
        "root = load()",
        {
          produces: [{ port: "root", repr: "dataframe" }],
        } as NonNullable<NotebookCellState["dagMetadata"]>,
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
          consumes: ["left", "right"],
        } as NonNullable<NotebookCellState["dagMetadata"]>,
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
      graph.nodes.find((node) => node.id === "join")?.data.produces,
    ).toEqual([]);
  });

  test("marks stale edges from live dag status run-input versions", () => {
    const graph = buildDagGraph(
      ["root", "consumer"],
      {
        root: cell(
          "root = load()",
          {
            produces: [{ port: "root", repr: "dataframe" }],
            consumes: [],
          },
          3,
        ),
        consumer: cell(
          "consumer = root",
          {
            produces: [],
            consumes: ["root"],
          },
          4,
        ),
      },
      {
        dagStatus: {
          consumer: {
            state: "stale",
            ranPortVersions: { root: 1 },
          },
        },
        portManifest: { root: 2 },
      },
    );

    expect(graph.edges[0]).toMatchObject({ port: "root", stale: true });
    expect(graph.nodes.find((node) => node.id === "consumer")?.data.state).toBe(
      "stale",
    );
  });

  test("marks regular dag cells as code nodes", () => {
    const graph = buildDagGraph(["root"], {
      root: cell(
        "root = load()",
        {
          produces: [{ port: "root", repr: "dataframe" }],
          consumes: [],
        },
        3,
      ),
    });

    expect(graph.nodes[0]?.data.kind).toBe("code");
    expect(graph.nodes[0]?.data.aiLive).toBe(false);
  });

  test("marks spur kernelspec dag cells as ai nodes", () => {
    const graph = buildDagGraph(["summary"], {
      summary: cell(
        "Summarise the root dataframe",
        {
          produces: [{ port: "summary", repr: "text/plain" }],
          consumes: ["root"],
        },
        4,
        {
          cellMetadataOther: {
            kernelspec: { name: "spur" },
          },
        },
      ),
    });

    expect(graph.nodes[0]?.data.kind).toBe("ai");
    expect(graph.nodes[0]?.data.aiLive).toBe(false);
  });

  test("derives ai live from snake or camel case dag metadata", () => {
    const graph = buildDagGraph(["snake", "camel", "manual"], {
      snake: cell(
        "Live summary",
        {
          produces: [{ port: "snake", repr: "text/plain" }],
          consumes: [],
          ai_live: true,
        } as NonNullable<NotebookCellState["dagMetadata"]>,
        1,
        {
          cellMetadataOther: {
            kernelspec: { name: "spur" },
          },
        },
      ),
      camel: cell(
        "Live summary",
        {
          produces: [{ port: "camel", repr: "text/plain" }],
          consumes: [],
          aiLive: true,
        } as NonNullable<NotebookCellState["dagMetadata"]>,
        1,
        {
          cellMetadataOther: {
            kernelspec: { name: "spur" },
          },
        },
      ),
      manual: cell(
        "Manual summary",
        {
          produces: [{ port: "manual", repr: "text/plain" }],
          consumes: [],
        },
        1,
        {
          cellMetadataOther: {
            kernelspec: { name: "spur" },
          },
        },
      ),
    });

    expect(graph.nodes.find((node) => node.id === "snake")?.data.aiLive).toBe(
      true,
    );
    expect(graph.nodes.find((node) => node.id === "camel")?.data.aiLive).toBe(
      true,
    );
    expect(graph.nodes.find((node) => node.id === "manual")?.data.aiLive).toBe(
      false,
    );
  });
});
