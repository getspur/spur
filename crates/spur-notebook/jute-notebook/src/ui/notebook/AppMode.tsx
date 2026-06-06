import { useEffect, useMemo } from "react";
import { useStore } from "zustand";

import { useNotebook } from "@/stores/notebook";

import { CellOutput } from "./NotebookCells";

function pluralize(count: number, singular: string, plural: string): string {
  return count === 1 ? singular : plural;
}

export default function AppMode() {
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
  const dagStatus = useStore(notebook.store, (state) => state.dagStatus);
  const setViewMode = useStore(
    notebook.store,
    (state) => state.viewStateActions.setViewMode,
  );

  const frontendCellIds = useMemo(
    () => cellIds.filter((cellId) => Boolean(cells[cellId]?.frontendMetadata)),
    [cellIds, cells],
  );
  const runningCount = frontendCellIds.filter(
    (cellId) => dagStatus[cellId]?.state === "running",
  ).length;
  const failedCount = frontendCellIds.filter((cellId) => {
    const state = dagStatus[cellId]?.state;
    return state === "failed" || state === "upstream-failed";
  }).length;
  const staleCount = frontendCellIds.filter(
    (cellId) => dagStatus[cellId]?.state === "stale",
  ).length;
  const statusItems = [
    `${frontendCellIds.length} frontend ${pluralize(
      frontendCellIds.length,
      "cell",
      "cells",
    )}`,
    runningCount > 0 ? `${runningCount} running` : null,
    failedCount > 0 ? `${failedCount} failed` : null,
    staleCount > 0 ? `${staleCount} stale` : null,
  ].filter(Boolean);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setViewMode("cells");
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [setViewMode]);

  if (isLoading) {
    return (
      <div className="flex h-full w-full flex-col bg-white pt-16">
        <div className="flex min-h-0 flex-1 items-center justify-center text-sm text-gray-600">
          <div>Loading...</div>
        </div>
      </div>
    );
  }

  return (
    <div className="relative flex h-full min-h-0 w-full flex-col overflow-hidden bg-white pt-16">
      <div
        role="status"
        aria-label="App mode status"
        className="pointer-events-none absolute bottom-4 right-4 z-20 flex flex-wrap items-center gap-2 rounded bg-white/85 px-3 py-1.5 font-mono text-xs text-gray-600 shadow-sm ring-1 ring-gray-200 backdrop-blur"
      >
        <span className="font-medium text-gray-900">App</span>
        {statusItems.map((item) => (
          <span key={item}>{item}</span>
        ))}
        <span>Esc notebook</span>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {frontendCellIds.map((cellId) => (
          <section
            key={cellId}
            role="region"
            aria-label={`Frontend cell ${cellId}`}
            className="min-h-full w-full"
          >
            <CellOutput cellId={cellId} chromeless />
          </section>
        ))}
      </div>
    </div>
  );
}
