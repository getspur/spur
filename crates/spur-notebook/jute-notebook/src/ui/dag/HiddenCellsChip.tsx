import { useMemo, useState } from "react";

import type { NotebookCellState } from "@/stores/notebook";

type HiddenCellsChipProps = {
  cellIds: string[];
  cells: Record<string, NotebookCellState>;
};

type HiddenCell = {
  id: string;
  kind: NotebookCellState["type"];
};

export default function HiddenCellsChip({
  cellIds,
  cells,
}: HiddenCellsChipProps) {
  const [open, setOpen] = useState(false);
  const hiddenCells = useMemo(
    () => cellIds.flatMap((id) => hiddenCell(id, cells[id])),
    [cellIds, cells],
  );

  if (hiddenCells.length === 0) return null;

  return (
    <div className="relative">
      <button
        aria-expanded={open}
        className="inline-flex items-center gap-2 rounded border border-amber-200 bg-amber-50 px-3 py-1.5 text-xs font-medium text-amber-900 transition-colors hover:border-amber-300 hover:bg-amber-100"
        onClick={() => setOpen((value) => !value)}
        type="button"
      >
        Hidden cells ({hiddenCells.length})
      </button>
      {open ? (
        <div className="absolute right-0 z-20 mt-2 w-64 rounded border border-gray-200 bg-white p-2 text-xs shadow-lg">
          <ul className="grid gap-1">
            {hiddenCells.map((cell) => (
              <li
                className="flex items-center justify-between gap-3 rounded px-2 py-1.5 text-gray-700"
                key={cell.id}
              >
                <span className="min-w-0 truncate font-medium">{cell.id}</span>
                <span className="shrink-0 rounded bg-gray-100 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-normal text-gray-600">
                  {cell.kind}
                </span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}

function hiddenCell(
  id: string,
  cell: NotebookCellState | undefined,
): HiddenCell[] {
  if (!cell || isDagCell(cell)) return [];
  return [{ id, kind: cell.type }];
}

function isDagCell(cell: NotebookCellState): boolean {
  const dagMetadata = cell.dagMetadata;
  return Boolean(
    dagMetadata &&
      (dagMetadata.produces.length > 0 ||
        dagMetadata.consumes.length > 0 ||
        dagMetadata.source),
  );
}
