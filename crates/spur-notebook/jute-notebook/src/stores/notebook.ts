import type { EditorView } from "@codemirror/view";
import { Channel, invoke } from "@tauri-apps/api/core";
import { WritableDraft } from "immer";
import { createContext, useContext } from "react";
import { v4 as uuidv4 } from "uuid";
import { StoreApi, createStore } from "zustand";
import { immer } from "zustand/middleware/immer";

import type {
  Cell,
  CellMetadata,
  DaemonCell,
  JuteDeckCellMetadata,
  NotebookMetadata,
  NotebookDelta,
  NotebookRoot,
  Output,
  OutputDisplayData,
  RunCellEvent,
} from "@/bindings";
import {
  daemonControl,
  snapshotFromDaemonControlResponse,
} from "@/daemon/control";

type NotebookStore = NotebookStoreState & NotebookStoreActions;

/** Actions are kept private, only to be used from the `Notebook` class. */
type NotebookStoreActions = {
  serverStateActions: ReturnType<typeof notebookServerStateActions>;
  viewStateActions: ReturnType<typeof notebookViewStateActions>;
  editBufferActions: ReturnType<typeof notebookEditBufferActions>;
};

const INITIAL_CELL_VERSION = 1;
const AUTOSAVE_DEBOUNCE_MS = 5000;
export type NotebookCellState = {
  type: CellType;
  initialText: string;
  source: string;
  version: number;
  lastEditedBy?: string;
  juteDeckMetadata?: JuteDeckCellMetadata;
  result?: CellResult;
};

export type NotebookServerState = {
  /** Last authoritative Rust store document version applied to this replica. */
  lastAppliedVersion: number;

  /** Root-level notebook metadata loaded from the notebook file. */
  notebookMetadata: NotebookMetadata;

  /** A list of cell IDs in order. */
  cellIds: string[];

  /** Information about each cell, keyed by ID. */
  cells: Record<string, NotebookCellState>;
};

export type NotebookViewState = {
  /** ID of the currently focused cell, when any. */
  selectedCellId: string | null;

  /** True when loading the notebook from disk. */
  isLoading: boolean;

  /** Error related to loading the notebook. */
  loadError?: string;

  /** Path to the notebook file, if saved to a file path. */
  path?: string;

  /** ID of the running kernel, populated after the kernel is started. */
  kernelId?: string;

  /** Spec name for the running kernel slot. */
  kernelSpecName?: string;

  /** In-memory generation of the running kernel slot. */
  kernelGeneration?: number;
};

export type NotebookEditBuffer = {
  /** Optimistic cell source/version overlays while editors are ahead of Rust. */
  cellSources: Record<
    string,
    { source: string; version: number; lastEditedBy?: string }
  >;
};

/** Zustand reactive data used by the UI to render notebooks. */
export type NotebookStoreState = {
  /** Replica of the Rust authoritative notebook store. */
  serverState: NotebookServerState;

  /** React/UI-only state that does not belong to the Rust store. */
  viewState: NotebookViewState;

  /** Optimistic local overlays not yet reflected in serverState. */
  editBuffer: NotebookEditBuffer;
};

export type CellType = "code" | "markdown";

export type CellResult = {
  status: "running" | "success" | "error";
  timings?: {
    startedAt: number;
    finishedAt?: number;
  };
  executionCount?: number;
  outputs?: Output[];
  displays?: Record<string, number>;
};

export type KernelSlotInfo = {
  kernel_id: string;
  spec_name: string;
  generation: number;
  status: string;
  cpu_pct: number;
  mem_mb: number;
};

type NotebookLocalDelta = {
  version: number;
  kind:
    | {
        type: "localCellSnapshot";
        cellId: string;
        cell: NotebookCellState;
        after_id?: string | null;
      }
    | {
        type: "localClearResult";
        cell_id: string;
      };
};

type NotebookStateDelta = NotebookDelta | NotebookLocalDelta;

function isLocalNotebookDelta(
  delta: NotebookStateDelta,
): delta is NotebookLocalDelta {
  const type = delta.kind.type;
  return type === "localCellSnapshot" || type === "localClearResult";
}

function shouldAdvanceAppliedVersion(delta: NotebookStateDelta): boolean {
  return !isLocalNotebookDelta(delta) && delta.version > 0;
}

function hasAuthoritativeVersionGap(
  state: NotebookServerState,
  delta: NotebookDelta,
): boolean {
  return (
    delta.kind.type !== "loaded" &&
    delta.version > 0 &&
    state.lastAppliedVersion > 0 &&
    delta.version > state.lastAppliedVersion + 1
  );
}

type RunCellEventDeltaApplication = {
  runState: RunCellEventApplicationState;
  options?: ApplyRunCellEventOptions;
};

function notebookServerStateActions(
  // Updater used by Zustand / Immer to mutate the state.
  set: (updater: (state: WritableDraft<NotebookStoreState>) => void) => void,
) {
  return {
    /** Apply the reducer that is allowed to mutate the Rust replica slice. */
    applyNotebookDelta: (
      delta: NotebookStateDelta,
      application?: RunCellEventDeltaApplication,
    ) => {
      let nextRunState: RunCellEventApplicationState | undefined;
      set((state) => {
        nextRunState = applyNotebookDeltaDraft(
          state.serverState,
          delta,
          application,
        );
      });
      return nextRunState;
    },
  };
}

function notebookViewStateActions(
  set: (updater: (state: WritableDraft<NotebookStoreState>) => void) => void,
) {
  return {
    /** Set the currently focused cell. */
    setSelectedCell: (cellId: string) =>
      set((state) => {
        if (state.serverState.cells[cellId]) {
          state.viewState.selectedCellId = cellId;
        }
      }),

    clearSelectedCellIfDeleted: (cellId: string) =>
      set((state) => {
        if (state.viewState.selectedCellId === cellId) {
          state.viewState.selectedCellId = null;
        }
      }),

    startLoading: () =>
      set((state) => {
        // TODO: Fix this to handle errors better.
        if (state.viewState.isLoading) {
          throw new Error("Notebook is already loading");
        }
        state.viewState.isLoading = true;
      }),

    finishLoading: () =>
      set((state) => {
        state.viewState.selectedCellId = null;
        state.viewState.isLoading = false;
        state.viewState.loadError = undefined;
      }),

    /** Set the error on failure to load a notebook. */
    setLoadError: (error: string) =>
      set((state) => {
        state.viewState.loadError = error;
        state.viewState.isLoading = false;
      }),

    /** Set the path of the notebook, when it is opened or saved. */
    setPath: (path: string) =>
      set((state) => {
        state.viewState.path = path;
      }),

    setKernelSlotInfo: (info: KernelSlotInfo) =>
      set((state) => {
        state.viewState.kernelId = info.kernel_id;
        state.viewState.kernelSpecName = info.spec_name;
        state.viewState.kernelGeneration = info.generation;
      }),
  };
}

function notebookEditBufferActions(
  set: (updater: (state: WritableDraft<NotebookStoreState>) => void) => void,
) {
  return {
    /** Set the optimistic source text of a cell. */
    setCellSource: (cellId: string, source: string, lastEditedBy?: string) =>
      set((state) => {
        const cell = selectCell(state, cellId);
        const serverCell = state.serverState.cells[cellId];
        if (!cell || !serverCell) return;

        const version =
          cell.source !== source ? cell.version + 1 : cell.version;
        if (
          source === serverCell.source &&
          version === serverCell.version &&
          lastEditedBy === serverCell.lastEditedBy
        ) {
          delete state.editBuffer.cellSources[cellId];
          return;
        }

        state.editBuffer.cellSources[cellId] = {
          source,
          version,
          lastEditedBy,
        };
      }),

    clearCell: (cellId: string) =>
      set((state) => {
        delete state.editBuffer.cellSources[cellId];
      }),

    clearAll: () =>
      set((state) => {
        state.editBuffer.cellSources = {};
      }),
  };
}

/** Initialize the Zustand store for a notebook and define mutators. */
function createNotebookStore(): StoreApi<NotebookStore> {
  // @ts-ignore TypeScript says that the instantiation is too deep, infinite?
  return createStore<NotebookStore>()(
    immer<NotebookStore>((set) => {
      const initialState: NotebookStoreState = {
        serverState: {
          lastAppliedVersion: 0,
          notebookMetadata: {},
          cellIds: [],
          cells: {},
        },
        viewState: {
          selectedCellId: null,
          isLoading: false,
        },
        editBuffer: {
          cellSources: {},
        },
      };
      const actions: NotebookStoreActions = {
        serverStateActions: notebookServerStateActions(set),
        viewStateActions: notebookViewStateActions(set),
        editBufferActions: notebookEditBufferActions(set),
      };
      return { ...initialState, ...actions };
    }),
  );
}

type CellHandle = {
  editor?: EditorView;
};

export type RunCellEventApplicationState = {
  status: CellResult["status"];
  timings: NonNullable<CellResult["timings"]>;
  executionCount: CellResult["executionCount"];
  willClearOutput: boolean;
};

type DirectRunCellState = RunCellEventApplicationState;

type ApplyRunCellEventOptions = {
  displayId?: string;
  finishedAt?: number;
  handleDisconnect?: boolean;
};

export function selectCell(
  state: NotebookStoreState,
  cellId: string,
): NotebookCellState | undefined {
  const cell = state.serverState.cells[cellId];
  if (!cell) return undefined;

  const sourceDraft = state.editBuffer.cellSources[cellId];
  if (!sourceDraft) return cell;

  return {
    ...cell,
    source: sourceDraft.source,
    version: sourceDraft.version,
    lastEditedBy: sourceDraft.lastEditedBy,
  };
}

function selectCells(
  state: NotebookStoreState,
): Record<string, NotebookCellState> {
  return Object.fromEntries(
    state.serverState.cellIds
      .map((cellId) => [cellId, selectCell(state, cellId)] as const)
      .filter(
        (entry): entry is readonly [string, NotebookCellState] =>
          entry[1] !== undefined,
      ),
  );
}

function applyNotebookDeltaDraft(
  state: WritableDraft<NotebookServerState>,
  delta: NotebookStateDelta,
  application?: RunCellEventDeltaApplication,
): RunCellEventApplicationState | undefined {
  const kind = delta.kind;
  let runCellApplication: RunCellEventApplicationState | undefined;
  if (kind.type === "loaded") {
    loadNotebookRootDraft(state, kind.root);
  } else if (kind.type === "cellWritten") {
    applyDaemonCellSnapshotDraft(state, kind.cell);
  } else if (kind.type === "cellInserted") {
    applyDaemonCellSnapshotDraft(state, kind.cell, kind.after_id);
  } else if (kind.type === "cellDeleted") {
    state.cellIds = state.cellIds.filter((id) => id !== kind.id);
    delete state.cells[kind.id];
  } else if (kind.type === "runCellEvent") {
    if (!application) return undefined;
    runCellApplication = applyRunCellEvent(
      state,
      kind.cell_id,
      kind.event,
      application.runState,
      application.options,
    );
  } else if (kind.type === "localCellSnapshot") {
    const afterIndex = kind.after_id
      ? state.cellIds.indexOf(kind.after_id)
      : -1;
    const insertAt = afterIndex >= 0 ? afterIndex + 1 : state.cellIds.length;
    if (!state.cellIds.includes(kind.cellId)) {
      state.cellIds.splice(insertAt, 0, kind.cellId);
    }
    state.cells[kind.cellId] = kind.cell;
  } else if (kind.type === "localClearResult") {
    const cell = state.cells[kind.cell_id];
    if (cell) {
      cell.result = undefined;
    }
  } else {
    assertNever(kind);
  }

  if (shouldAdvanceAppliedVersion(delta)) {
    state.lastAppliedVersion = Math.max(
      state.lastAppliedVersion,
      delta.version,
    );
  }

  return runCellApplication;
}

function loadNotebookRootDraft(
  state: WritableDraft<NotebookServerState>,
  notebook: NotebookRoot,
) {
  state.notebookMetadata = notebook.metadata;

  // Filter out 'raw' cells, as they aren't supported yet.
  const cells = notebook.cells.filter(
    (cell) => cell.cell_type === "code" || cell.cell_type === "markdown",
  );

  // Some older notebooks have no cell IDs, so we generate them on import.
  const cellIds = cells.map((cell) => cell.id ?? uuidv4());

  state.cellIds = cellIds;
  state.cells = Object.fromEntries(
    cells.map((cell, i) => {
      const imported: NotebookCellState = {
        type: cell.cell_type,
        initialText: multiline(cell.source),
        source: multiline(cell.source),
        version: cell.metadata.spur?.version ?? INITIAL_CELL_VERSION,
        lastEditedBy: cell.metadata.spur?.last_edited_by,
        juteDeckMetadata: cell.metadata.jute_deck,
      };

      if (cell.cell_type === "code") {
        if (cell.execution_count || cell.outputs.length > 0) {
          // Infer status based on the outputs of the cell.
          const status = cell.outputs.some(
            (output) => output.output_type === "error",
          )
            ? "error"
            : "success";
          imported.result = {
            status,
            outputs: cell.outputs,
          };
          if (cell.execution_count) {
            imported.result.executionCount = cell.execution_count;
          }
        }
      }

      return [cellIds[i], imported];
    }),
  );
}

function applyDaemonCellSnapshotDraft(
  state: WritableDraft<NotebookServerState>,
  cell: DaemonCell,
  afterId?: string | null,
) {
  const type = cellTypeFromDaemon(cell.kind);
  if (!type) return;

  const existing = state.cells[cell.id];
  if (!existing) {
    const afterIndex = afterId ? state.cellIds.indexOf(afterId) : -1;
    const insertAt = afterIndex >= 0 ? afterIndex + 1 : state.cellIds.length;
    if (!state.cellIds.includes(cell.id)) {
      state.cellIds.splice(insertAt, 0, cell.id);
    }
    state.cells[cell.id] = {
      type,
      initialText: cell.source,
      source: cell.source,
      version: cell.version,
      lastEditedBy: cell.lastEditedBy ?? undefined,
    };
    return;
  }

  existing.type = type;
  existing.source = cell.source;
  existing.version = cell.version;
  existing.lastEditedBy = cell.lastEditedBy ?? undefined;
}

function updateResultDraft(
  state: WritableDraft<NotebookServerState>,
  cellId: string,
  result: CellResult,
) {
  const obj = state.cells[cellId].result;
  if (obj) {
    for (const [key, value] of Object.entries(result)) {
      // @ts-ignore
      obj[key] = value;
    }
  } else {
    // @ts-ignore Type instantiation is excessively deep and possibly infinite.
    state.cells[cellId].result = result;
  }
}

function appendOutputDraft(
  state: WritableDraft<NotebookServerState>,
  cellId: string,
  output: Output,
  displayId?: string,
) {
  const obj = state.cells[cellId].result;
  if (obj) {
    if (displayId) {
      if (output.output_type !== "display_data") {
        throw new Error("displayId can only be used with display_data");
      }
      obj.displays ??= {};
      obj.displays[displayId] = obj.outputs?.length ?? 0;
    }

    obj.outputs = obj.outputs ?? [];
    if (obj.outputs.length > 0) {
      const lastOutput = obj.outputs[obj.outputs.length - 1];
      if (
        lastOutput.output_type === "stream" &&
        output.output_type === "stream" &&
        lastOutput.name === output.name
      ) {
        // Concatenate to the last stream output if on the same stream.
        lastOutput.text = [
          ...(typeof lastOutput.text === "string"
            ? [lastOutput.text]
            : lastOutput.text),
          ...(typeof output.text === "string" ? [output.text] : output.text),
        ];
        return;
      }
    }

    obj.outputs.push(output);
  }
}

function clearOutputDraft(
  state: WritableDraft<NotebookServerState>,
  cellId: string,
) {
  const obj = state.cells[cellId].result;
  if (obj) {
    obj.outputs = [];
    obj.displays = {};
  }
}

function updateOutputDisplayDraft(
  state: WritableDraft<NotebookServerState>,
  cellId: string,
  displayId: string,
  displayData: OutputDisplayData,
) {
  const obj = state.cells[cellId].result;
  if (obj) {
    const index = obj.displays?.[displayId];
    if (index !== undefined) {
      const output = obj.outputs?.[index];
      if (output && output.output_type === "display_data") {
        output.data = displayData.data;
        output.metadata = displayData.metadata;
      }
    }
  }
}

function updateRunCellResultDraft(
  state: WritableDraft<NotebookServerState>,
  cellId: string,
  runState: RunCellEventApplicationState,
) {
  updateResultDraft(state, cellId, {
    status: runState.status,
    timings: runState.timings,
    executionCount: runState.executionCount,
  });
}

export function applyRunCellEvent(
  state: WritableDraft<NotebookServerState>,
  cellId: string,
  message: RunCellEvent,
  runState: RunCellEventApplicationState,
  options: ApplyRunCellEventOptions = {},
): RunCellEventApplicationState {
  const nextRunState = { ...runState };

  if (nextRunState.willClearOutput) {
    clearOutputDraft(state, cellId);
    nextRunState.willClearOutput = false;
  }

  if (message.event === "stdout" || message.event === "stderr") {
    appendOutputDraft(state, cellId, {
      output_type: "stream",
      name: message.event,
      text: message.data,
    });
  } else if (message.event === "error") {
    nextRunState.status = "error";
    updateRunCellResultDraft(state, cellId, nextRunState);
    appendOutputDraft(state, cellId, {
      output_type: "error",
      ename: message.data.ename,
      evalue: message.data.evalue,
      traceback: message.data.traceback,
    });
  } else if (message.event === "execute_result") {
    // This means that there was a return value for the cell.
    nextRunState.executionCount = message.data.execution_count;
    updateRunCellResultDraft(state, cellId, nextRunState);
    appendOutputDraft(state, cellId, {
      output_type: "execute_result",
      execution_count: message.data.execution_count,
      data: message.data.data,
      metadata: message.data.metadata,
    });
  } else if (message.event === "display_data") {
    const displayId = message.data.transient?.display_id || options.displayId;
    appendOutputDraft(
      state,
      cellId,
      {
        output_type: "display_data",
        data: message.data.data,
        metadata: message.data.metadata,
      },
      displayId,
    );
  } else if (message.event === "update_display_data") {
    const displayId = message.data.transient?.display_id;
    if (displayId) {
      updateOutputDisplayDraft(state, cellId, displayId, {
        data: message.data.data,
        metadata: message.data.metadata,
      });
    }
  } else if (message.event === "clear_output") {
    if (message.data.wait) {
      nextRunState.willClearOutput = true;
    } else {
      clearOutputDraft(state, cellId);
    }
  } else if (message.event === "started") {
    nextRunState.status = "running";
    updateRunCellResultDraft(state, cellId, nextRunState);
  } else if (message.event === "finished") {
    nextRunState.status = message.data.status === "ok" ? "success" : "error";
    nextRunState.executionCount =
      message.data.exec_count ?? nextRunState.executionCount;
    if (options.finishedAt !== undefined) {
      nextRunState.timings = {
        ...nextRunState.timings,
        finishedAt: options.finishedAt,
      };
    }
    updateRunCellResultDraft(state, cellId, nextRunState);
  } else if (message.event === "disconnect" && options.handleDisconnect) {
    nextRunState.status = "error";
    if (options.finishedAt !== undefined) {
      nextRunState.timings = {
        ...nextRunState.timings,
        finishedAt: options.finishedAt,
      };
    }
    updateRunCellResultDraft(state, cellId, nextRunState);
    appendOutputDraft(state, cellId, {
      output_type: "error",
      ename: "InternalError",
      evalue: message.data,
      traceback: [],
    });
  }

  return nextRunState;
}

function displayIdForRunCellEvent(message: RunCellEvent): string | undefined {
  if (message.event !== "display_data") return undefined;
  return message.data.transient?.display_id || uuidv4();
}

function cellTypeFromDaemon(kind: string): CellType | undefined {
  if (kind === "code" || kind === "markdown") return kind;
  return undefined;
}

function assertNever(value: never): never {
  throw new Error(`Unhandled notebook delta kind: ${JSON.stringify(value)}`);
}

export async function reconcileNotebookDelta(
  notebook: Notebook,
  delta: NotebookDelta,
) {
  const lastAppliedVersion = notebook.state.serverState.lastAppliedVersion;
  if (hasAuthoritativeVersionGap(notebook.state.serverState, delta)) {
    console.warn(
      "Notebook delta version gap detected; requesting snapshot resync",
      {
        lastAppliedVersion,
        receivedVersion: delta.version,
        kind: delta.kind.type,
      },
    );
    await notebook.resyncFromSnapshot();
    return;
  }

  notebook.applyNotebookDelta(delta);
}

/**
 * Centralized stateful object representing a notebook.
 *
 * The Notebook class is responsible for communicating with a running Jupyter
 * kernel and handling edits to notebooks. It also manages the Zustand state
 * for rendering a notebook in the UI.
 *
 * Generally, all user actions will go through methods on this class, which may
 * dispatch to Zustand. The UI subscribes to Zustand for updates.
 */
export class Notebook {
  /** Promise that resolves when the kernel is started. */
  kernelStartPromise: Promise<void>;

  /** Zustand object used to reactively update DOM nodes. */
  store: StoreApi<NotebookStore>;

  /** Direct handles to editors and other HTML elements after render. */
  refs: Map<string, CellHandle>;

  private autosaveTimer?: ReturnType<typeof setTimeout>;

  private directRunCellStates: Map<string, DirectRunCellState>;

  constructor() {
    const store = createNotebookStore();
    this.store = store;
    this.refs = new Map();
    this.directRunCellStates = new Map();

    store.subscribe(() => this.scheduleAutosave());

    this.kernelStartPromise = (async () => {
      const kernelId = await invoke<string>("start_kernel", {
        specName: "python3",
      });
      await this.setKernelSlotInfo(kernelId);
    })();
  }

  /** Access the current value of the notebook store, non-reactively. */
  get state() {
    return this.store.getState();
  }

  /** Save this notebook as an nbformat JSON object. */
  export(): NotebookRoot {
    const cells: Cell[] = [];
    const state = this.state;
    const effectiveCells = selectCells(state);

    for (const cellId of state.serverState.cellIds) {
      const cell = effectiveCells[cellId];
      if (!cell) continue;
      if (cell.type === "code") {
        cells.push({
          cell_type: "code",
          id: cellId,
          source: cell.source,
          execution_count: cell.result?.executionCount ?? null,
          outputs: cell.result?.outputs ?? [],
          metadata: cellMetadata(
            cell.version,
            cell.lastEditedBy,
            cell.juteDeckMetadata,
          ),
        });
      } else if (cell.type === "markdown") {
        cells.push({
          cell_type: "markdown",
          id: cellId,
          source: cell.source,
          metadata: cellMetadata(
            cell.version,
            cell.lastEditedBy,
            cell.juteDeckMetadata,
          ),
        });
      } else {
        throw new Error(`Unknown cell type: ${cell.type}`);
      }
    }

    return {
      nbformat: 4,
      nbformat_minor: 5,
      metadata: {}, // TODO: Add metadata.
      cells,
    };
  }

  /** Load a notebook from a direct object. */
  loadNotebook(notebook: NotebookRoot) {
    this.applyNotebookDelta({
      version: 0,
      kind: { type: "loaded", root: notebook },
    });
  }

  /** Load a notebook from a file path. */
  async loadNotebookFromPath(path: string) {
    try {
      this.state.viewStateActions.startLoading();
    } catch {
      return;
    }
    try {
      const notebook = await invoke<NotebookRoot>("get_notebook", { path });
      this.loadNotebook(notebook);
      this.state.viewStateActions.setPath(path);
    } catch (e: any) {
      this.state.viewStateActions.setLoadError(e.toString());
    }
  }

  async resyncFromSnapshot() {
    const response = await daemonControl({ command: "snapshot" });
    const snapshot = snapshotFromDaemonControlResponse(response);
    this.applyNotebookDelta({
      version: snapshot.version,
      kind: { type: "loaded", root: snapshot.root },
    });
  }

  addCell(type: CellType, initialText: string, lastEditedBy?: string): string {
    const cellId = uuidv4();
    this.refs.set(cellId, {});
    this.applyLocalCellSnapshot(cellId, {
      type,
      initialText,
      source: initialText,
      version: INITIAL_CELL_VERSION,
      lastEditedBy,
    });
    return cellId;
  }

  insertCellAfter(
    afterId: string | undefined,
    type: CellType,
    initialText: string,
    lastEditedBy?: string,
  ): string {
    const cellId = uuidv4();
    this.refs.set(cellId, {});
    this.applyLocalCellSnapshot(
      cellId,
      {
        type,
        initialText,
        source: initialText,
        version: INITIAL_CELL_VERSION,
        lastEditedBy,
      },
      afterId,
    );
    return cellId;
  }

  deleteCell(cellId: string) {
    this.refs.delete(cellId);
    this.state.serverStateActions.applyNotebookDelta({
      version: 0,
      kind: { type: "cellDeleted", id: cellId },
    });
    this.state.viewStateActions.clearSelectedCellIfDeleted(cellId);
    this.state.editBufferActions.clearCell(cellId);
  }

  setCellType(cellId: string, type: CellType) {
    const cell = selectCell(this.state, cellId);
    if (!cell || cell.type === type) return;

    this.applyLocalCellSnapshot(cellId, {
      ...cell,
      type,
      lastEditedBy: undefined,
      version: cell.version + 1,
    });
  }

  setSelectedCell(cellId: string) {
    this.state.viewStateActions.setSelectedCell(cellId);
  }

  updateCellSource(cellId: string, source: string, lastEditedBy?: string) {
    this.state.editBufferActions.setCellSource(cellId, source, lastEditedBy);
  }

  getCellSnapshotById(cellId: string): { metadata: CellMetadata } | undefined {
    const cell = selectCell(this.state, cellId);
    if (!cell) return undefined;
    return {
      metadata: cellMetadata(
        cell.version,
        cell.lastEditedBy,
        cell.juteDeckMetadata,
      ),
    };
  }

  mergeCellJuteDeckMetadata(
    cellId: string,
    patch: Partial<JuteDeckCellMetadata>,
  ): number {
    const cell = selectCell(this.state, cellId);
    if (!cell) return 0;

    const merged: JuteDeckCellMetadata = {
      ...(cell.juteDeckMetadata ?? {}),
    };
    if (patch.layout !== undefined) merged.layout = patch.layout;
    if (patch.hidden !== undefined) merged.hidden = patch.hidden;
    if (patch.speaker_notes !== undefined) {
      merged.speaker_notes = patch.speaker_notes;
    }
    if (patch.theme_override !== undefined) {
      merged.theme_override = patch.theme_override;
    }
    if (patch.fragments !== undefined) merged.fragments = patch.fragments;
    if (patch.background !== undefined) merged.background = patch.background;

    const nextVersion = cell.version + 1;
    this.applyLocalCellSnapshot(cellId, {
      ...cell,
      juteDeckMetadata: merged,
      version: nextVersion,
      lastEditedBy: "brain",
    });
    return nextVersion;
  }

  applyNotebookDelta(delta: NotebookDelta) {
    const kind = delta.kind;
    if (kind.type === "loaded") {
      this.state.serverStateActions.applyNotebookDelta(delta);
      this.refs = new Map(
        this.state.serverState.cellIds.map((cellId) => [cellId, {}]),
      );
      this.state.viewStateActions.finishLoading();
      this.state.editBufferActions.clearAll();
    } else if (kind.type === "cellWritten") {
      this.upsertStoreCell(delta, kind.cell);
    } else if (kind.type === "cellInserted") {
      this.upsertStoreCell(delta, kind.cell);
    } else if (kind.type === "cellDeleted") {
      this.refs.delete(kind.id);
      this.state.serverStateActions.applyNotebookDelta(delta);
      this.state.viewStateActions.clearSelectedCellIfDeleted(kind.id);
      this.state.editBufferActions.clearCell(kind.id);
    } else if (kind.type === "runCellEvent") {
      this.handleRunCellEvent(kind.cell_id, kind.event, delta.version);
    } else {
      assertNever(kind);
    }
  }

  clearResult(cellId: string) {
    this.state.serverStateActions.applyNotebookDelta({
      version: 0,
      kind: { type: "localClearResult", cell_id: cellId },
    });
  }

  async refreshKernelSlotInfo(): Promise<KernelSlotInfo> {
    if (!this.state.viewState.kernelId) {
      await this.kernelStartPromise;
    }
    const kernelId = this.state.viewState.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    return this.setKernelSlotInfo(kernelId);
  }

  async restartKernel() {
    if (!this.state.viewState.kernelId) {
      await this.kernelStartPromise;
    }
    const kernelId = this.state.viewState.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    const restartedKernelId = await invoke<string>("restart_kernel", {
      slotId: kernelId,
      specName: this.state.viewState.kernelSpecName ?? "python3",
    });
    await this.setKernelSlotInfo(restartedKernelId);
  }

  async interruptKernel() {
    if (!this.state.viewState.kernelId) {
      await this.kernelStartPromise;
    }
    const kernelId = this.state.viewState.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    await invoke("interrupt_kernel", { kernelId });
  }

  async execute(cellId: string) {
    if (!this.state.viewState.kernelId) {
      await this.kernelStartPromise;
    }

    const editor = this.refs.get(cellId)?.editor;
    if (!editor) {
      throw new Error(`Cell ${cellId} not found`);
    }
    const code = editor.state.doc.toString();
    const cell = selectCell(this.state, cellId);
    const lastEditedBy = cell?.source === code ? cell.lastEditedBy : undefined;
    this.updateCellSource(cellId, code, lastEditedBy);

    let runState: RunCellEventApplicationState = {
      status: "running",
      timings: { startedAt: Date.now() },
      executionCount: undefined,
      willClearOutput: false,
    };

    runState = this.applyRunCellEventDelta(
      cellId,
      { event: "started" },
      runState,
    );
    runState = this.applyRunCellEventDelta(
      cellId,
      { event: "clear_output", data: { wait: false } },
      runState,
    );

    try {
      const onEvent = new Channel<RunCellEvent>();

      onEvent.onmessage = (message: RunCellEvent) => {
        runState = this.applyRunCellEventDelta(cellId, message, runState, {
          displayId: displayIdForRunCellEvent(message),
        });
        if (message.event === "disconnect") {
          console.warn("Skipping unhandled event", message);
        }
      };

      await invoke("run_cell", {
        kernelId: this.state.viewState.kernelId,
        code,
        onEvent,
      });
    } catch (error: any) {
      runState = this.applyRunCellEventDelta(
        cellId,
        { event: "disconnect", data: error.toString() },
        runState,
        {
          finishedAt: Date.now(),
          handleDisconnect: true,
        },
      );
    } finally {
      if (runState.status === "running") {
        runState = this.applyRunCellEventDelta(
          cellId,
          {
            event: "finished",
            data: {
              status: "ok",
              exec_count: runState.executionCount ?? null,
            },
          },
          runState,
          { finishedAt: Date.now() },
        );
      } else if (runState.timings.finishedAt === undefined) {
        runState = this.applyRunCellEventDelta(
          cellId,
          {
            event: "finished",
            data: {
              status: runState.status === "success" ? "ok" : "error",
              exec_count: runState.executionCount ?? null,
            },
          },
          runState,
          { finishedAt: Date.now() },
        );
      }
    }
  }

  handleRunCellEvent(
    cellId: string,
    message: RunCellEvent,
    documentVersion = 0,
  ) {
    if (!this.state.serverState.cells[cellId]) {
      this.directRunCellStates.delete(cellId);
      console.warn("Skipping run cell event for unknown cell", {
        cellId,
        message,
      });
      return;
    }

    const beginRun = (startedVersion = 0) => {
      const runState: DirectRunCellState = {
        status: "running",
        timings: { startedAt: Date.now() },
        executionCount: undefined,
        willClearOutput: false,
      };
      this.directRunCellStates.set(cellId, runState);
      let nextRunState = this.applyRunCellEventDelta(
        cellId,
        { event: "started" },
        runState,
        undefined,
        startedVersion,
      );
      nextRunState = this.applyRunCellEventDelta(
        cellId,
        { event: "clear_output", data: { wait: false } },
        nextRunState,
      );
      return nextRunState;
    };

    let runState = this.directRunCellStates.get(cellId);
    if (message.event === "started") {
      beginRun(documentVersion);
      return;
    }
    if (!runState) {
      runState = beginRun();
    }

    const isTerminal =
      message.event === "finished" || message.event === "disconnect";
    runState = this.applyRunCellEventDelta(
      cellId,
      message,
      runState,
      {
        displayId: displayIdForRunCellEvent(message),
        finishedAt: isTerminal ? Date.now() : undefined,
        handleDisconnect: true,
      },
      documentVersion,
    );

    if (isTerminal) {
      this.directRunCellStates.delete(cellId);
    } else {
      this.directRunCellStates.set(cellId, runState);
    }
  }

  private upsertStoreCell(delta: NotebookDelta, cell: DaemonCell) {
    if (!cellTypeFromDaemon(cell.kind)) {
      console.warn("Skipping unsupported notebook store cell kind", {
        cellId: cell.id,
        kind: cell.kind,
      });
      return;
    }
    if (!this.refs.has(cell.id)) {
      this.refs.set(cell.id, {});
    }
    this.state.serverStateActions.applyNotebookDelta(delta);
    this.state.editBufferActions.clearCell(cell.id);
  }

  private applyLocalCellSnapshot(
    cellId: string,
    cell: NotebookCellState,
    afterId?: string | null,
  ) {
    this.state.serverStateActions.applyNotebookDelta({
      version: cell.version,
      kind: {
        type: "localCellSnapshot",
        cellId,
        cell,
        after_id: afterId,
      },
    });
    this.state.editBufferActions.clearCell(cellId);
  }

  private applyRunCellEventDelta(
    cellId: string,
    event: RunCellEvent,
    runState: RunCellEventApplicationState,
    options?: ApplyRunCellEventOptions,
    documentVersion = 0,
  ): RunCellEventApplicationState {
    return (
      this.state.serverStateActions.applyNotebookDelta(
        {
          version: documentVersion,
          kind: { type: "runCellEvent", cell_id: cellId, event },
        },
        { runState, options },
      ) ?? runState
    );
  }

  private scheduleAutosave() {
    if (this.autosaveTimer) {
      clearTimeout(this.autosaveTimer);
      this.autosaveTimer = undefined;
    }

    if (!this.state.viewState.path || this.state.viewState.isLoading) {
      return;
    }

    this.autosaveTimer = setTimeout(() => {
      this.autosaveTimer = undefined;
      void this.saveToDisk();
    }, AUTOSAVE_DEBOUNCE_MS);
  }

  async saveNow() {
    if (this.autosaveTimer) {
      clearTimeout(this.autosaveTimer);
      this.autosaveTimer = undefined;
    }
    await this.saveToDisk();
  }

  private async saveToDisk() {
    const path = this.state.viewState.path;
    if (!path) return;

    try {
      await invoke("save_to_disk", {
        path,
        contents: this.export(),
      });
    } catch (error) {
      console.error("Failed to autosave notebook", error);
    }
  }

  private async setKernelSlotInfo(kernelId: string): Promise<KernelSlotInfo> {
    const info = await invoke<KernelSlotInfo>("kernel_slot_info", { kernelId });
    this.state.viewStateActions.setKernelSlotInfo(info);
    return info;
  }
}

/** Helper function to convert a maybe-multiline string to a string. */
function multiline(string: string | string[]): string {
  return typeof string === "string" ? string : string.join("");
}

function cellMetadata(
  version: number,
  lastEditedBy?: string,
  juteDeckMetadata?: JuteDeckCellMetadata,
): CellMetadata {
  const metadata: CellMetadata = {
    spur: { version, last_edited_by: lastEditedBy },
  };
  if (juteDeckMetadata !== undefined) {
    metadata.jute_deck = juteDeckMetadata;
  }
  return metadata;
}

export const NotebookContext = createContext<Notebook | undefined>(undefined);

export function useNotebook(): Notebook {
  const notebook = useContext(NotebookContext);
  if (!notebook) {
    throw new Error("useNotebook must be used within a NotebookContext");
  }
  return notebook;
}
