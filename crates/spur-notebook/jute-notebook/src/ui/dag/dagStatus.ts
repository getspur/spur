import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import type { NodeStatus } from "@/stores/notebook";

export type DagStatusState = NodeStatus["state"];

export type DagPortManifest = Record<string, number>;

export type NotebookDagStatusSnapshot = {
  notebook_version: number;
  nodes: NotebookDagStatusNode[];
  edges: NotebookDagStatusEdge[];
  port_manifest: DagPortManifest;
};

export type NotebookDagStatusNode = {
  id: string;
  state: {
    kind: string;
    version?: number | null;
    execution_count?: number | null;
  };
  dag: {
    produces?: Array<{ port: string; repr: string; display?: string }>;
    consumes?: string[];
    source?: { kind: string; port: string };
  };
};

export type NotebookDagStatusEdge = {
  producer: string;
  consumer: string;
  port: string;
};

type Invoke = (
  command: string,
  args: Record<string, never>,
) => Promise<unknown>;

export async function loadNotebookDagStatus(
  invoke: Invoke = tauriInvoke,
): Promise<NotebookDagStatusSnapshot> {
  const response = await invoke("notebook_dag_status", {});
  return readStructuredContent(response);
}

export function buildDagStatusMap(
  snapshot: NotebookDagStatusSnapshot,
): Record<string, NodeStatus> {
  return Object.fromEntries(
    snapshot.nodes.map((node) => {
      const executionCount = node.state.execution_count ?? undefined;
      const state = deriveSeedNodeState(executionCount);

      return [
        node.id,
        {
          state,
          // TODO(t5): populate run-input port versions from dag_status_changed.
          // The seed snapshot does not persist the port versions a cell last
          // ran against, so true stale/failed/running states cannot be derived
          // from notebook_dag_status alone.
          ranPortVersions: {},
          executionCount,
        },
      ];
    }),
  );
}

export function staleConsumedPorts(
  status: NodeStatus | undefined,
  portManifest: DagPortManifest,
): string[] {
  if (!status) return [];

  return Object.entries(status.ranPortVersions)
    .filter(([port, ranVersion]) => {
      const currentVersion = portManifest[port];
      return currentVersion !== undefined && currentVersion > ranVersion;
    })
    .map(([port]) => port);
}

function deriveSeedNodeState(
  executionCount: number | undefined,
): DagStatusState {
  return executionCount && executionCount > 0 ? "fresh" : "never-run";
}

function readStructuredContent(response: unknown): NotebookDagStatusSnapshot {
  if (isRecord(response)) {
    const structured =
      response.structured_content ?? response.structuredContent ?? response;
    if (isNotebookDagStatusSnapshot(structured)) return structured;
  }
  throw new Error("notebook_dag_status returned an invalid payload");
}

function isNotebookDagStatusSnapshot(
  value: unknown,
): value is NotebookDagStatusSnapshot {
  if (!isRecord(value)) return false;
  return (
    typeof value.notebook_version === "number" &&
    Array.isArray(value.nodes) &&
    Array.isArray(value.edges) &&
    isRecord(value.port_manifest)
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
