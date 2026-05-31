import type { NotebookCellState } from "@/stores/notebook";

export type DagNodeState = "neutral";

export type DagProducedPort = {
  port: string;
  repr: string;
  display?: string;
  version: number;
};

export type DagConsumedPort = {
  port: string;
  version?: number;
};

export type DagNodeData = {
  id: string;
  label: string;
  cellType: NotebookCellState["type"];
  codePreview: string;
  produces: DagProducedPort[];
  consumes: DagConsumedPort[];
  source?: string;
  state: DagNodeState;
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
};

export type DagGraph = {
  nodes: DagGraphNode[];
  edges: DagGraphEdge[];
};

export function buildDagGraph(
  cellIds: string[],
  cells: Record<string, NotebookCellState>,
): DagGraph {
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
          codePreview: firstSourceLine(cell.source),
          produces:
            dagMetadata?.produces.map((port) => ({
              port: port.port,
              repr: port.repr,
              display: port.display,
              version: cell.version,
            })) ?? [],
          consumes:
            dagMetadata?.consumes.map((port) => ({
              port,
              version: producerVersionByPort.get(port),
            })) ?? [],
          source: formatSource(dagMetadata?.source),
          state: "neutral",
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
