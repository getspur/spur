import type { Notebook } from "@/stores/notebook";

import {
  AgentHandlerError,
  type AgentBridgeRequest,
  type AgentCellStatus,
  type AgentKernelInfo,
  type AgentReadCell,
  type AgentSnapshotCell,
} from "./types";

// Atomic-handler invariant: handlers that perform version checks and mutations
// must keep check + mutation synchronous, with no await between them. The M3
// read-side handlers below read Zustand synchronously; kernel_info only awaits
// after reading the current kernel slot ID and does not mutate cell state.

export async function dispatchAgentRequest(
  notebook: Notebook | undefined,
  request: AgentBridgeRequest,
): Promise<unknown> {
  switch (request.method) {
    case "notebook.snapshot":
      return snapshot(requireNotebook(notebook));
    case "notebook.read_cell":
      return readCell(requireNotebook(notebook), request.params);
    case "notebook.kernel_info":
      return kernelInfo(requireNotebook(notebook));
    default:
      throw new AgentHandlerError(
        "unknown_method",
        `Unknown notebook agent method: ${request.method}`,
      );
  }
}

function requireNotebook(notebook: Notebook | undefined): Notebook {
  if (!notebook || notebook.state.isLoading || notebook.state.loadError) {
    throw new AgentHandlerError("notebook_not_open", "No notebook is loaded");
  }
  return notebook;
}

function snapshot(notebook: Notebook): AgentSnapshotCell[] {
  const state = notebook.state;
  return state.cellIds.map((id) => {
    const cell = state.cells[id];
    return {
      id,
      kind: cell.type,
      version: cell.version,
      exec_count: cell.result?.executionCount ?? null,
      status: cellStatus(cell.result?.status),
      source: cell.source,
    };
  });
}

function readCell(notebook: Notebook, params: unknown): AgentReadCell {
  const id = readCellId(params);
  const state = notebook.state;
  const cell = state.cells[id];
  if (!cell) {
    throw new AgentHandlerError("cell_not_found", `Cell not found: ${id}`);
  }

  return {
    id,
    kind: cell.type,
    version: cell.version,
    exec_count: cell.result?.executionCount ?? null,
    status: cellStatus(cell.result?.status),
    source: cell.source,
    outputs: cell.result?.outputs ?? [],
  };
}

async function kernelInfo(notebook: Notebook): Promise<AgentKernelInfo> {
  return notebook.refreshKernelSlotInfo();
}

function readCellId(params: unknown): string {
  if (
    typeof params === "object" &&
    params !== null &&
    "id" in params &&
    typeof params.id === "string" &&
    params.id.length > 0
  ) {
    return params.id;
  }
  throw new AgentHandlerError(
    "invalid_params",
    "notebook.read_cell requires { id }",
  );
}

function cellStatus(status: AgentCellStatus | undefined): AgentCellStatus {
  return status ?? "idle";
}
