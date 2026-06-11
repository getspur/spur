import type { EditorView } from "@codemirror/view";
import { Channel, invoke } from "@tauri-apps/api/core";
import { WritableDraft } from "immer";
import { createContext, useContext } from "react";
import { v4 as uuidv4 } from "uuid";
import { StoreApi, create, createStore } from "zustand";
import { immer } from "zustand/middleware/immer";

import type {
  Cell,
  CellDagMetadata,
  CellMetadata,
  CodeType,
  CompilePhase,
  DaemonCell,
  FrontendCellMetadata,
  JuteDeckCellMetadata,
  NotebookDelta,
  NotebookMetadata,
  NotebookRoot,
  Output,
  OutputDisplayData,
  RunCellEvent,
} from "@/bindings";
import {
  daemonControl,
  snapshotFromDaemonControlResponse,
} from "@/daemon/control";

import {
  dispose as disposeWidgetModel,
  emit as emitWidgetModel,
  get as getWidgetModel,
  set as setWidgetModel,
} from "./widgetRegistry";

type NotebookStore = NotebookStoreState & NotebookStoreActions;

/** Actions are kept private, only to be used from the `Notebook` class. */
type NotebookStoreActions = {
  serverStateActions: ReturnType<typeof notebookServerStateActions>;
  viewStateActions: ReturnType<typeof notebookViewStateActions>;
  editBufferActions: ReturnType<typeof notebookEditBufferActions>;
  dagStateActions: ReturnType<typeof notebookDagStateActions>;
};

const INITIAL_CELL_VERSION = 1;
const AUTOSAVE_DEBOUNCE_MS = 5000;

function normalizeNotebookPath(path: string): string {
  return path.replace(/\/+$/, "");
}

/**
 * Whether an authoritative delta belongs to the notebook displayed by this
 * window. The daemon holds a single process-wide store but broadcasts every
 * delta to all windows; this guard prevents a mutation to one notebook from
 * leaking into the others. When either side has no path (unsaved/scratch
 * notebook, or a pre-`path` daemon build) we apply the delta for backward
 * compatibility.
 */
export function notebookDeltaIsForPath(
  notebookPath: string | undefined,
  deltaPath: string | null | undefined,
): boolean {
  if (!notebookPath || !deltaPath) {
    return true;
  }
  return (
    normalizeNotebookPath(notebookPath) === normalizeNotebookPath(deltaPath)
  );
}

type SupportedKernelSpecName = "deno" | "python3" | "evcxr" | "gonb";

function supportedKernelSpecName(name?: string): SupportedKernelSpecName {
  return name === "deno" ||
    name === "python3" ||
    name === "evcxr" ||
    name === "gonb"
    ? name
    : "python3";
}

function kernelSpecNameFromMetadata(
  metadata: NotebookMetadata,
): SupportedKernelSpecName {
  return supportedKernelSpecName(metadata.kernelspec?.name);
}

export type NotebookCellState = {
  type: CellType;
  initialText: string;
  source: string;
  version: number;
  lastEditedBy?: string;
  datasourceSetup?: boolean;
  dagMetadata?: CellDagMetadata;
  frontendMetadata?: CellFrontendMetadata;
  codeType?: CodeType;
  juteDeckMetadata?: JuteDeckCellMetadata;
  cellMetadataOther?: Record<string, unknown>;
  result?: CellResult;
};

export type CellFrontendMetadata = Record<string, unknown> & {
  binds?: string[];
  emits?: string[];
};

export type NotebookViewMode = "cells" | "dag" | "app";

export type NodeStatus = {
  state:
    | "fresh"
    | "stale"
    | "running"
    | "failed"
    | "upstream-failed"
    | "never-run";
  ranPortVersions: Record<string, number>;
  executionCount?: number;
};

export type DagPortManifest = Record<string, number>;

type DagStatusChangedDelta = {
  version: number;
  path?: string;
  kind: {
    type: "dagStatusChanged";
    snapshot: DagStatusSnapshot;
  };
};

type DagStatusSnapshot = {
  notebook_version?: number;
  nodes?: DagStatusSnapshotNode[];
  port_manifest?: DagPortManifest;
};

type DagStatusSnapshotNode = {
  id?: unknown;
  state?: unknown;
  execution_count?: unknown;
  executionCount?: unknown;
  ran_port_versions?: unknown;
  ranPortVersions?: unknown;
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

/** App-open information returned by the `notebook_open_mode` Tauri command. */
export type NotebookOpenInfo = {
  open_mode: string;
  app_name: string;
  /** Absolute path to the app root directory. */
  app_root: string;
  capabilities: {
    active_output_scripts: boolean;
    canvas_capture: boolean;
    artifacts_dir: boolean;
    ports?: unknown;
  };
  skill: string;
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

  /** Active in-place notebook view. */
  viewMode: NotebookViewMode;

  /**
   * App-mode open information populated when the notebook is an app entry
   * point. `undefined` for regular (non-app) notebooks.
   */
  appOpenInfo?: NotebookOpenInfo;
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

  /** DAG execution status keyed by cell ID. */
  dagStatus: Record<string, NodeStatus>;

  /** Current DAG port versions keyed by port name. */
  dagPortManifest: DagPortManifest;
};

export type CellType = "code" | "markdown";

export type CellResult = {
  status: "running" | "success" | "error";
  timings?: {
    startedAt: number;
    finishedAt?: number;
  };
  executionCount?: number;
  compile?: {
    phase: CompilePhase;
    current: string | null;
    startedAt: number;
  };
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

export type NotebookTabKernelState = "idle" | "live" | "running";

export type NotebookTab = {
  id: string;
  path?: string;
  title: string;
  dirty: boolean;
  kernelState: NotebookTabKernelState;
  language?: string;
  mode: NotebookViewMode;
  pinned?: boolean;
  attention?: boolean;
  kernelGeneration?: number;
};

export type ClosedTabRecord = { tab: NotebookTab; index: number };

type NotebookTabsStore = {
  tabs: NotebookTab[];
  activeTabId?: string;
  closedTabs: ClosedTabRecord[];
  setTabs: (tabs: NotebookTab[]) => void;
  setActiveTabId: (tabId: string | undefined) => void;
  updateTab: (tabId: string, patch: Partial<NotebookTab>) => void;
  setPinned: (tabId: string, pinned: boolean) => void;
  moveTab: (tabId: string, toIndex: number) => void;
  pushClosedTab: (record: ClosedTabRecord) => void;
  popClosedTab: () => ClosedTabRecord | undefined;
};

export const useNotebookTabsStore = create<NotebookTabsStore>((set, get) => ({
  tabs: [],
  activeTabId: undefined,
  closedTabs: [],
  setTabs: (tabs) =>
    set((state) => {
      const activeTabStillOpen =
        state.activeTabId !== undefined &&
        tabs.some((tab) => tab.id === state.activeTabId);
      return {
        tabs,
        activeTabId: activeTabStillOpen
          ? state.activeTabId
          : (tabs[0]?.id ?? undefined),
      };
    }),
  setActiveTabId: (tabId) =>
    set((state) => {
      if (tabId !== undefined && !state.tabs.some((tab) => tab.id === tabId)) {
        return state;
      }
      return {
        activeTabId: tabId,
        tabs: state.tabs.map((tab) =>
          tab.id === tabId && tab.attention
            ? { ...tab, attention: false }
            : tab,
        ),
      };
    }),
  updateTab: (tabId, patch) =>
    set((state) => ({
      tabs: state.tabs.map((tab) => {
        if (tab.id !== tabId) return tab;
        const next = { ...tab, ...patch, id: tab.id };
        const finishedInBackground =
          tab.kernelState === "running" &&
          next.kernelState !== "running" &&
          state.activeTabId !== tabId;
        if (finishedInBackground) next.attention = true;
        return next;
      }),
    })),
  setPinned: (tabId, pinned) =>
    set((state) => {
      const tab = state.tabs.find((candidate) => candidate.id === tabId);
      if (!tab || Boolean(tab.pinned) === pinned) return state;
      const rest = state.tabs.filter((candidate) => candidate.id !== tabId);
      const pinnedCount = rest.filter((candidate) => candidate.pinned).length;
      const next = [...rest];
      next.splice(pinnedCount, 0, { ...tab, pinned });
      return { tabs: next };
    }),
  moveTab: (tabId, toIndex) =>
    set((state) => {
      const from = state.tabs.findIndex((candidate) => candidate.id === tabId);
      if (from < 0) return state;
      const tab = state.tabs[from];
      const next = state.tabs.filter((candidate) => candidate.id !== tabId);
      const pinnedCount = next.filter((candidate) => candidate.pinned).length;
      const clamped = tab.pinned
        ? Math.min(Math.max(toIndex, 0), pinnedCount)
        : Math.min(Math.max(toIndex, pinnedCount), next.length);
      next.splice(clamped, 0, tab);
      return { tabs: next };
    }),
  pushClosedTab: (record) =>
    set((state) => ({ closedTabs: [...state.closedTabs.slice(-9), record] })),
  popClosedTab: () => {
    const stack = get().closedTabs;
    const top = stack[stack.length - 1];
    if (top) set({ closedTabs: stack.slice(0, -1) });
    return top;
  },
}));

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

type AuthoritativeNotebookDelta = NotebookDelta | DagStatusChangedDelta;

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
  delta: AuthoritativeNotebookDelta,
): boolean {
  return (
    delta.kind.type !== "loaded" &&
    delta.kind.type !== "dagStatusChanged" &&
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

    setViewMode: (viewMode: NotebookViewMode) =>
      set((state) => {
        state.viewState.viewMode = viewMode;
      }),

    setAppOpenInfo: (info: NotebookOpenInfo | undefined) =>
      set((state) => {
        state.viewState.appOpenInfo = info;
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

function notebookDagStateActions(
  set: (updater: (state: WritableDraft<NotebookStoreState>) => void) => void,
) {
  return {
    applyDagStatusSnapshot: (snapshot: DagStatusSnapshot) =>
      set((state) => {
        applyDagStatusSnapshotDraft(state, snapshot);
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
          viewMode: "cells",
        },
        editBuffer: {
          cellSources: {},
        },
        dagStatus: {},
        dagPortManifest: {},
      };
      const actions: NotebookStoreActions = {
        serverStateActions: notebookServerStateActions(set),
        viewStateActions: notebookViewStateActions(set),
        editBufferActions: notebookEditBufferActions(set),
        dagStateActions: notebookDagStateActions(set),
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
  compile?: {
    phase: CompilePhase;
    current: string | null;
    startedAt: number;
  };
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
  } else if (kind.type === "dagStatusChanged") {
    // DAG status lives outside the server-state slice and is applied by Notebook.
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

function applyDagStatusSnapshotDraft(
  state: WritableDraft<NotebookStoreState>,
  snapshot: DagStatusSnapshot,
) {
  const portManifest = normalizePortManifest(snapshot.port_manifest);
  if (portManifest) {
    state.dagPortManifest = portManifest;
  }

  for (const node of snapshot.nodes ?? []) {
    if (typeof node.id !== "string" || node.id.length === 0) continue;
    const nodeState = normalizeDagNodeState(node.state);
    if (!nodeState) continue;

    const existing = state.dagStatus[node.id];
    const executionCount = normalizeExecutionCount(node);
    const explicitRanPortVersions = normalizePortManifest(
      node.ran_port_versions ?? node.ranPortVersions,
    );
    const ranPortVersions =
      explicitRanPortVersions ??
      inferRanPortVersions(
        state.serverState.cells[node.id],
        state.dagPortManifest,
        nodeState,
        existing?.ranPortVersions,
      );

    state.dagStatus[node.id] = {
      state: nodeState,
      ranPortVersions,
      ...(executionCount !== undefined ? { executionCount } : {}),
    };
  }
}

function normalizePortManifest(value: unknown): DagPortManifest | undefined {
  if (!isRecord(value)) return undefined;
  return Object.fromEntries(
    Object.entries(value).filter(
      (entry): entry is [string, number] => typeof entry[1] === "number",
    ),
  );
}

function normalizeDagNodeState(
  value: unknown,
): NodeStatus["state"] | undefined {
  if (typeof value === "string") {
    return isNodeStatusState(value) ? value : undefined;
  }
  if (isRecord(value)) {
    return deriveSeedNodeState(normalizeOptionalNumber(value.execution_count));
  }
  return undefined;
}

function normalizeExecutionCount(
  node: DagStatusSnapshotNode,
): number | undefined {
  if (isRecord(node.state)) {
    return normalizeOptionalNumber(node.state.execution_count);
  }
  return normalizeOptionalNumber(node.execution_count ?? node.executionCount);
}

function inferRanPortVersions(
  cell: NotebookCellState | undefined,
  portManifest: DagPortManifest,
  state: NodeStatus["state"],
  existing: Record<string, number> | undefined,
): Record<string, number> {
  if (state !== "fresh" && state !== "running") {
    return existing ?? {};
  }
  return Object.fromEntries(
    (cell?.dagMetadata?.consumes ?? [])
      .map((port) => [port, portManifest[port]] as const)
      .filter((entry): entry is readonly [string, number] => {
        return entry[1] !== undefined;
      }),
  );
}

function deriveSeedNodeState(
  executionCount: number | undefined,
): NodeStatus["state"] {
  return executionCount && executionCount > 0 ? "fresh" : "never-run";
}

function isNodeStatusState(value: string): value is NodeStatus["state"] {
  return (
    value === "fresh" ||
    value === "stale" ||
    value === "running" ||
    value === "failed" ||
    value === "upstream-failed" ||
    value === "never-run"
  );
}

function normalizeOptionalNumber(value: unknown): number | undefined {
  return typeof value === "number" ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function normalizeStringList(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) return undefined;

  const strings = Array.from(
    new Set(
      value.filter(
        (item): item is string => typeof item === "string" && item.length > 0,
      ),
    ),
  );
  return strings.length > 0 ? strings : undefined;
}

function normalizeFrontendMetadata(
  value: unknown,
): CellFrontendMetadata | undefined {
  if (!isRecord(value)) return undefined;

  const metadata: CellFrontendMetadata = { ...value };
  const binds = normalizeStringList(value.binds);
  const emits = normalizeStringList(value.emits);
  if (binds) {
    metadata.binds = binds;
  } else {
    delete metadata.binds;
  }
  if (emits) {
    metadata.emits = emits;
  } else {
    delete metadata.emits;
  }

  return Object.keys(metadata).length > 0 ? metadata : undefined;
}

function frontendCellMetadataForExport(
  value: CellFrontendMetadata | undefined,
): FrontendCellMetadata | undefined {
  const metadata = normalizeFrontendMetadata(value);
  if (!metadata) return undefined;

  return {
    kind: typeof metadata.kind === "string" ? metadata.kind : undefined,
    binds: metadata.binds ?? [],
    emits: metadata.emits ?? [],
  };
}

function frontendMetadataFromSpur(
  spur: CellMetadata["spur"] | undefined,
): CellFrontendMetadata | undefined {
  return normalizeFrontendMetadata(
    (spur as (Record<string, unknown> & { frontend?: unknown }) | undefined)
      ?.frontend,
  );
}

function frontendMetadataFromDaemonCell(
  cell: DaemonCell,
): CellFrontendMetadata | undefined {
  const typed = normalizeFrontendMetadata(cell.frontendMetadata);
  if (typed) return typed;

  const metadataOther = (
    cell as DaemonCell & { metadataOther?: Record<string, unknown> }
  ).metadataOther;
  const spur = metadataOther?.spur;
  if (!isRecord(spur)) return undefined;
  return normalizeFrontendMetadata(spur.frontend);
}

function daemonCellMetadataOther(
  cell: DaemonCell,
): Record<string, unknown> | undefined {
  return (cell as DaemonCell & { metadataOther?: Record<string, unknown> })
    .metadataOther;
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
      const { spur, jute_deck, ...cellMetadataOther } = cell.metadata;
      const imported: NotebookCellState = {
        type: cell.cell_type,
        initialText: multiline(cell.source),
        source: multiline(cell.source),
        version: spur?.version ?? INITIAL_CELL_VERSION,
        lastEditedBy: spur?.last_edited_by,
        datasourceSetup: spur?.datasource_setup,
        dagMetadata: spur?.dag,
        frontendMetadata: frontendMetadataFromSpur(spur),
        codeType: spur?.code_type,
        juteDeckMetadata: jute_deck,
        cellMetadataOther:
          Object.keys(cellMetadataOther).length > 0
            ? cellMetadataOther
            : undefined,
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
    const frontendMetadata = frontendMetadataFromDaemonCell(cell);
    state.cells[cell.id] = {
      type,
      initialText: cell.source,
      source: cell.source,
      version: cell.version,
      lastEditedBy: cell.lastEditedBy ?? undefined,
      datasourceSetup: cell.datasourceSetup ?? undefined,
      dagMetadata: cell.dagMetadata,
      frontendMetadata,
      codeType: cell.codeType,
      juteDeckMetadata: cell.juteDeckMetadata,
      cellMetadataOther: daemonCellMetadataOther(cell),
    };
    return;
  }

  existing.type = type;
  existing.source = cell.source;
  existing.version = cell.version;
  existing.lastEditedBy = cell.lastEditedBy ?? undefined;
  existing.datasourceSetup = cell.datasourceSetup ?? undefined;
  existing.dagMetadata = cell.dagMetadata;
  existing.frontendMetadata =
    frontendMetadataFromDaemonCell(cell) ?? existing.frontendMetadata;
  existing.codeType = cell.codeType;
  existing.juteDeckMetadata = cell.juteDeckMetadata;
  existing.cellMetadataOther = daemonCellMetadataOther(cell);
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
    compile: runState.compile,
  });
}

function dismissesCompileProgress(message: RunCellEvent): boolean {
  return (
    message.event === "stdout" ||
    message.event === "stderr" ||
    message.event === "execute_result" ||
    message.event === "display_data" ||
    message.event === "error" ||
    message.event === "disconnect" ||
    message.event === "finished"
  );
}

type JsonRecord = Record<string, unknown>;
type BufferPathSegment = string | number;

function isJsonRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function cloneJsonValue(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => cloneJsonValue(item));
  }
  if (isJsonRecord(value)) {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, cloneJsonValue(item)]),
    );
  }
  return value;
}

function cloneJsonRecord(value: unknown): JsonRecord {
  if (!isJsonRecord(value)) return {};
  return cloneJsonValue(value) as JsonRecord;
}

function bufferPathsFrom(value: unknown): BufferPathSegment[][] {
  if (!Array.isArray(value)) return [];
  return value.filter(
    (path): path is BufferPathSegment[] =>
      Array.isArray(path) &&
      path.every(
        (segment) => typeof segment === "string" || typeof segment === "number",
      ),
  );
}

function assignBufferAtPath(
  state: JsonRecord,
  path: BufferPathSegment[],
  buffer: number[],
) {
  if (path.length === 0) return;

  let cursor: JsonRecord | unknown[] = state;
  for (let i = 0; i < path.length - 1; i += 1) {
    const segment = path[i];
    const nextSegment = path[i + 1];
    const key = String(segment);
    const nextValue: unknown = Array.isArray(cursor)
      ? cursor[Number(segment)]
      : cursor[key];

    if (isJsonRecord(nextValue) || Array.isArray(nextValue)) {
      cursor = nextValue;
      continue;
    }

    const replacement: JsonRecord | unknown[] =
      typeof nextSegment === "number" ? [] : {};
    if (Array.isArray(cursor)) {
      cursor[Number(segment)] = replacement;
    } else {
      cursor[key] = replacement;
    }
    cursor = replacement;
  }

  const lastSegment = path[path.length - 1];
  if (Array.isArray(cursor)) {
    cursor[Number(lastSegment)] = buffer.slice();
  } else {
    cursor[String(lastSegment)] = buffer.slice();
  }
}

function stateWithBuffers(data: JsonRecord, buffers: number[][]): JsonRecord {
  const state = cloneJsonRecord(data.state);
  const bufferPaths = bufferPathsFrom(data.buffer_paths);
  for (let i = 0; i < bufferPaths.length && i < buffers.length; i += 1) {
    assignBufferAtPath(state, bufferPaths[i], buffers[i]);
  }
  return state;
}

function splitWidgetAssets(
  state: JsonRecord,
  fallbackData?: JsonRecord,
): {
  state: JsonRecord;
  esm?: string;
  css?: string;
} {
  const nextState = { ...state };
  const stateEsm = nextState._esm;
  const stateCss = nextState._css;
  delete nextState._esm;
  delete nextState._css;

  return {
    state: nextState,
    esm:
      typeof stateEsm === "string"
        ? stateEsm
        : typeof fallbackData?._esm === "string"
          ? fallbackData._esm
          : undefined,
    css:
      typeof stateCss === "string"
        ? stateCss
        : typeof fallbackData?._css === "string"
          ? fallbackData._css
          : undefined,
  };
}

function widgetUpdateFromCommData(data: unknown, buffers: number[][]) {
  const record = isJsonRecord(data) ? data : {};
  return splitWidgetAssets(stateWithBuffers(record, buffers), record);
}

function applyCommOpen(message: Extract<RunCellEvent, { event: "comm_open" }>) {
  if (message.data.target_name !== "jupyter.widget") return;

  setWidgetModel(
    message.data.comm_id,
    widgetUpdateFromCommData(message.data.data, message.data.buffers),
  );
}

function applyCommMsg(message: Extract<RunCellEvent, { event: "comm_msg" }>) {
  const data = isJsonRecord(message.data.data) ? message.data.data : {};
  if (data.method === "update") {
    const update = widgetUpdateFromCommData(data, message.data.buffers);
    setWidgetModel(message.data.comm_id, {
      ...update,
      state: {
        ...(getWidgetModel(message.data.comm_id)?.state ?? {}),
        ...update.state,
      },
    });
  } else if (data.method === "custom") {
    emitWidgetModel(
      message.data.comm_id,
      "msg:custom",
      data.content,
      message.data.buffers,
    );
  }
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

  if (dismissesCompileProgress(message)) {
    delete nextRunState.compile;
  }

  if (message.event === "stdout" || message.event === "stderr") {
    updateRunCellResultDraft(state, cellId, nextRunState);
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
    updateRunCellResultDraft(state, cellId, nextRunState);
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
  } else if (message.event === "compile_progress") {
    nextRunState.compile = {
      phase: message.data.phase,
      current: message.data.current,
      startedAt: nextRunState.compile?.startedAt ?? Date.now(),
    };
    updateRunCellResultDraft(state, cellId, nextRunState);
  } else if (message.event === "comm_open") {
    applyCommOpen(message);
  } else if (message.event === "comm_msg") {
    applyCommMsg(message);
  } else if (message.event === "comm_close") {
    disposeWidgetModel(message.data.comm_id);
  } else if (message.event === "started") {
    nextRunState.status = "running";
    delete nextRunState.compile;
    updateRunCellResultDraft(state, cellId, nextRunState);
  } else if (message.event === "finished") {
    nextRunState.status = message.data.status === "ok" ? "success" : "error";
    nextRunState.executionCount =
      message.data.exec_count ?? nextRunState.executionCount;
    delete nextRunState.compile;
    if (options.finishedAt !== undefined) {
      nextRunState.timings = {
        ...nextRunState.timings,
        finishedAt: options.finishedAt,
      };
    }
    updateRunCellResultDraft(state, cellId, nextRunState);
  } else if (message.event === "disconnect") {
    if (options.handleDisconnect) {
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
    } else {
      updateRunCellResultDraft(state, cellId, nextRunState);
    }
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
  delta: AuthoritativeNotebookDelta,
) {
  if (!notebookDeltaIsForPath(notebook.state.viewState.path, delta.path)) {
    // Delta belongs to a different open notebook window; ignore it.
    return;
  }

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

  private kernelStartInFlight?: Promise<void>;

  private resolveInitialKernelStartPromise?: () => void;

  private rejectInitialKernelStartPromise?: (error: unknown) => void;

  constructor() {
    const store = createNotebookStore();
    this.store = store;
    this.refs = new Map();
    this.directRunCellStates = new Map();

    store.subscribe(() => this.scheduleAutosave());

    this.kernelStartPromise = new Promise((resolve, reject) => {
      this.resolveInitialKernelStartPromise = resolve;
      this.rejectInitialKernelStartPromise = reject;
    });
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
            cell.datasourceSetup,
            cell.dagMetadata,
            cell.frontendMetadata,
            cell.codeType,
            cell.juteDeckMetadata,
            cell.cellMetadataOther,
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
            cell.datasourceSetup,
            cell.dagMetadata,
            cell.frontendMetadata,
            cell.codeType,
            cell.juteDeckMetadata,
            cell.cellMetadataOther,
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

  applyDagStatusSnapshot(snapshot: DagStatusSnapshot) {
    this.state.dagStateActions.applyDagStatusSnapshot(snapshot);
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
      try {
        const openInfo = await invoke<NotebookOpenInfo | null>(
          "notebook_open_mode",
          { path },
        );
        if (openInfo?.open_mode === "app") {
          this.state.viewStateActions.setViewMode("app");
          this.state.viewStateActions.setAppOpenInfo(openInfo);
        } else {
          this.state.viewStateActions.setAppOpenInfo(undefined);
        }
      } catch {
        // If open-mode detection fails, keep the default view mode.
      }
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
      datasourceSetup: undefined,
      codeType: undefined,
    });
    return cellId;
  }

  insertCellAfter(
    afterId: string | undefined,
    type: CellType,
    initialText: string,
    lastEditedBy?: string,
    codeType?: CodeType,
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
        datasourceSetup: undefined,
        codeType,
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

  async setCellCodeType(cellId: string, codeType: CodeType) {
    const cell = selectCell(this.state, cellId);
    if (!cell || (cell.type === "code" && cell.codeType === codeType)) return;
    const expectedVersion = cell.version;
    this.applyLocalCellSnapshot(cellId, {
      ...cell,
      type: "code",
      codeType,
      version: expectedVersion + 1,
    });

    const response = await daemonControl({
      command: "set_cell_metadata",
      id: cellId,
      patch: { spur: { code_type: codeType } },
      expected_version: expectedVersion,
    });
    if (!response.ok) {
      throw new Error(
        response.error?.message ?? "Failed to update cell code type metadata",
      );
    }
    if (response.result?.type !== "delta") {
      throw new Error("daemon set_cell_metadata did not return a delta");
    }

    await reconcileNotebookDelta(this, response.result.data as NotebookDelta);
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
        cell.datasourceSetup,
        cell.dagMetadata,
        cell.frontendMetadata,
        cell.codeType,
        cell.juteDeckMetadata,
        cell.cellMetadataOther,
      ),
    };
  }

  mergeCellJuteDeckMetadata(
    cellId: string,
    patch: Partial<JuteDeckCellMetadata> & {
      spur?: {
        datasource_setup?: boolean;
        dag?: CellDagMetadata;
        frontend?: CellFrontendMetadata;
        code_type?: CodeType;
      };
    },
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
    const datasourceSetup =
      patch.spur?.datasource_setup ?? cell.datasourceSetup;
    const dagMetadata = patch.spur?.dag ?? cell.dagMetadata;
    const frontendMetadata =
      patch.spur && Object.hasOwn(patch.spur, "frontend")
        ? normalizeFrontendMetadata(patch.spur.frontend)
        : cell.frontendMetadata;
    const codeType = patch.spur?.code_type ?? cell.codeType;

    const nextVersion = cell.version + 1;
    this.applyLocalCellSnapshot(cellId, {
      ...cell,
      juteDeckMetadata: merged,
      datasourceSetup,
      dagMetadata,
      frontendMetadata,
      codeType,
      version: nextVersion,
      lastEditedBy: "brain",
    });
    return nextVersion;
  }

  applyNotebookDelta(delta: AuthoritativeNotebookDelta) {
    if (delta.kind.type === "dagStatusChanged") {
      this.applyDagStatusSnapshot(delta.kind.snapshot as DagStatusSnapshot);
      return;
    }

    const notebookDelta = delta as NotebookDelta;
    const kind = notebookDelta.kind;
    if (kind.type === "loaded") {
      this.state.serverStateActions.applyNotebookDelta(notebookDelta);
      this.refs = new Map(
        this.state.serverState.cellIds.map((cellId) => [cellId, {}]),
      );
      this.state.viewStateActions.finishLoading();
      this.state.editBufferActions.clearAll();
      void this.ensureKernelStarted();
    } else if (kind.type === "cellWritten") {
      this.upsertStoreCell(notebookDelta, kind.cell);
    } else if (kind.type === "cellInserted") {
      this.upsertStoreCell(notebookDelta, kind.cell);
    } else if (kind.type === "cellDeleted") {
      this.refs.delete(kind.id);
      this.state.serverStateActions.applyNotebookDelta(notebookDelta);
      this.state.viewStateActions.clearSelectedCellIfDeleted(kind.id);
      this.state.editBufferActions.clearCell(kind.id);
    } else if (kind.type === "runCellEvent") {
      this.handleRunCellEvent(kind.cell_id, kind.event, notebookDelta.version);
    } else if (kind.type === "dagStatusChanged") {
      this.applyDagStatusSnapshot(kind.snapshot as DagStatusSnapshot);
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
    await this.ensureKernelStarted();
    const kernelId = this.state.viewState.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    return this.setKernelSlotInfo(kernelId);
  }

  async restartKernel() {
    await this.ensureKernelStarted();
    const kernelId = this.state.viewState.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    const restartedKernelId = await invoke<string>("restart_kernel", {
      slotId: kernelId,
      specName: supportedKernelSpecName(this.state.viewState.kernelSpecName),
    });
    await this.setKernelSlotInfo(restartedKernelId);
  }

  async interruptKernel() {
    await this.ensureKernelStarted();
    const kernelId = this.state.viewState.kernelId;
    if (!kernelId) {
      throw new Error("Kernel has not started");
    }
    await invoke("interrupt_kernel", { kernelId });
  }

  async execute(cellId: string) {
    await this.ensureKernelStarted();

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
      const notebookPath = this.state.viewState.path;
      if (!notebookPath) {
        throw new Error("Notebook path is not available");
      }
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
        notebookPath,
        kernelId: this.state.viewState.kernelId,
        cellId,
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

  private ensureKernelStarted(): Promise<void> {
    if (this.state.viewState.kernelId) {
      return Promise.resolve();
    }
    if (this.kernelStartInFlight) {
      return this.kernelStartInFlight;
    }

    const specName = kernelSpecNameFromMetadata(
      this.state.serverState.notebookMetadata,
    );
    const promise = (async () => {
      const kernelId = await invoke<string>("start_kernel", { specName });
      await this.setKernelSlotInfo(kernelId);
    })();

    this.kernelStartInFlight = promise;
    this.kernelStartPromise = promise;
    promise.then(
      () => {
        if (this.kernelStartInFlight === promise) {
          this.kernelStartInFlight = undefined;
        }
        this.resolveInitialKernelStartPromise?.();
      },
      (error) => {
        if (this.kernelStartInFlight === promise) {
          this.kernelStartInFlight = undefined;
        }
        this.rejectInitialKernelStartPromise?.(error);
      },
    );

    return promise;
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
  datasourceSetup?: boolean,
  dagMetadata?: CellDagMetadata,
  frontendMetadata?: CellFrontendMetadata,
  codeType?: CodeType,
  juteDeckMetadata?: JuteDeckCellMetadata,
  other?: Record<string, unknown>,
): CellMetadata {
  const frontend = frontendCellMetadataForExport(frontendMetadata);
  const spur: CellMetadata["spur"] = {
    version,
    last_edited_by: lastEditedBy,
    datasource_setup: datasourceSetup,
    dag: dagMetadata,
    frontend,
    code_type: codeType,
  };
  const metadata: CellMetadata = {
    ...(other ?? {}),
    spur,
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
