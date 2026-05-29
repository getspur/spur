import { open } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  DatabaseIcon,
  FileUpIcon,
  PlusIcon,
} from "lucide-react";
import { DragEvent, useCallback, useMemo, useState } from "react";

import type { DatasourceEntry } from "@/bindings";
import {
  attachDatasourceCommand,
  daemonControl,
  datasourceEntryFromDaemonControlResponse,
} from "@/daemon/control";

type DroppedFile = File & {
  path?: string;
  webkitRelativePath?: string;
};

type DatasourceGroup = {
  key: string;
  label: string;
  entries: DatasourceEntry[];
};

const DATASOURCE_EXTENSIONS = [
  "csv",
  "parquet",
  "parq",
  "json",
  "jsonl",
  "ndjson",
];

export default function DatasourceSidebar() {
  const [collapsed, setCollapsed] = useState(false);
  const [entries, setEntries] = useState<DatasourceEntry[]>([]);
  const [group, setGroup] = useState("");
  const [dragActive, setDragActive] = useState(false);
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const groupedEntries = useMemo(
    () => groupDatasourceEntries(entries),
    [entries],
  );

  const attachPath = useCallback(
    async (path: string) => {
      const name = datasourceNameFromPath(path);
      setPendingPath(path);
      setError(null);

      try {
        const response = await daemonControl(
          attachDatasourceCommand({
            name,
            path,
            group: normalizeGroup(group),
          }),
        );
        const entry = datasourceEntryFromDaemonControlResponse(response);
        setEntries((current) => upsertDatasourceEntry(current, entry));
      } catch (caught) {
        setError(errorMessage(caught));
      } finally {
        setPendingPath(null);
      }
    },
    [group],
  );

  const handleAddDatasource = useCallback(async () => {
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Datasource", extensions: DATASOURCE_EXTENSIONS }],
    });

    const path = firstSelectedPath(selected);
    if (path) {
      await attachPath(path);
    }
  }, [attachPath]);

  const handleDrop = useCallback(
    (event: DragEvent<HTMLElement>) => {
      event.preventDefault();
      setDragActive(false);

      const path = firstDroppedPath(event);
      if (path) {
        void attachPath(path);
      } else {
        setError("Dropped file did not include a readable path.");
      }
    },
    [attachPath],
  );

  if (collapsed) {
    return (
      <aside
        className="flex h-full w-12 shrink-0 flex-col items-center border-l border-gray-200 bg-gray-50 pt-14 text-gray-500"
        data-tauri-drag-region
      >
        <button
          aria-label="Expand datasources"
          className="rounded p-1.5 transition-colors hover:bg-gray-200 hover:text-gray-950"
          onClick={() => setCollapsed(false)}
          type="button"
        >
          <ChevronLeftIcon size={18} strokeWidth={1.5} />
        </button>
        <DatabaseIcon className="mt-4" size={18} strokeWidth={1.5} />
      </aside>
    );
  }

  return (
    <aside className="flex h-full w-80 shrink-0 flex-col border-l border-gray-200 bg-gray-50 text-gray-700">
      <div className="flex h-full min-h-0 flex-col gap-3 px-3 pb-16 pt-14">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <DatabaseIcon className="shrink-0 text-gray-500" size={18} />
            <h2 className="truncate text-sm font-medium text-gray-950">
              Datasources
            </h2>
          </div>
          <button
            aria-label="Collapse datasources"
            className="rounded p-1.5 text-gray-500 transition-colors hover:bg-gray-200 hover:text-gray-950"
            onClick={() => setCollapsed(true)}
            type="button"
          >
            <ChevronRightIcon size={18} strokeWidth={1.5} />
          </button>
        </div>

        <div className="flex items-center gap-2">
          <label className="min-w-0 flex-1">
            <span className="sr-only">Group</span>
            <input
              aria-label="Group"
              className="h-8 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-gray-900"
              onChange={(event) => setGroup(event.currentTarget.value)}
              placeholder="Group"
              value={group}
            />
          </label>
          <button
            aria-label="Add datasource"
            className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded border border-gray-300 bg-white text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
            disabled={pendingPath !== null}
            onClick={() => void handleAddDatasource()}
            title="Add datasource"
            type="button"
          >
            <PlusIcon size={16} strokeWidth={1.5} />
          </button>
        </div>

        <section
          className={clsx(
            "flex min-h-24 shrink-0 items-center justify-center rounded border border-dashed px-3 py-4 text-center text-sm transition-colors",
            dragActive
              ? "border-gray-900 bg-white text-gray-950"
              : "border-gray-300 bg-gray-100 text-gray-500",
          )}
          data-testid="datasource-dropzone"
          onDragEnter={(event) => {
            event.preventDefault();
            setDragActive(true);
          }}
          onDragLeave={(event) => {
            event.preventDefault();
            setDragActive(false);
          }}
          onDragOver={(event) => {
            event.preventDefault();
          }}
          onDrop={handleDrop}
        >
          <div className="flex flex-col items-center gap-2">
            <FileUpIcon size={20} strokeWidth={1.5} />
            <span>{pendingPath ? "Attaching..." : "Drop datasource file"}</span>
          </div>
        </section>

        {error && (
          <div className="rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
            {error}
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto pr-1">
          {groupedEntries.length === 0 ? (
            <p className="px-1 py-2 text-sm text-gray-400">
              No datasources attached.
            </p>
          ) : (
            <div className="space-y-4">
              {groupedEntries.map((datasourceGroup) => (
                <section key={datasourceGroup.key}>
                  <h3 className="mb-2 truncate text-xs uppercase tracking-wide text-gray-400">
                    {datasourceGroup.label}
                  </h3>
                  <div className="space-y-2">
                    {datasourceGroup.entries.map((entry) => (
                      <DatasourceListItem entry={entry} key={entry.name} />
                    ))}
                  </div>
                </section>
              ))}
            </div>
          )}
        </div>
      </div>
    </aside>
  );
}

function DatasourceListItem({ entry }: { entry: DatasourceEntry }) {
  return (
    <article className="rounded border border-gray-200 bg-white p-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <h4 className="truncate text-sm font-medium text-gray-950">
            {entry.name}
          </h4>
          <p
            className="mt-0.5 truncate text-xs text-gray-400"
            title={entry.path}
          >
            {entry.path}
          </p>
        </div>
        <span className="shrink-0 rounded bg-gray-100 px-1.5 py-0.5 text-[10px] uppercase text-gray-500">
          {entry.kind}
        </span>
      </div>

      <div className="mt-3 space-y-1">
        {entry.columns.map((column) => (
          <div
            className="grid grid-cols-[minmax(0,1fr),auto] items-center gap-2 text-xs"
            key={column.name}
          >
            <span className="truncate text-gray-700">{column.name}</span>
            <span className="truncate text-gray-400">{column.sqlType}</span>
          </div>
        ))}
      </div>

      {entry.rowCount !== null && (
        <p className="mt-3 text-xs text-gray-400">
          {entry.rowCount.toLocaleString()} rows
        </p>
      )}
    </article>
  );
}

function groupDatasourceEntries(entries: DatasourceEntry[]): DatasourceGroup[] {
  const groups = new Map<string, DatasourceGroup>();

  for (const entry of entries) {
    const key = entry.group?.trim() || "__ungrouped__";
    const label = entry.group?.trim() || "Ungrouped";
    const group = groups.get(key) ?? { key, label, entries: [] };
    group.entries.push(entry);
    groups.set(key, group);
  }

  return [...groups.values()];
}

function upsertDatasourceEntry(
  entries: DatasourceEntry[],
  entry: DatasourceEntry,
): DatasourceEntry[] {
  const index = entries.findIndex((current) => current.name === entry.name);
  if (index === -1) return [...entries, entry];

  const next = [...entries];
  next[index] = entry;
  return next;
}

function datasourceNameFromPath(path: string): string {
  const fileName = path.split(/[\\/]/).pop() ?? path;
  const stem = fileName.replace(/\.[^.]+$/, "").trim();
  return stem || "datasource";
}

function normalizeGroup(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function firstSelectedPath(
  value: Awaited<ReturnType<typeof open>>,
): string | null {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value[0] ?? null;
  return null;
}

function firstDroppedPath(event: DragEvent<HTMLElement>): string | null {
  const [file] = Array.from(event.dataTransfer.files) as DroppedFile[];
  if (file?.path) return file.path;
  if (file?.webkitRelativePath) return file.webkitRelativePath;

  const textPath = event.dataTransfer.getData("text/plain").trim();
  return textPath.split(/\r?\n/).find(Boolean) ?? null;
}

function errorMessage(caught: unknown): string {
  if (caught instanceof Error) return caught.message;
  return String(caught);
}
