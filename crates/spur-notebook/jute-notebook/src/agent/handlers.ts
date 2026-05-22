import type { CellType, Notebook } from "@/stores/notebook";

import {
  AgentHandlerError,
  type AgentBridgeRequest,
  type AgentCellStatus,
  type AgentDeleteCell,
  type AgentInsertCell,
  type AgentKernelInfo,
  type AgentReadCell,
  type AgentSnapshotCell,
  type AgentWriteCell,
} from "./types";

// Atomic-handler invariant: handlers that perform version checks and mutations
// must keep check + mutation synchronous, with no await between them. The M5
// write-side handlers below enforce expected_version and mutate Zustand in the
// same tick; kernel_info/interrupt may await because they do not mutate cells.

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
    case "notebook.insert_cell":
      return insertCell(requireNotebook(notebook), request.params);
    case "notebook.write_cell":
      return writeCell(requireNotebook(notebook), request.params);
    case "notebook.delete_cell":
      return deleteCell(requireNotebook(notebook), request.params);
    case "notebook.interrupt":
      return interrupt(requireNotebook(notebook));
    case "notebook.save":
      return save(requireNotebook(notebook));
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

function insertCell(notebook: Notebook, params: unknown): AgentInsertCell {
  const kind = readKind(params, "notebook.insert_cell");
  const source = readStringParam(params, "source", "notebook.insert_cell");
  const afterId = readOptionalStringParam(params, "after_id");
  const state = notebook.state;
  if (afterId && !state.cells[afterId]) {
    throw new AgentHandlerError("cell_not_found", `Cell not found: ${afterId}`);
  }

  const id = notebook.insertCellAfter(afterId, kind, source);
  return { id, version: notebook.state.cells[id].version };
}

function writeCell(notebook: Notebook, params: unknown): AgentWriteCell {
  const id = readStringParam(params, "id", "notebook.write_cell");
  const source = readStringParam(params, "source", "notebook.write_cell");
  const expectedVersion = readExpectedVersion(params, "notebook.write_cell");
  const cell = notebook.state.cells[id];
  if (!cell) {
    throw new AgentHandlerError("cell_not_found", `Cell not found: ${id}`);
  }
  if (cell.version !== expectedVersion) {
    throw new AgentHandlerError(
      "stale_version",
      `Cell ${id} is at version ${cell.version}, not ${expectedVersion}`,
    );
  }

  notebook.updateCellSource(id, source);
  return { version: notebook.state.cells[id].version };
}

function deleteCell(notebook: Notebook, params: unknown): AgentDeleteCell {
  const id = readStringParam(params, "id", "notebook.delete_cell");
  const expectedVersion = readExpectedVersion(params, "notebook.delete_cell");
  const cell = notebook.state.cells[id];
  if (!cell) {
    throw new AgentHandlerError("cell_not_found", `Cell not found: ${id}`);
  }
  if (cell.version !== expectedVersion) {
    throw new AgentHandlerError(
      "stale_version",
      `Cell ${id} is at version ${cell.version}, not ${expectedVersion}`,
    );
  }

  notebook.deleteCell(id);
  return { deleted: true };
}

async function interrupt(notebook: Notebook): Promise<{ ok: true }> {
  await notebook.interruptKernel();
  return { ok: true };
}

async function save(notebook: Notebook): Promise<{ ok: true }> {
  await notebook.saveNow();
  return { ok: true };
}

function readCellId(params: unknown): string {
  return readStringParam(params, "id", "notebook.read_cell");
}

function readStringParam(params: unknown, key: string, method: string): string {
  if (typeof params === "object" && params !== null) {
    const value = (params as Record<string, unknown>)[key];
    if (typeof value === "string" && value.length > 0) {
      return value;
    }
  }
  throw new AgentHandlerError("invalid_params", `${method} requires ${key}`);
}

function readOptionalStringParam(params: unknown, key: string): string | undefined {
  if (typeof params !== "object" || params === null || !(key in params)) {
    return undefined;
  }
  const value = (params as Record<string, unknown>)[key];
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value === "string" && value.length > 0) {
    return value;
  }
  throw new AgentHandlerError("invalid_params", `${key} must be a string`);
}

function readExpectedVersion(params: unknown, method: string): number {
  if (
    typeof params === "object" &&
    params !== null &&
    "expected_version" in params &&
    typeof params.expected_version === "number" &&
    Number.isInteger(params.expected_version) &&
    params.expected_version >= 1
  ) {
    return params.expected_version;
  }
  throw new AgentHandlerError(
    "invalid_params",
    `${method} requires expected_version`,
  );
}

function readKind(params: unknown, method: string): CellType {
  const kind = readStringParam(params, "kind", method);
  if (kind === "code" || kind === "markdown") {
    return kind;
  }
  throw new AgentHandlerError("invalid_params", `${method} kind is invalid`);
}

function cellStatus(status: AgentCellStatus | undefined): AgentCellStatus {
  return status ?? "idle";
}
