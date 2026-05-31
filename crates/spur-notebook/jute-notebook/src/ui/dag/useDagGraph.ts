import type { NodeStatus, NotebookCellState } from "@/stores/notebook";

import { type DagPortManifest, staleConsumedPorts } from "./dagStatus";

export type DagNodeState = NodeStatus["state"];

export type DagProducedPort = {
  port: string;
  repr: string;
  display?: string;
  version: number;
};

export type DagConsumedPort = {
  port: string;
  version?: number;
  ranVersion?: number;
  stale?: boolean;
};

export type DagNodeData = {
  id: string;
  label: string;
  cellType: NotebookCellState["type"];
  code: string;
  codePreview: string;
  produces: DagProducedPort[];
  consumes: DagConsumedPort[];
  source?: string;
  state: DagNodeState;
  onSelect?: (id: string) => void;
};

export type DagGraphNode = {
  id: string;
  data: DagNodeData;
};

export type DagGraphEdge = {
  id: string;
  source: string;
  target: string;
  port: string;
  stale?: boolean;
};

export type DagGraph = {
  nodes: DagGraphNode[];
  edges: DagGraphEdge[];
};

export function buildDagGraph(
  cellIds: string[],
  cells: Record<string, NotebookCellState>,
  options: {
    dagStatus?: Record<string, NodeStatus>;
    portManifest?: DagPortManifest;
  } = {},
): DagGraph {
  const dagStatus = options.dagStatus ?? {};
  const portManifest = options.portManifest ?? {};
  const dagCellIds = cellIds.filter((id) => isDagCell(cells[id]));
  const producerByPort = new Map<string, string>();
  const producerVersionByPort = new Map<string, number>();
  const consumersByPort = new Map<string, Set<string>>();

  for (const id of dagCellIds) {
    const cell = cells[id];
    const dagMetadata = cell.dagMetadata;
    if (!dagMetadata) continue;

    for (const produced of dagMetadata.produces) {
      if (!producerByPort.has(produced.port)) {
        producerByPort.set(produced.port, id);
        producerVersionByPort.set(produced.port, cell.version);
      }
    }

    for (const consumedPort of dagMetadata.consumes) {
      const consumers = consumersByPort.get(consumedPort) ?? new Set<string>();
      consumers.add(id);
      consumersByPort.set(consumedPort, consumers);
    }
  }

  return {
    nodes: dagCellIds.map((id) => {
      const cell = cells[id];
      const dagMetadata = cell.dagMetadata;

      return {
        id,
        data: {
          id,
          label: id,
          cellType: cell.type,
          code: cell.source,
          codePreview: firstSourceLine(cell.source),
          produces:
            dagMetadata?.produces.map((port) => ({
              port: port.port,
              repr: port.repr,
              display: port.display,
              version: portManifest[port.port] ?? cell.version,
            })) ?? [],
          consumes:
            dagMetadata?.consumes.map((port) => {
              const status = dagStatus[id];
              const ranVersion = status?.ranPortVersions[port];
              const stale = staleConsumedPorts(status, portManifest).includes(
                port,
              );
              return {
                port,
                version: portManifest[port] ?? producerVersionByPort.get(port),
                ...(ranVersion !== undefined ? { ranVersion } : {}),
                ...(stale ? { stale } : {}),
              };
            }) ?? [],
          source: formatSource(dagMetadata?.source),
          state: dagStatus[id]?.state ?? "never-run",
        },
      };
    }),
    edges: sortedPorts(consumersByPort).flatMap((port) => {
      const producer = producerByPort.get(port);
      if (!producer) return [];

      return Array.from(consumersByPort.get(port) ?? [])
        .sort()
        .map((consumer) => ({
          id: `${producer}->${consumer}:${port}`,
          source: producer,
          target: consumer,
          port,
          stale: staleConsumedPorts(dagStatus[consumer], portManifest).includes(
            port,
          ),
        }));
    }),
  };
}

function isDagCell(
  cell: NotebookCellState | undefined,
): cell is NotebookCellState {
  const dagMetadata = cell?.dagMetadata;
  return Boolean(
    dagMetadata &&
      (dagMetadata.produces.length > 0 ||
        dagMetadata.consumes.length > 0 ||
        dagMetadata.source),
  );
}

function firstSourceLine(source: string): string {
  return source.split(/\r?\n/, 1)[0]?.trim() || "(empty)";
}

function formatSource(
  source: NonNullable<NotebookCellState["dagMetadata"]>["source"],
): string | undefined {
  return source ? `${source.kind}:${source.port}` : undefined;
}

function sortedPorts(consumersByPort: Map<string, Set<string>>): string[] {
  return Array.from(consumersByPort.keys()).sort();
}
