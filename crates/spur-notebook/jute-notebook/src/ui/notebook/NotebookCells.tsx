import clsx from "clsx";
import {
  BotIcon,
  CheckIcon,
  Code2Icon,
  LetterTextIcon,
  LucideIcon,
  PlusIcon,
  XIcon,
  XSquareIcon,
} from "lucide-react";
import {
  ReactNode,
  Suspense,
  lazy,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useStore } from "zustand";

import { type NotebookStoreState, useNotebook } from "@/stores/notebook";

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

const Aside = ({ children }: { children: ReactNode }) => (
  <aside className="absolute right-[-200px] w-[200px] px-2">{children}</aside>
);

const AsideIconButton = ({
  Icon,
  onClick,
}: {
  Icon: LucideIcon;
  onClick?: () => void;
}) => (
  <button
    className="rounded p-1 text-gray-500 transition-all hover:bg-gray-200 hover:text-black active:scale-110"
    onClick={onClick}
  >
    <Icon size={16} />
  </button>
);

function CellInputAside({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
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
    <Aside>
      <div className="mt-1 flex gap-0.5">
        {lastEditedBy && (
          <span
            aria-label={`Agent-edited cell. Last edited by ${lastEditedBy}`}
            className="inline-flex items-center rounded p-1 text-gray-500"
            title={`last-edited-by: ${lastEditedBy}`}
          >
            <BotIcon size={16} />
          </span>
        )}
        <AsideIconButton
          Icon={type === "code" ? Code2Icon : LetterTextIcon}
          onClick={() => {
            notebook.setCellType(cellId, type === "code" ? "markdown" : "code");
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
    </Aside>
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

function CellLanguageHeader({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
  const cell = useStore(notebook.store, (s) => s.serverState.cells[cellId]);
  const [menuOpen, setMenuOpen] = useState(false);
  const chipButtonRef = useRef<HTMLButtonElement>(null);
  if (!cell || cell.type !== "code") return null;
  const token = cellLanguageToken(cell);
  const languageId = cellLanguageId(cell);
  const isAi = languageId === "spur";
  const live = isAi && cellAiLive(cell);
  return (
    <div className="flex items-center gap-2 pl-[57px] pr-[18px] pt-3">
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

  if (!output) return null;
  return (
    <OutputView
      value={output}
      cellId={cellId}
      chromeless={chromeless}
      afmPortBindings={afmPortBindings}
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
          <CellInputAside cellId={id} />
          <CellLanguageHeader cellId={id} />
          <Suspense fallback={<CellInputFallback cellId={id} />}>
            <CellInput cellId={id} />
          </Suspense>

          {cells[id]?.result && (
            <>
              <hr className="border-gray-200" />
              <Aside>
                <div className="mt-1 flex gap-0.5">
                  <AsideIconButton
                    Icon={XSquareIcon}
                    onClick={() => notebook.clearResult(id)}
                  />
                </div>
              </Aside>
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
