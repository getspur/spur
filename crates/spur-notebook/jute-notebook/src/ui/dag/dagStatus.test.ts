import { describe, expect, test, vi } from "vitest";

import {
  buildDagStatusMap,
  loadNotebookDagStatus,
  staleConsumedPorts,
} from "./dagStatus";

describe("dagStatus", () => {
  test("derives only fresh and never-run from the seed snapshot shape", () => {
    const status = buildDagStatusMap({
      notebook_version: 12,
      nodes: [
        {
          id: "fresh-cell",
          state: {
            kind: "code",
            version: 4,
            execution_count: 2,
          },
          dag: {
            produces: [],
            consumes: ["customers"],
          },
        },
        {
          id: "never-run-cell",
          state: {
            kind: "code",
            version: 5,
            execution_count: null,
          },
          dag: {
            produces: [],
            consumes: ["customers"],
          },
        },
      ],
      edges: [],
      port_manifest: { customers: 3 },
    });

    expect(status["fresh-cell"]).toMatchObject({
      state: "fresh",
      executionCount: 2,
      ranPortVersions: {},
    });
    expect(status["never-run-cell"]).toMatchObject({
      state: "never-run",
      ranPortVersions: {},
    });
  });

  test("detects stale consumed ports from live run-input versions", () => {
    expect(
      staleConsumedPorts(
        { state: "fresh", ranPortVersions: { customers: 2 } },
        { customers: 3 },
      ),
    ).toEqual(["customers"]);
  });

  test("calls notebook_dag_status and accepts the real command payload shape", async () => {
    const invoke = vi.fn().mockResolvedValue({
      notebook_version: 2,
      nodes: [
        {
          id: "root",
          state: {
            kind: "code",
            version: 1,
            execution_count: 1,
          },
          dag: {
            produces: [{ port: "root", repr: "dataframe" }],
            consumes: [],
          },
        },
      ],
      edges: [],
      port_manifest: { root: 1 },
    });

    const snapshot = await loadNotebookDagStatus(invoke);

    expect(invoke).toHaveBeenCalledWith("notebook_dag_status", {});
    expect(snapshot.nodes.map((node) => node.id)).toEqual(["root"]);
  });
});
