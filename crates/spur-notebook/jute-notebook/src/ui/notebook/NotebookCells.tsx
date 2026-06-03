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
import { ReactNode, Suspense, lazy, useEffect, useState } from "react";
import { useStore } from "zustand";

import { useNotebook } from "@/stores/notebook";

import CellInputFallback from "./CellInputFallback";
import OutputView from "./OutputView";
import {
  compilePhasePresentation,
  formatCompileElapsed,
} from "./compileProgress";

const CellInput = lazy(() => import("./CellInput"));

function isAiCell(cell: {
  cellMetadataOther?: Record<string, unknown>;
}): boolean {
  const ks = (
    cell.cellMetadataOther?.kernelspec as { name?: string } | undefined
  )?.name;
  return ks === "spur";
}

function cellAiLive(cell: { dagMetadata?: unknown }): boolean {
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

  const ai = isAiCell(cell);
  return (
    <div
      aria-hidden="true"
      className={clsx(
        markerClassName,
        ai ? "text-violet-600" : "text-gray-400",
      )}
    >
      {ai ? "✦" : ""}[{executionCount}]
    </div>
  );
}

function AiCellHeader({ cellId }: { cellId: string }) {
  const notebook = useNotebook();
  const cell = useStore(notebook.store, (s) => s.serverState.cells[cellId]);
  if (!cell || cell.type !== "code" || !isAiCell(cell)) return null;
  const live = cellAiLive(cell);
  return (
    <div className="flex items-center gap-2 pl-[57px] pr-[18px] pt-3">
      <span className="inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-1.5 py-px font-mono text-[10px] font-semibold text-violet-700">
        ✦ AI
      </span>
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
    </div>
  );
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

          <CellExecutionMarker cellId={id} />
          <CellInputAside cellId={id} />
          <AiCellHeader cellId={id} />
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
                <OutputView value={cells[id].result} />
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
