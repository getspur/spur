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
  DaemonNotebookSnapshot,
  JuteDeckCellMetadata,
  NotebookDelta,
  NotebookRoot,
  Output,
  OutputDisplayData,
  RunCellEvent,
} from "@/bindings";

type NotebookStore = NotebookStoreState & NotebookStoreActions;

/** Actions are kept private, only to be used from the `Notebook` class. */
type NotebookStoreActions = ReturnType<typeof notebookStoreActions>;

const INITIAL_CELL_VERSION = 1;
const AUTOSAVE_DEBOUNCE_MS = 5000;
const NOTEBOOK_IN_PROC_STORE_ENV = "VITE_SPUR_NOTEBOOK_IN_PROC_STORE";

/** Zustand reactive data used by the UI to render notebooks. */
export type NotebookStoreState = {
  /** A list of cell IDs in order. */
  cellIds: string[];

  /** Information about each cell, keyed by ID. */
  cells: {
    [cellId: string]: {
      type: CellType;
      initialText: string;
      source: string;
      version: number;
      lastEditedBy?: string;
      juteDeckMetadata?: JuteDeckCellMetadata;
      result?: CellResult;
    };
  };

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

function notebookStoreActions(
  // Updater used by Zustand / Immer to mutate the state.
  set: (updater: (state: WritableDraft<NotebookStoreState>) => void) => void,
) {
  return {
    /** Add a new cell to the notebook. */
    addCell: (
      cellId: string,
      type: CellType,
      initialText: string,
      lastEditedBy?: string,
    ) =>
      set((state) => {
        state.cellIds.push(cellId);
        state.cells[cellId] = {
          type,
          initialText,
          source: initialText,
          version: INITIAL_CELL_VERSION,
          lastEditedBy,
        };
      }),

    /** Insert a new cell after another cell, or at the end when omitted. */
    insertCellAfter: (
      cellId: string,
      afterId: string | undefined,
      type: CellType,
      initialText: string,
      lastEditedBy?: string,
    ) =>
      set((state) => {
        const insertAt = afterId
          ? state.cellIds.indexOf(afterId) + 1
          : state.cellIds.length;
        state.cellIds.splice(insertAt, 0, cellId);
        state.cells[cellId] = {
          type,
          initialText,
          source: initialText,
          version: INITIAL_CELL_VERSION,
          lastEditedBy,
        };
      }),

    /** Set the type of a cell. */
    setCellType: (cellId: string, type: CellType) =>
      set((state) => {
        if (state.cells[cellId].type === type) return;
        state.cells[cellId].type = type;
        state.cells[cellId].lastEditedBy = undefined;
        state.cells[cellId].version += 1;
      }),

    /** Set the currently focused cell. */
    setSelectedCell: (cellId: string) =>
      set((state) => {
        if (state.cells[cellId]) {
          state.selectedCellId = cellId;
        }
      }),

    /** Set the source text of a cell. */
    setCellSource: (cellId: string, source: string, lastEditedBy?: string) =>
      set((state) => {
        const cell = state.cells[cellId];
        if (cell.source !== source) {
          cell.source = source;
          cell.version += 1;
        }
        cell.lastEditedBy = lastEditedBy;
      }),

    /** Merge set-valued jute-deck metadata fields into a cell. */
    mergeCellJuteDeckMetadata: (
      cellId: string,
      patch: Partial<JuteDeckCellMetadata>,
      lastEditedBy?: string,
    ) => {
      let nextVersion = 0;
      set((state) => {
        const cell = state.cells[cellId];
        if (!cell) return;

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
        if (patch.background !== undefined)
          merged.background = patch.background;

        cell.juteDeckMetadata = merged;
        cell.version += 1;
        cell.lastEditedBy = lastEditedBy;
        nextVersion = cell.version;
      });
      return nextVersion;
    },

    /** Reconcile one cell from the authoritative Rust store. */
    applyCellSnapshot: (cell: DaemonCell, afterId?: string | null) =>
      set((state) => {
        const type = cellTypeFromDaemon(cell.kind);
        if (!type) return;

        const existing = state.cells[cell.id];
        if (!existing) {
          const afterIndex = afterId ? state.cellIds.indexOf(afterId) : -1;
          const insertAt =
            afterIndex >= 0 ? afterIndex + 1 : state.cellIds.length;
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
      }),

    /** Delete a cell from the notebook. */
    deleteCell: (cellId: string) =>
      set((state) => {
        state.cellIds = state.cellIds.filter((id) => id !== cellId);
        delete state.cells[cellId];
        if (state.selectedCellId === cellId) {
          state.selectedCellId = null;
        }
      }),

    /** Clear the result of a cell. */
    clearResult: (cellId: string) =>
      set((state) => {
        state.cells[cellId].result = undefined;
      }),

    /** Update properties of a cell result, except the actual `outputs` array. */
    updateResult: (cellId: string, result: CellResult) =>
      set((state) => {
        updateResultDraft(state, cellId, result);
      }),

    /** Append outputs to a cell. */
    appendOutput: (cellId: string, output: Output, displayId?: string) =>
      set((state) => {
        appendOutputDraft(state, cellId, output, displayId);
      }),

    /** Clear the output of a cell. */
    clearOutput: (cellId: string) =>
      set((state) => {
        clearOutputDraft(state, cellId);
      }),

    /** Update an existing `display_data` output. */
    updateOutputDisplay: (
      cellId: string,
      displayId: string,
      displayData: OutputDisplayData,
    ) =>
      set((state) => {
        updateOutputDisplayDraft(state, cellId, displayId, displayData);
      }),

    applyRunCellEvent: (
      cellId: string,
      event: RunCellEvent,
      runState: RunCellEventApplicationState,
      options?: ApplyRunCellEventOptions,
    ) => {
      let nextRunState = runState;
      set((state) => {
        nextRunState = applyRunCellEvent(
          state,
          cellId,
          event,
          runState,
          options,
        );
      });
      return nextRunState;
    },

    /**
     * Start loading the notebook from an external source.
     *
     * After this function is called, no new cell executions should happen until
     * the notebook finishes loading and one of the functions below is called. If
     * successful, this clears the current cells.
     */
    startLoading: () =>
      set((state) => {
        // TODO: Fix this to handle errors better.
        if (state.isLoading) throw new Error("Notebook is already loading");
        state.isLoading = true;
      }),

    /** Load the notebook from a JSON object. */
    loadNotebook: (notebook: NotebookRoot) =>
      set((state) => {
        // Filter out 'raw' cells, as they aren't supported yet.
        const cells = notebook.cells.filter(
          (cell) => cell.cell_type === "code" || cell.cell_type === "markdown",
        );

        // Some older notebooks have no cell IDs, so we generate them on import.
        const cellIds = cells.map((cell) => cell.id ?? uuidv4());

        state.cellIds = cellIds;
        state.cells = Object.fromEntries(
          cells.map((cell, i) => {
            const imported: NotebookStoreState["cells"][string] = {
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
        state.selectedCellId = null;
        state.isLoading = false;
        state.loadError = undefined;
      }),

    /** Set the error on failure to load a notebook. */
    setLoadError: (error: string) =>
      set((state) => {
        state.loadError = error;
        state.isLoading = false;
      }),

    /** Set the path of the notebook, when it is opened or saved. */
    setPath: (path: string) =>
      set((state) => {
        state.path = path;
      }),
  };
}

/** Initialize the Zustand store for a notebook and define mutators. */
function createNotebookStore(): StoreApi<NotebookStore> {
  // @ts-ignore TypeScript says that the instantiation is too deep, infinite?
  return createStore<NotebookStore>()(
    immer<NotebookStore>((set) => {
      const initialState: NotebookStoreState = {
        cellIds: [],
        cells: {},
        selectedCellId: null,
        isLoading: false,
      };
      const actions: NotebookStoreActions = notebookStoreActions(set);
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

function updateResultDraft(
  state: WritableDraft<NotebookStoreState>,
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
  state: WritableDraft<NotebookStoreState>,
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
  state: WritableDraft<NotebookStoreState>,
  cellId: string,
) {
  const obj = state.cells[cellId].result;
  if (obj) {
    obj.outputs = [];
    obj.displays = {};
  }
}

function updateOutputDisplayDraft(
  state: WritableDraft<NotebookStoreState>,
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
  state: WritableDraft<NotebookStoreState>,
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
  state: WritableDraft<NotebookStoreState>,
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

/** Webview-side mirror of the Rust `NotebookRuntimeConfig` Tauri command. */
export type NotebookRuntimeConfig = { inProcStore: boolean };

let runtimeConfigCache: NotebookRuntimeConfig | null = null;
let runtimeConfigPromise: Promise<NotebookRuntimeConfig> | null = null;

/**
 * Resolve the runtime config from the Rust process (single source of truth).
 *
 * Cached on first success. Falls back to the env-derived default — which now
 * matches the Rust default of in-proc-store enabled — when invoke is
 * unavailable (vitest, SSR, jute standalone shells without the command
 * registered).
 */
export async function loadNotebookRuntimeConfig(): Promise<NotebookRuntimeConfig> {
  if (runtimeConfigCache) return runtimeConfigCache;
  if (!runtimeConfigPromise) {
    runtimeConfigPromise = (async () => {
      try {
        const cfg = await invoke<NotebookRuntimeConfig>(
          "notebook_runtime_config",
        );
        runtimeConfigCache = cfg;
        return cfg;
      } catch {
        const cfg: NotebookRuntimeConfig = {
          inProcStore: envInProcStoreEnabled(),
        };
        runtimeConfigCache = cfg;
        return cfg;
      }
    })();
  }
  return runtimeConfigPromise;
}

function envInProcStoreEnabled(): boolean {
  const metaEnv = import.meta.env as Record<string, unknown>;
  const processEnv = typeof process === "undefined" ? undefined : process.env;
  const value =
    metaEnv[NOTEBOOK_IN_PROC_STORE_ENV] ??
    processEnv?.[NOTEBOOK_IN_PROC_STORE_ENV] ??
    processEnv?.SPUR_NOTEBOOK_IN_PROC_STORE;
  if (typeof value === "boolean") return value;
  if (typeof value !== "string") return true;
  return !["0", "false", "no", "off"].includes(value.toLowerCase());
}

function displayIdForRunCellEvent(message: RunCellEvent): string | undefined {
  if (message.event !== "display_data") return undefined;
  return message.data.transient?.display_id || uuidv4();
}

function cellTypeFromDaemon(kind: string): CellType | undefined {
  if (kind === "code" || kind === "markdown") return kind;
  return undefined;
}

export async function reconcileNotebookDelta(
  notebook: Notebook,
  delta: NotebookDelta,
) {
  const { inProcStore } = await loadNotebookRuntimeConfig();
  if (!inProcStore) return;
  await notebook.applyNotebookDelta(delta);
}

/** Test-only: drop the cached runtime config so the next call refetches. */
export function __resetNotebookRuntimeConfigCacheForTesting() {
  runtimeConfigCache = null;
  runtimeConfigPromise = null;
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

    for (const cellId of state.cellIds) {
      const cell = state.cells[cellId];
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
    this.state.loadNotebook(notebook);
    this.refs = new Map(this.state.cellIds.map((cellId) => [cellId, {}]));
  }

  /** Load a notebook from a file path. */
  async loadNotebookFromPath(path: string) {
    try {
      this.state.startLoading();
    } catch {
      return;
    }
    try {
      const notebook = await invoke<NotebookRoot>("get_notebook", { path });
      this.loadNotebook(notebook);
      this.state.setPath(path);
    } catch (e: any) {
      this.state.setLoadError(e.toString());
    }
  }

  addCell(type: CellType, initialText: string, lastEditedBy?: string): string {
    const cellId = uuidv4();
    this.refs.set(cellId, {});
    this.state.addCell(cellId, type, initialText, lastEditedBy);
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
    this.state.insertCellAfter(
      cellId,
      afterId,
      type,
      initialText,
      lastEditedBy,
    );
    return cellId;
  }

  deleteCell(cellId: string) {
    this.refs.delete(cellId);
    this.state.deleteCell(cellId);
  }

  setCellType(cellId: string, type: CellType) {
    this.state.setCellType(cellId, type);
  }

  setSelectedCell(cellId: string) {
    this.state.setSelectedCell(cellId);
  }

  updateCellSource(cellId: string, source: string, lastEditedBy?: string) {
    this.state.setCellSource(cellId, source, lastEditedBy);
  }

  getCellSnapshotById(cellId: string): { metadata: CellMetadata } | undefined {
    const cell = this.state.cells[cellId];
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
    return this.state.mergeCellJuteDeckMetadata(cellId, patch, "brain");
  }

  async applyNotebookDelta(delta: NotebookDelta) {
    const kind = delta.kind;
    if (kind.type === "loaded") {
      const snapshot = await invoke<DaemonNotebookSnapshot>(
        "notebook_store_snapshot",
      );
      this.loadNotebook(snapshot.root);
    } else if (kind.type === "cellWritten") {
      await this.reconcileStoreCell(kind.id);
    } else if (kind.type === "cellInserted") {
      await this.reconcileStoreCell(kind.id, kind.after_id);
    } else if (kind.type === "cellDeleted") {
      this.refs.delete(kind.id);
      this.state.deleteCell(kind.id);
    } else if (kind.type === "runCellEvent") {
      this.handleRunCellEvent(kind.cell_id, kind.event);
    }
  }

  clearResult(cellId: string) {
    this.state.clearResult(cellId);
  }

  async refreshKernelSlotInfo(): Promise<KernelSlotInfo> {
    if (!this.state.kernelId) {
      await this.kernelStartPromise;
    }
    const kernelId = this.state.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    return this.setKernelSlotInfo(kernelId);
  }

  async restartKernel() {
    if (!this.state.kernelId) {
      await this.kernelStartPromise;
    }
    const kernelId = this.state.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    const restartedKernelId = await invoke<string>("restart_kernel", {
      slotId: kernelId,
      specName: this.state.kernelSpecName ?? "python3",
    });
    await this.setKernelSlotInfo(restartedKernelId);
  }

  async interruptKernel() {
    if (!this.state.kernelId) {
      await this.kernelStartPromise;
    }
    const kernelId = this.state.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    await invoke("interrupt_kernel", { kernelId });
  }

  async execute(cellId: string) {
    if (!this.state.kernelId) {
      await this.kernelStartPromise;
    }

    const editor = this.refs.get(cellId)?.editor;
    if (!editor) {
      throw new Error(`Cell ${cellId} not found`);
    }
    const code = editor.state.doc.toString();
    const cell = this.state.cells[cellId];
    const lastEditedBy = cell?.source === code ? cell.lastEditedBy : undefined;
    this.updateCellSource(cellId, code, lastEditedBy);

    let runState: RunCellEventApplicationState = {
      status: "running",
      timings: { startedAt: Date.now() },
      executionCount: undefined,
      willClearOutput: false,
    };

    const update = () =>
      this.state.updateResult(cellId, {
        status: runState.status,
        timings: runState.timings,
        executionCount: runState.executionCount,
      });
    update();
    this.state.clearOutput(cellId);

    try {
      const onEvent = new Channel<RunCellEvent>();

      onEvent.onmessage = (message: RunCellEvent) => {
        runState = this.state.applyRunCellEvent(cellId, message, runState, {
          displayId: displayIdForRunCellEvent(message),
        });
        if (message.event === "disconnect") {
          console.warn("Skipping unhandled event", message);
        }
      };

      await invoke("run_cell", {
        kernelId: this.state.kernelId,
        code,
        onEvent,
      });
      if (runState.status === "running") {
        runState = { ...runState, status: "success" };
      }
    } catch (error: any) {
      runState = { ...runState, status: "error" };
      // Synthesize an error output for kernel disconnects or other errors.
      this.state.appendOutput(cellId, {
        output_type: "error",
        ename: "InternalError",
        evalue: error.toString(),
        traceback: [],
      });
    } finally {
      runState = {
        ...runState,
        timings: { ...runState.timings, finishedAt: Date.now() },
      };
      update();
    }
  }

  handleRunCellEvent(cellId: string, message: RunCellEvent) {
    if (!this.state.cells[cellId]) {
      this.directRunCellStates.delete(cellId);
      console.warn("Skipping run cell event for unknown cell", {
        cellId,
        message,
      });
      return;
    }

    const beginRun = () => {
      const runState: DirectRunCellState = {
        status: "running",
        timings: { startedAt: Date.now() },
        executionCount: undefined,
        willClearOutput: false,
      };
      this.directRunCellStates.set(cellId, runState);
      this.updateDirectRunResult(cellId, runState);
      this.state.clearOutput(cellId);
      return runState;
    };

    let runState = this.directRunCellStates.get(cellId);
    if (message.event === "started") {
      beginRun();
      return;
    }
    if (!runState) {
      runState = beginRun();
    }

    const isTerminal =
      message.event === "finished" || message.event === "disconnect";
    runState = this.state.applyRunCellEvent(cellId, message, runState, {
      displayId: displayIdForRunCellEvent(message),
      finishedAt: isTerminal ? Date.now() : undefined,
      handleDisconnect: true,
    });

    if (isTerminal) {
      this.directRunCellStates.delete(cellId);
    } else {
      this.directRunCellStates.set(cellId, runState);
    }
  }

  private updateDirectRunResult(cellId: string, runState: DirectRunCellState) {
    this.state.updateResult(cellId, {
      status: runState.status,
      timings: runState.timings,
      executionCount: runState.executionCount,
    });
  }

  private async reconcileStoreCell(cellId: string, afterId?: string | null) {
    const cell = await invoke<DaemonCell>("read_notebook_store_cell", {
      id: cellId,
    });
    if (!cellTypeFromDaemon(cell.kind)) {
      console.warn("Skipping unsupported notebook store cell kind", {
        cellId,
        kind: cell.kind,
      });
      return;
    }
    if (!this.refs.has(cell.id)) {
      this.refs.set(cell.id, {});
    }
    this.state.applyCellSnapshot(cell, afterId);
  }

  private scheduleAutosave() {
    if (this.autosaveTimer) {
      clearTimeout(this.autosaveTimer);
      this.autosaveTimer = undefined;
    }

    if (!this.state.path || this.state.isLoading) {
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
    const path = this.state.path;
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
    this.store.setState({
      kernelId: info.kernel_id,
      kernelSpecName: info.spec_name,
      kernelGeneration: info.generation,
    });
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
