import type { NodeStatus, NotebookCellState } from "@/stores/notebook";

import { type DagPortManifest, staleConsumedPorts } from "./dagStatus";

export type DagNodeState = NodeStatus["state"];
export type DagNodeKind = "code" | "ai";

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
  kind: DagNodeKind;
  aiLive?: boolean;
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

    for (const produced of dagMetadata.produces ?? []) {
      if (!producerByPort.has(produced.port)) {
        producerByPort.set(produced.port, id);
        producerVersionByPort.set(produced.port, cell.version);
      }
    }

    for (const consumedPort of dagMetadata.consumes ?? []) {
      const consumers = consumersByPort.get(consumedPort) ?? new Set<string>();
      consumers.add(id);
      consumersByPort.set(consumedPort, consumers);
    }
  }

  return {
    nodes: dagCellIds.map((id) => {
      const cell = cells[id];
      const dagMetadata = cell.dagMetadata;
      const status = dagStatus[id];
      const stalePorts = staleConsumedPorts(status, portManifest);

      return {
        id,
        data: {
          id,
          kind: deriveNodeKind(cell),
          aiLive: deriveAiLive(dagMetadata),
          label: deriveLabel(dagMetadata, id),
          cellType: cell.type,
          code: cell.source,
          codePreview: firstSourceLine(cell.source),
          produces: (dagMetadata?.produces ?? []).map((port) => ({
            port: port.port,
            repr: port.repr,
            display: port.display,
            version: portManifest[port.port] ?? cell.version,
          })),
          consumes: (dagMetadata?.consumes ?? []).map((port) => {
            const ranVersion = status?.ranPortVersions[port];
            const stale =
              status?.state === "stale" || stalePorts.includes(port);
            return {
              port,
              version: portManifest[port] ?? producerVersionByPort.get(port),
              ...(ranVersion !== undefined ? { ranVersion } : {}),
              ...(stale ? { stale } : {}),
            };
          }),
          source: formatSource(dagMetadata?.source),
          state: deriveNodeState(status, portManifest),
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

function deriveNodeState(
  status: NodeStatus | undefined,
  portManifest: DagPortManifest,
): DagNodeState {
  if (!status) return "never-run";
  if (
    status.state === "fresh" &&
    staleConsumedPorts(status, portManifest).length > 0
  ) {
    return "stale";
  }
  return status.state;
}

function deriveNodeKind(cell: NotebookCellState): DagNodeKind {
  const kernelspec = cell.cellMetadataOther?.kernelspec;
  if (isRecord(kernelspec) && kernelspec.name === "spur") {
    return "ai";
  }
  return "code";
}

function deriveAiLive(metadata: NotebookCellState["dagMetadata"]): boolean {
  if (!metadata) return false;
  const metadataWithAi = metadata as NotebookCellState["dagMetadata"] & {
    ai_live?: unknown;
    aiLive?: unknown;
  };
  if (typeof metadataWithAi.ai_live === "boolean") {
    return metadataWithAi.ai_live;
  }
  if (typeof metadataWithAi.aiLive === "boolean") {
    return metadataWithAi.aiLive;
  }
  return false;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isDagCell(
  cell: NotebookCellState | undefined,
): cell is NotebookCellState {
  const dagMetadata = cell?.dagMetadata;
  return Boolean(
    dagMetadata &&
      ((dagMetadata.produces?.length ?? 0) > 0 ||
        (dagMetadata.consumes?.length ?? 0) > 0 ||
        dagMetadata.source),
  );
}

function firstSourceLine(source: string): string {
  return source.split(/\r?\n/, 1)[0]?.trim() || "(empty)";
}

// A node's human identity is the port it produces, not its opaque cell id.
// Fall back to the id for portless sinks so every node still has a title.
function deriveLabel(
  metadata: NotebookCellState["dagMetadata"],
  id: string,
): string {
  const produced = metadata?.produces?.[0];
  return produced ? (produced.display ?? produced.port) : id;
}

function formatSource(
  source: NonNullable<NotebookCellState["dagMetadata"]>["source"],
): string | undefined {
  return source ? `${source.kind}:${source.port}` : undefined;
}

function sortedPorts(consumersByPort: Map<string, Set<string>>): string[] {
  return Array.from(consumersByPort.keys()).sort();
}
