import { useMemo } from "react";
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

  if (isLoading) {
    return (
      <div className="relative p-8 px-14">
        <div>Loading...</div>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-5xl px-8 py-6">
      <div
        role="status"
        aria-label="App mode status"
        className="mb-4 flex flex-wrap items-center gap-2 border-b border-gray-200 pb-2 text-xs text-gray-600"
      >
        <span className="font-medium text-gray-900">App</span>
        {statusItems.map((item) => (
          <span key={item}>{item}</span>
        ))}
      </div>

      <div className="flex flex-col">
        {frontendCellIds.map((cellId) => (
          <section
            key={cellId}
            role="region"
            aria-label={`Frontend cell ${cellId}`}
            className="border-b border-gray-200 py-4 last:border-b-0"
          >
            <CellOutput cellId={cellId} chromeless />
          </section>
        ))}
      </div>
    </div>
  );
}
