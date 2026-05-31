import type { NotebookDelta } from "@/bindings";
import { daemonControl } from "@/daemon/control";
import { type CellType, type Notebook, selectCell } from "@/stores/notebook";

import {
  type AgentBridgeRequest,
  type AgentCellStatus,
  type AgentDeleteCell,
  AgentHandlerError,
  type AgentInsertCell,
  type AgentReadCell,
  type AgentSetCellMetadata,
  type AgentSnapshotCell,
  type AgentWriteCell,
} from "./types";

// Atomic-handler invariant: handlers that perform version checks and mutations
// must keep check + mutation synchronous, with no await between them. The M5
// write-side handlers below enforce expected_version and mutate Zustand in the
// same tick.

export async function dispatchAgentRequest(
  notebook: Notebook | undefined,
  request: AgentBridgeRequest,
): Promise<unknown> {
  switch (request.method) {
    case "notebook.snapshot":
      return snapshot(requireNotebook(notebook));
    case "notebook.export":
      return requireNotebook(notebook).export();
    case "notebook.flush_pending":
      return flushPending(requireNotebook(notebook));
    case "notebook.read_cell":
      return readCell(requireNotebook(notebook), request.params);
    case "notebook.insert_cell":
      return insertCell(requireNotebook(notebook), request.params);
    case "notebook.write_cell":
      return writeCell(requireNotebook(notebook), request.params);
    case "notebook.delete_cell":
      return deleteCell(requireNotebook(notebook), request.params);
    case "notebook.set_cell_metadata":
      return setCellMetadata(requireNotebook(notebook), request.params);
    default:
      throw new AgentHandlerError(
        "unknown_method",
        `Unknown notebook agent method: ${(request as { method: string }).method}`,
      );
  }
}

function flushPending(notebook: Notebook) {
  const path = notebook.state.viewState.path;
  if (!path) {
    throw new AgentHandlerError(
      "notebook_not_open",
      "No notebook path is loaded",
    );
  }
  return { path, contents: notebook.export() };
}

function requireNotebook(notebook: Notebook | undefined): Notebook {
  if (
    !notebook ||
    notebook.state.viewState.isLoading ||
    notebook.state.viewState.loadError
  ) {
    throw new AgentHandlerError("notebook_not_open", "No notebook is loaded");
  }
  return notebook;
}

function snapshot(notebook: Notebook): AgentSnapshotCell[] {
  const state = notebook.state;
  return state.serverState.cellIds.map((id) => {
    const cell = selectCell(state, id);
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
    };
  });
}

function readCell(notebook: Notebook, params: unknown): AgentReadCell {
  const id = readCellId(params);
  const state = notebook.state;
  const cell = selectCell(state, id);
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

function insertCell(notebook: Notebook, params: unknown): AgentInsertCell {
  const kind = readKind(params, "notebook.insert_cell");
  const source = readStringParam(params, "source", "notebook.insert_cell");
  const afterId = readOptionalStringParam(params, "after_id");
  const lastEditedBy =
    readOptionalStringParam(params, "last_edited_by") ?? "brain";
  const state = notebook.state;
  if (afterId && !state.serverState.cells[afterId]) {
    throw new AgentHandlerError("cell_not_found", `Cell not found: ${afterId}`);
  }

  const id = notebook.insertCellAfter(afterId, kind, source, lastEditedBy);
  return { id, version: selectCell(notebook.state, id)?.version ?? 0 };
}

function writeCell(notebook: Notebook, params: unknown): AgentWriteCell {
  const id = readStringParam(params, "id", "notebook.write_cell");
  const source = readStringParam(params, "source", "notebook.write_cell");
  const expectedVersion = readExpectedVersion(params, "notebook.write_cell");
  const lastEditedBy =
    readOptionalStringParam(params, "last_edited_by") ?? "brain";
  const cell = selectCell(notebook.state, id);
  if (!cell) {
    throw new AgentHandlerError("cell_not_found", `Cell not found: ${id}`);
  }
  if (cell.version !== expectedVersion) {
    throw new AgentHandlerError(
      "stale_version",
      `Cell ${id} is at version ${cell.version}, not ${expectedVersion}`,
    );
  }

  notebook.updateCellSource(id, source, lastEditedBy);
  return { version: selectCell(notebook.state, id)?.version ?? 0 };
}

function deleteCell(notebook: Notebook, params: unknown): AgentDeleteCell {
  const id = readStringParam(params, "id", "notebook.delete_cell");
  const expectedVersion = readExpectedVersion(params, "notebook.delete_cell");
  const cell = selectCell(notebook.state, id);
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

async function setCellMetadata(
  notebook: Notebook,
  params: AgentSetCellMetadata,
): Promise<{ ok: true; version: number }> {
  const { id, patch, expected_version } = params;
  if (!id || typeof patch !== "object" || patch === null) {
    throw new AgentHandlerError(
      "invalid_params",
      "notebook.set_cell_metadata requires { id, patch, expected_version }",
    );
  }
  if (!Number.isInteger(expected_version) || expected_version < 1) {
    throw new AgentHandlerError(
      "invalid_params",
      "notebook.set_cell_metadata expected_version must be >= 1",
    );
  }

  const response = await daemonControl({
    command: "set_cell_metadata",
    id,
    patch,
    expected_version,
  });
  if (!response.ok) {
    throw new AgentHandlerError(
      response.error?.code ?? "metadata_update_failed",
      response.error?.message ?? "Failed to update cell metadata",
    );
  }
  if (response.result?.type !== "delta") {
    throw new AgentHandlerError(
      "metadata_update_failed",
      "notebook.set_cell_metadata did not return a notebook delta",
    );
  }

  const delta = response.result.data as NotebookDelta;
  if (delta.kind.type !== "cellWritten") {
    throw new AgentHandlerError(
      "metadata_update_failed",
      "notebook.set_cell_metadata did not update a cell",
    );
  }
  notebook.applyNotebookDelta(delta);
  return { ok: true, version: delta.version };
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

function readOptionalStringParam(
  params: unknown,
  key: string,
): string | undefined {
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
