import clsx from "clsx";
import {
  BotIcon,
  CheckIcon,
  ClockIcon,
  Code2Icon,
  LetterTextIcon,
  LucideIcon,
  PlayIcon,
  PlusIcon,
  Trash2Icon,
  XIcon,
  XSquareIcon,
} from "lucide-react";
import {
  Suspense,
  lazy,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useStore } from "zustand";

import { type NotebookStoreState, useNotebook } from "@/stores/notebook";

import { ScheduleSection } from "../dag/ScheduleSection";
import { scheduleLabel } from "../dag/scheduleApi";
import ConfirmModal from "../shared/ConfirmModal";
import CellInputFallback from "./CellInputFallback";
import CellLanguageMenu from "./CellLanguageMenu";
import type { AfmPortBindingSnapshot } from "./JuteAppOutput";
import OutputView from "./OutputView";
import { cellLanguageId, cellLanguageToken } from "./cellLanguage";
import {
  compilePhasePresentation,
  formatCompileElapsed,
} from "./compileProgress";

const CellInput = lazy(() => import("./CellInput"));

interface CellLanguageCell {
  codeType?: string;
  cellMetadataOther?: Record<string, unknown>;
}

function isAiCell(cell: CellLanguageCell): boolean {
  return cellLanguageId(cell) === "spur";
}

function cellAiLive(
  cell: CellLanguageCell & { dagMetadata?: unknown },
): boolean {
  if (!isAiCell(cell)) return false;
  const dag = cell.dagMetadata as
    | { ai_live?: boolean; aiLive?: boolean }
    | undefined;
  return Boolean(dag?.ai_live ?? dag?.aiLive);
}

const CellActionButton = ({
  label,
  Icon,
  onClick,
}: {
  label: string;
  Icon: LucideIcon;
  onClick?: () => void;
}) => (
  <button
    aria-label={label}
    className="inline-flex h-7 w-7 items-center justify-center rounded border border-transparent text-gray-500 transition-all hover:border-gray-200 hover:bg-gray-50 hover:text-black active:scale-105"
    onClick={onClick}
    title={label}
    type="button"
  >
    <Icon size={16} />
  </button>
);

function CellInputToolbar({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
  const [deleteConfirmationOpen, setDeleteConfirmationOpen] = useState(false);
  const type = useStore(
    notebook.store,
    (state) => state.serverState.cells[cellId].type,
  );
  const output = useStore(
    notebook.store,
    (state) => state.serverState.cells[cellId].result,
  );
  const lastEditedBy = useStore(notebook.store, (state) => {
    const sourceDraft = state.editBuffer.cellSources[cellId];
    return sourceDraft
      ? sourceDraft.lastEditedBy
      : state.serverState.cells[cellId].lastEditedBy;
  });
  const formatExecutionDuration = (durationMs: number) => {
    const seconds = durationMs / 1000;
    if (seconds < 1) {
      return `${durationMs} ms`;
    } else {
      return `${seconds.toFixed(2)} s`;
    }
  };

  return (
    <>
      <div className="flex min-h-8 items-center gap-1 pr-[18px] pt-2">
        <div className="flex items-center gap-1 rounded border border-gray-200 bg-white px-1 py-0.5 shadow-sm">
          {lastEditedBy && (
            <span
              aria-label={`Agent-edited cell. Last edited by ${lastEditedBy}`}
              className="inline-flex items-center rounded p-1 text-gray-500"
              title={`last-edited-by: ${lastEditedBy}`}
            >
              <BotIcon size={16} />
            </span>
          )}
          {type === "code" ? (
            <CellActionButton
              label="Run cell"
              Icon={PlayIcon}
              onClick={() => {
                void notebook.execute(cellId);
              }}
            />
          ) : null}
          <CellActionButton
            label={
              type === "code"
                ? "Convert cell to markdown"
                : "Convert cell to code"
            }
            Icon={type === "code" ? Code2Icon : LetterTextIcon}
            onClick={() => {
              notebook.setCellType(cellId, type === "code" ? "markdown" : "code");
            }}
          />
          <CellActionButton
            label="Insert cell below"
            Icon={PlusIcon}
            onClick={() => {
              notebook.insertCellAfter(cellId, "code", "");
            }}
          />
          <CellActionButton
            label="Delete cell"
            Icon={Trash2Icon}
            onClick={() => {
              setDeleteConfirmationOpen(true);
            }}
          />
        </div>
        {output?.timings?.finishedAt ? (
          <div className="mt-0.5 flex items-center">
            {output.status === "success" ? (
              <CheckIcon size={16} className="mr-1 text-green-500" />
            ) : (
              <XIcon size={16} className="mr-1 text-red-500" />
            )}
            <p className="text-sm text-gray-400">
              {formatExecutionDuration(
                output?.timings.finishedAt - output?.timings.startedAt,
              )}
            </p>
          </div>
        ) : null}
      </div>
      <ConfirmModal
        body="This will permanently delete the cell."
        confirmLabel="Delete"
        danger
        onCancel={() => setDeleteConfirmationOpen(false)}
        onConfirm={() => {
          setDeleteConfirmationOpen(false);
          notebook.deleteCell(cellId);
        }}
        open={deleteConfirmationOpen}
        title="Delete cell?"
      />
    </>
  );
}

function useCompileNow(active: boolean, startedAt: number | undefined) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!active || startedAt === undefined) {
      return;
    }

    setNow(Date.now());
    const intervalId = window.setInterval(() => {
      setNow(Date.now());
    }, 1000);

    return () => window.clearInterval(intervalId);
  }, [active, startedAt]);

  return now;
}

function CellExecutionMarker({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
  const cell = useStore(
    notebook.store,
    (state) => state.serverState.cells[cellId],
  );
  const compile = cell.result?.compile;
  const now = useCompileNow(Boolean(compile), compile?.startedAt);
  const markerClassName =
    "absolute left-0 top-4 z-10 flex w-[57px] justify-center font-mono text-[10.5px] leading-5";

  if (cell.type !== "code") {
    return null;
  }

  if (compile) {
    const presentation = compilePhasePresentation(compile.phase);
    const elapsed = formatCompileElapsed(compile.startedAt, now);

    return (
      <div
        role="status"
        aria-live="polite"
        aria-label={`Cell execution ${presentation.label} ${elapsed}`}
        className={clsx(markerClassName, presentation.gutterBadgeClassName)}
      >
        <span className="inline-flex items-center gap-1 rounded px-1 py-0.5">
          <span
            aria-hidden="true"
            className={clsx(
              "h-1.5 w-1.5 rounded-full motion-safe:animate-pulse",
              presentation.dotClassName,
            )}
          />
          <span>{elapsed}</span>
        </span>
      </div>
    );
  }

  const executionCount =
    cell.result?.status === "running"
      ? "*"
      : cell.result?.executionCount !== undefined
        ? String(cell.result.executionCount)
        : " ";

  const token = cellLanguageToken(cell);
  const ai = isAiCell(cell);
  return (
    <div
      aria-hidden="true"
      className={markerClassName}
      style={{ color: token.accent }}
    >
      {ai ? "✦" : ""}[{executionCount}]
    </div>
  );
}

/**
 * SQL-cell chrome: a pill signalling the shared kernel DuckDB session, and an
 * inline-editable output relation name. Naming the relation publishes a DAG
 * Arrow port; an empty name leaves the result as an anonymous preview.
 * Presentational only — the parent wires `onRelationChange` to the store.
 */
export function SqlCellHeader({
  relation,
  onRelationChange,
}: {
  relation: string;
  onRelationChange: (next: string) => void;
}) {
  return (
    <>
      <span
        className="inline-flex items-center gap-1 rounded-full border border-amber-200 bg-amber-50 px-1.5 py-px font-mono text-[9px] font-semibold text-amber-700"
        title="Reuses the kernel's shared DuckDB session (views and temp tables persist across SQL cells)"
      >
        <span aria-hidden="true">⛁</span> kernel session
      </span>
      <label className="inline-flex items-center gap-1 font-mono text-[10px] text-gray-500">
        <span aria-hidden="true">→</span>
        <span className="sr-only">relation</span>
        <input
          aria-label="relation"
          className="w-28 rounded bg-amber-100 px-1.5 py-px font-mono text-[10px] font-semibold text-amber-900 placeholder:font-normal placeholder:text-amber-700/50"
          onChange={(event) => onRelationChange(event.target.value)}
          placeholder="relation"
          spellCheck={false}
          value={relation}
        />
      </label>
    </>
  );
}

function CellLanguageHeader({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
  const cell = useStore(notebook.store, (s) => s.serverState.cells[cellId]);
  const [menuOpen, setMenuOpen] = useState(false);
  const [scheduleOpen, setScheduleOpen] = useState(false);
  const chipButtonRef = useRef<HTMLButtonElement>(null);
  if (!cell || cell.type !== "code") return null;
  const token = cellLanguageToken(cell);
  const languageId = cellLanguageId(cell);
  const isAi = languageId === "spur";
  const live = isAi && cellAiLive(cell);
  const armedSchedule = cell.schedule?.enabled ? cell.schedule : undefined;
  return (
    <div className="relative flex flex-1 items-center gap-2 pl-[57px] pt-3">
      <div className="relative inline-flex">
        <button
          ref={chipButtonRef}
          aria-expanded={menuOpen}
          aria-haspopup="menu"
          aria-label={`Change cell language: ${token.label}`}
          className="inline-flex items-center gap-1.5 rounded border px-1.5 py-px font-mono text-[10px] font-semibold transition-shadow hover:shadow-sm"
          onClick={() => setMenuOpen((open) => !open)}
          style={{
            color: token.chipText,
            background: token.chipBg,
            borderColor: token.chipBorder,
          }}
          type="button"
        >
          <span
            className="inline-flex h-[18px] w-[18px] items-center justify-center rounded text-[9px]"
            style={{ background: token.glyphBg }}
          >
            {token.glyph}
          </span>
          {token.label}
        </button>
        {menuOpen && (
          <CellLanguageMenu
            anchorRef={chipButtonRef}
            currentLanguageId={languageId}
            currentType={cell.type}
            onClose={() => setMenuOpen(false)}
            onSelectCodeType={(codeType) => {
              void notebook.setCellCodeType(cellId, codeType);
            }}
            onSelectType={(type) => {
              notebook.setCellType(cellId, type);
            }}
          />
        )}
      </div>
      {languageId === "sql" && (
        <SqlCellHeader
          relation={cell.dagMetadata?.produces?.[0]?.port ?? ""}
          onRelationChange={(next) => {
            void notebook.setCellProducedPort(cellId, next);
          }}
        />
      )}
      {isAi && (
        <span
          className={clsx(
            "rounded border px-1.5 py-px font-mono text-[9px]",
            live
              ? "border-violet-600 bg-violet-600 text-white"
              : "border-gray-300 bg-white text-gray-500",
          )}
        >
          {live ? "● LIVE" : "manual"}
        </span>
      )}
      {armedSchedule ? (
        <button
          type="button"
          aria-label="Edit schedule trigger"
          className="inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-1.5 py-px font-mono text-[9.5px] font-semibold text-violet-700 transition-colors hover:border-violet-300 hover:bg-violet-100"
          onClick={() => setScheduleOpen((open) => !open)}
        >
          <ClockIcon size={10} />
          {scheduleLabel(armedSchedule.cron)}
        </button>
      ) : (
        <button
          type="button"
          aria-label="Edit schedule trigger"
          className="inline-flex h-[22px] w-[22px] items-center justify-center rounded border border-gray-200 bg-white text-gray-400 transition-colors hover:border-gray-300 hover:bg-gray-50 hover:text-gray-700"
          onClick={() => setScheduleOpen((open) => !open)}
          title="Schedule trigger"
        >
          <ClockIcon size={12} />
        </button>
      )}
      {scheduleOpen ? (
        <div
          role="dialog"
          aria-label="Schedule trigger"
          className="absolute left-[57px] top-9 z-30 w-[320px] rounded border border-gray-200 bg-white p-3 text-left shadow-lg"
        >
          <div className="mb-2 flex items-center justify-between gap-3">
            <div className="text-sm font-semibold text-gray-950">
              Schedule trigger
            </div>
            <button
              type="button"
              aria-label="Close schedule trigger"
              className="inline-flex h-7 w-7 items-center justify-center rounded border border-gray-200 bg-white text-gray-500 transition-colors hover:border-gray-300 hover:bg-gray-50 hover:text-gray-700"
              onClick={() => setScheduleOpen(false)}
            >
              <XIcon size={14} />
            </button>
          </div>
          <ScheduleSection
            cellId={cellId}
            heading="Trigger"
            schedule={cell.schedule}
            variant="compact"
            version={cell.version ?? 0}
          />
        </div>
      ) : null}
    </div>
  );
}

export function CellOutput({
  cellId,
  chromeless = false,
}: {
  cellId: string;
  chromeless?: boolean;
}) {
  const notebook = useNotebook();
  const output = useStore(
    notebook.store,
    (state) => state.serverState.cells[cellId]?.result,
  );
  const afmPortBindingsKey = useStore(notebook.store, (state) =>
    selectAfmPortBindingsKey(state, cellId),
  );
  const afmPortBindings = useMemo(
    () => parseAfmPortBindingsKey(afmPortBindingsKey),
    [afmPortBindingsKey],
  );
  // Pass app root so HtmlOutput can resolve per-app trust grants.
  const appRoot = useStore(
    notebook.store,
    (state) => state.viewState.appOpenInfo?.app_root,
  );

  if (!output) return null;
  return (
    <OutputView
      value={output}
      cellId={cellId}
      chromeless={chromeless}
      afmPortBindings={afmPortBindings}
      appRoot={appRoot}
    />
  );
}

function selectAfmPortBindingsKey(
  state: NotebookStoreState,
  cellId: string,
): string | undefined {
  const bindings = selectAfmPortBindings(state, cellId);
  return bindings ? JSON.stringify(bindings) : undefined;
}

function parseAfmPortBindingsKey(
  key: string | undefined,
): AfmPortBindingSnapshot | undefined {
  if (!key) return undefined;
  return JSON.parse(key) as AfmPortBindingSnapshot;
}

function selectAfmPortBindings(
  state: NotebookStoreState,
  cellId: string,
): AfmPortBindingSnapshot | undefined {
  const cell = state.serverState.cells[cellId];
  const binds = cell?.frontendMetadata?.binds ?? [];
  if (binds.length === 0) return undefined;

  const status = state.dagStatus[cellId];
  const ports: AfmPortBindingSnapshot["ports"] = {};
  for (const port of binds) {
    const currentVersion = state.dagPortManifest[port];
    const ranVersion = status?.ranPortVersions[port];
    ports[port] = {
      ...(currentVersion !== undefined ? { currentVersion } : {}),
      ...(status?.executionCount !== undefined
        ? { executionCount: status.executionCount }
        : {}),
      ...(ranVersion !== undefined ? { ranVersion } : {}),
      ...(status?.state !== undefined ? { state: status.state } : {}),
    };
  }

  return { cellId, binds, ports };
}

export default function NotebookCells() {
  const notebook = useNotebook();
  const cellIds = useStore(
    notebook.store,
    (state) => state.serverState.cellIds,
  );
  const cells = useStore(notebook.store, (state) => state.serverState.cells);
  const isLoading = useStore(
    notebook.store,
    (state) => state.viewState.isLoading,
  );

  if (isLoading)
    // TODO: add a better loading state
    return (
      <div className="relative p-8 px-14">
        <div>Loading...</div>
      </div>
    );

  return (
    <div className="relative py-8">
      {cellIds.map((id) => (
        <div key={id} className="relative">
          <hr className="border-gray-200" />

          {cells[id]?.type === "code" && (
            <span
              aria-hidden="true"
              className="absolute bottom-4 left-0 top-4 w-[3px] rounded"
              style={{ background: cellLanguageToken(cells[id]).accent }}
            />
          )}
          <CellExecutionMarker cellId={id} />
          <div className="flex items-start justify-between gap-3">
            <CellLanguageHeader cellId={id} />
            <CellInputToolbar cellId={id} />
          </div>
          <Suspense fallback={<CellInputFallback cellId={id} />}>
            <CellInput cellId={id} />
          </Suspense>

          {cells[id]?.result && (
            <>
              <hr className="border-gray-200" />
              <div className="flex justify-end px-[18px] pt-2">
                <div className="rounded border border-gray-200 bg-white px-1 py-0.5 shadow-sm">
                  <CellActionButton
                    label="Clear cell output"
                    Icon={XSquareIcon}
                    onClick={() => notebook.clearResult(id)}
                  />
                </div>
              </div>
              <div className="max-h-[680px] overflow-y-auto">
                {/* TODO: Move this icon into the output view itself. Also it should only be displayed
                  when the cell has a return value, and next to the return value. */}
                {/* <CornerDownRightIcon size={16} className="text-gray-400" /> */}
                <CellOutput cellId={id} />
              </div>
            </>
          )}
        </div>
      ))}

      <div className="mx-2 my-4">
        <button
          className="flex w-full items-center justify-center gap-1.5 rounded border border-gray-200 p-2 transition-colors hover:border-gray-300 hover:bg-gray-50"
          onClick={() => {
            notebook.addCell("code", "");
          }}
        >
          <PlusIcon size={18} />
          <span>New cell</span>
        </button>
      </div>
    </div>
  );
}
