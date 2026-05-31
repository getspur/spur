import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { type NotebookCellState, useNotebook } from "@/stores/notebook";

type DagCell = {
  id: string;
  cell: NotebookCellState;
};

function formatList(values: string[]): string {
  return values.length > 0 ? values.join(", ") : "None";
}

function formatProducedPorts(cell: NotebookCellState): string {
  const produces = cell.dagMetadata?.produces ?? [];
  if (produces.length === 0) return "None";
  return produces
    .map(
      (port) =>
        `${port.port} (${port.repr}${port.display ? `, ${port.display}` : ""})`,
    )
    .join(", ");
}

function formatSource(cell: NotebookCellState): string {
  const source = cell.dagMetadata?.source;
  return source ? `${source.kind}:${source.port}` : "None";
}

export default function DagView() {
  const notebook = useNotebook();
  const [cellIds, cells] = useStore(
    notebook.store,
    useShallow((state) => [state.serverState.cellIds, state.serverState.cells]),
  );
  const dagCells = cellIds
    .map((id): DagCell | undefined => {
      const cell = cells[id];
      if (!cell?.dagMetadata) return undefined;
      return { id, cell };
    })
    .filter((entry): entry is DagCell => entry !== undefined);

  if (dagCells.length === 0) {
    return (
      <section className="mx-auto mt-8 w-full max-w-4xl px-6 text-sm text-gray-500">
        No DAG cells found.
      </section>
    );
  }

  return (
    <section className="mx-auto mt-8 w-full max-w-4xl px-6">
      <div className="mb-3 text-xs font-medium uppercase text-gray-500">
        DAG cells
      </div>
      <div className="divide-y divide-gray-200 rounded border border-gray-200 bg-white">
        {dagCells.map(({ id, cell }) => (
          <article key={id} className="px-4 py-3">
            <div className="mb-2 flex items-center justify-between gap-3">
              <h2 className="truncate text-sm font-medium text-gray-900">
                {id}
              </h2>
              <span className="shrink-0 rounded bg-gray-100 px-2 py-0.5 text-xs text-gray-600">
                {cell.type}
              </span>
            </div>
            <dl className="grid gap-2 text-xs text-gray-600 sm:grid-cols-3">
              <div>
                <dt className="font-medium text-gray-900">Produces</dt>
                <dd className="mt-1 break-words">
                  {formatProducedPorts(cell)}
                </dd>
              </div>
              <div>
                <dt className="font-medium text-gray-900">Consumes</dt>
                <dd className="mt-1 break-words">
                  {formatList(cell.dagMetadata?.consumes ?? [])}
                </dd>
              </div>
              <div>
                <dt className="font-medium text-gray-900">Source</dt>
                <dd className="mt-1 break-words">{formatSource(cell)}</dd>
              </div>
            </dl>
          </article>
        ))}
      </div>
    </section>
  );
}
