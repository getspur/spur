import { type Event as TauriEvent, listen } from "@tauri-apps/api/event";
import { type DragDropEvent, getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ChevronDownIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  DatabaseIcon,
  FileUpIcon,
  PlugIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import {
  type DragEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import type { ConnectionTemplate, DatasourceEntry } from "@/bindings";
import {
  attachDatasourceCommand,
  attachSavedConnectionCommand,
  attachedSavedConnectionFromDaemonControlResponse,
  daemonControl,
  datasourceEntriesFromDaemonControlResponse,
  datasourceEntriesFromEventPayload,
  datasourceEntryFromDaemonControlResponse,
  deleteSavedConnectionCommand,
  detachDatasourceCommand,
  listDatasourcesCommand,
  listSavedConnectionsCommand,
  savedConnectionsFromDaemonControlResponse,
} from "@/daemon/control";

import AddRestApiWizard from "./AddRestApiWizard";

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
  "duckdb",
  "db",
  "sqlite",
];

export default function DatasourceSidebar() {
  const [collapsed, setCollapsed] = useState(false);
  const [entries, setEntries] = useState<DatasourceEntry[]>([]);
  const [group, setGroup] = useState("");
  const [dragActive, setDragActive] = useState(false);
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [savedConnections, setSavedConnections] = useState<
    ConnectionTemplate[]
  >([]);
  const [expandedSavedConnection, setExpandedSavedConnection] = useState<
    string | null
  >(null);
  const [savedConnectionNotice, setSavedConnectionNotice] = useState<
    string | null
  >(null);
  const [apiModalOpen, setApiModalOpen] = useState(false);
  const [editingConnection, setEditingConnection] =
    useState<ConnectionTemplate | null>(null);
  const dropzoneRef = useRef<HTMLElement | null>(null);
  const entriesRef = useRef<DatasourceEntry[]>([]);

  const groupedEntries = useMemo(
    () => groupDatasourceEntries(entries),
    [entries],
  );

  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

  const attachPath = useCallback(
    async (path: string) => {
      const name = datasourceNameFromPath(path);
      const collidingEntry = entriesRef.current.find(
        (entry) => entry.name === name && entry.path !== path,
      );
      if (collidingEntry) {
        setError(
          `Datasource "${name}" is already attached from ${collidingEntry.path}. Remove it before attaching ${path}.`,
        );
        return;
      }

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

  const handleDetachDatasource = useCallback(async (name: string) => {
    setError(null);

    try {
      await daemonControl(detachDatasourceCommand({ name }));
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, []);

  const handleAttachSavedConnection = useCallback(async (name: string) => {
    setError(null);
    setSavedConnectionNotice(null);

    try {
      const response = await daemonControl(
        attachSavedConnectionCommand({ name }),
      );
      const { entry, missingEnvVars } =
        attachedSavedConnectionFromDaemonControlResponse(response);
      setEntries((current) => {
        const nextEntries = upsertDatasourceEntry(current, entry);
        entriesRef.current = nextEntries;
        return nextEntries;
      });
      if (missingEnvVars.length > 0) {
        setSavedConnectionNotice(
          `Open the Add REST API wizard to supply ${missingEnvVars.join(", ")}.`,
        );
      }
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, []);

  const handleDeleteSavedConnection = useCallback(async (name: string) => {
    setError(null);

    try {
      await daemonControl(deleteSavedConnectionCommand({ name }));
      setSavedConnections((current) =>
        current.filter((connection) => connection.name !== name),
      );
      setExpandedSavedConnection((current) =>
        current === name ? null : current,
      );
      setSavedConnectionNotice(null);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, []);

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

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void daemonControl(listDatasourcesCommand())
      .then((response) => {
        if (disposed) return;
        const nextEntries =
          datasourceEntriesFromDaemonControlResponse(response);
        entriesRef.current = nextEntries;
        setEntries(nextEntries);
      })
      .catch((caught) => {
        if (!disposed) {
          setError(errorMessage(caught));
        }
      });

    try {
      void listen("datasources://changed", (event) => {
        try {
          const nextEntries = datasourceEntriesFromEventPayload(event.payload);
          entriesRef.current = nextEntries;
          setEntries(nextEntries);
        } catch (caught) {
          setError(errorMessage(caught));
        }
      })
        .then((cleanup) => {
          if (disposed) {
            cleanup();
          } else {
            unlisten = cleanup;
          }
        })
        .catch(() => undefined);
    } catch {
      return undefined;
    }

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    const reloadSavedConnections = async () => {
      try {
        const response = await daemonControl(listSavedConnectionsCommand());
        if (disposed) return;
        setSavedConnections(
          savedConnectionsFromDaemonControlResponse(response),
        );
      } catch (caught) {
        if (!disposed) {
          setError(errorMessage(caught));
        }
      }
    };

    void reloadSavedConnections();

    try {
      void listen("connections://changed", () => {
        void reloadSavedConnections();
      })
        .then((cleanup) => {
          if (disposed) {
            cleanup();
          } else {
            unlisten = cleanup;
          }
        })
        .catch(() => undefined);
    } catch {
      return undefined;
    }

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    const handleDragDropEvent = (event: TauriEvent<DragDropEvent>) => {
      const payload = event.payload;

      if (payload.type === "leave") {
        setDragActive(false);
        return;
      }

      const insideDropzone = isPositionInsideElement(
        dropzoneRef.current,
        payload.position,
      );

      if (payload.type === "enter" || payload.type === "over") {
        setDragActive(insideDropzone);
        return;
      }

      setDragActive(false);
      if (!insideDropzone) return;

      const path = payload.paths[0];
      if (!path) return;

      if (!isDatasourcePath(path)) {
        setError("Unsupported datasource type");
        return;
      }

      void attachPath(path);
    };

    try {
      void getCurrentWebview()
        .onDragDropEvent(handleDragDropEvent)
        .then((cleanup) => {
          if (disposed) {
            cleanup();
          } else {
            unlisten = cleanup;
          }
        })
        .catch(() => undefined);
    } catch {
      return undefined;
    }

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [attachPath]);

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
    <>
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
            <button
              aria-label="Add API datasource"
              className="inline-flex h-8 shrink-0 items-center gap-1 rounded border border-gray-300 bg-white px-2 text-xs font-medium text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950"
              onClick={() => setApiModalOpen(true)}
              title="Add API datasource"
              type="button"
            >
              <PlugIcon size={14} strokeWidth={1.5} />
              API
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
            ref={dropzoneRef}
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
              <span>
                {pendingPath ? "Attaching..." : "Drop datasource file"}
              </span>
            </div>
          </section>

          {error && (
            <div className="rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
              {error}
            </div>
          )}

          <div className="min-h-0 flex-1 overflow-y-auto pr-1">
            <section>
              <h3 className="mb-2 truncate text-xs uppercase tracking-wide text-gray-400">
                In this notebook
              </h3>
              {groupedEntries.length === 0 ? (
                <p className="px-1 py-2 text-sm text-gray-400">
                  No datasources attached.
                </p>
              ) : (
                <div className="space-y-4">
                  {groupedEntries.map((datasourceGroup) => (
                    <section key={datasourceGroup.key}>
                      <h4 className="mb-2 truncate text-xs uppercase tracking-wide text-gray-400">
                        {datasourceGroup.label}
                      </h4>
                      <div className="space-y-2">
                        {datasourceGroup.entries.map((entry) => (
                          <DatasourceListItem
                            entry={entry}
                            key={entry.name}
                            onRemove={handleDetachDatasource}
                          />
                        ))}
                      </div>
                    </section>
                  ))}
                </div>
              )}
            </section>
          </div>

          <SavedConnectionsSection
            connections={savedConnections}
            expandedName={expandedSavedConnection}
            notice={savedConnectionNotice}
            onAttach={(name) => void handleAttachSavedConnection(name)}
            onDelete={(name) => void handleDeleteSavedConnection(name)}
            onEdit={(connection) => setEditingConnection(connection)}
            onToggle={(name) =>
              setExpandedSavedConnection((current) =>
                current === name ? null : name,
              )
            }
          />
        </div>
      </aside>
      <AddRestApiWizard
        editConnection={editingConnection}
        open={apiModalOpen || editingConnection !== null}
        onClose={() => {
          setApiModalOpen(false);
          setEditingConnection(null);
        }}
      />
    </>
  );
}

function SavedConnectionsSection({
  connections,
  expandedName,
  notice,
  onAttach,
  onDelete,
  onEdit,
  onToggle,
}: {
  connections: ConnectionTemplate[];
  expandedName: string | null;
  notice: string | null;
  onAttach: (name: string) => void;
  onDelete: (name: string) => void;
  onEdit: (connection: ConnectionTemplate) => void;
  onToggle: (name: string) => void;
}) {
  return (
    <section className="shrink-0 border-t border-gray-200 pt-3">
      <h3 className="mb-2 truncate text-xs uppercase tracking-wide text-gray-400">
        Saved connections
      </h3>
      {notice && (
        <p className="mb-2 rounded border border-amber-200 bg-amber-50 px-2 py-1.5 text-xs text-amber-800">
          {notice}
        </p>
      )}
      {connections.length === 0 ? (
        <p className="px-1 py-1 text-xs text-gray-400">No saved connections.</p>
      ) : (
        <div className="max-h-60 space-y-1 overflow-y-auto pr-1">
          {connections.map((connection) => (
            <SavedConnectionRow
              connection={connection}
              expanded={expandedName === connection.name}
              key={connection.name}
              onAttach={onAttach}
              onDelete={onDelete}
              onEdit={onEdit}
              onToggle={onToggle}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function SavedConnectionRow({
  connection,
  expanded,
  onAttach,
  onDelete,
  onEdit,
  onToggle,
}: {
  connection: ConnectionTemplate;
  expanded: boolean;
  onAttach: (name: string) => void;
  onDelete: (name: string) => void;
  onEdit: (connection: ConnectionTemplate) => void;
  onToggle: (name: string) => void;
}) {
  const credentialCount = connection.credentialEnvVars.length;
  const provider = connection.provider?.trim() || "custom";
  const tableFunctionLabel = `${connection.tables.length} ${
    connection.tables.length === 1 ? "table-function" : "table-functions"
  }`;

  return (
    <article className="rounded border border-gray-200 bg-white">
      <div className="flex items-center gap-1 p-1.5">
        <button
          aria-label={`${expanded ? "Collapse" : "Expand"} saved connection ${
            connection.name
          }`}
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded px-1 py-1 text-left text-sm text-gray-700 transition-colors hover:bg-gray-50 hover:text-gray-950"
          onClick={() => onToggle(connection.name)}
          type="button"
        >
          {expanded ? (
            <ChevronDownIcon className="shrink-0" size={14} strokeWidth={1.5} />
          ) : (
            <ChevronRightIcon
              className="shrink-0"
              size={14}
              strokeWidth={1.5}
            />
          )}
          <span
            aria-label={
              credentialCount > 0
                ? "Credentials required"
                : "No credentials required"
            }
            className={clsx(
              "h-2 w-2 shrink-0 rounded-full",
              credentialCount > 0 ? "bg-amber-400" : "bg-emerald-500",
            )}
            role="img"
          />
          <span className="truncate font-medium">{connection.name}</span>
        </button>
        <button
          aria-label={`Attach saved connection ${connection.name}`}
          className="shrink-0 rounded border border-gray-300 px-2 py-1 text-xs font-medium text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950"
          onClick={() => onAttach(connection.name)}
          type="button"
        >
          Attach
        </button>
      </div>

      {expanded && (
        <div className="space-y-2 border-t border-gray-100 px-3 py-2 text-xs text-gray-500">
          <p className="truncate">
            {provider} · {tableFunctionLabel}
          </p>

          <div className="flex flex-wrap gap-1">
            {connection.credentialEnvVars.length === 0 ? (
              <span className="rounded bg-emerald-50 px-1.5 py-0.5 text-[10px] uppercase text-emerald-700">
                No credentials
              </span>
            ) : (
              connection.credentialEnvVars.map((envVar) => (
                <span
                  className="rounded bg-amber-50 px-1.5 py-0.5 text-[10px] uppercase text-amber-700"
                  key={envVar}
                >
                  {envVar}
                </span>
              ))
            )}
          </div>

          {connection.tables.length > 0 && (
            <ul className="space-y-1">
              {connection.tables.map((table) => (
                <li
                  className="flex items-center justify-between gap-2"
                  key={table.name}
                >
                  <span className="truncate text-gray-700">{table.name}</span>
                  <span className="shrink-0 rounded bg-gray-50 px-1.5 py-0.5 text-[10px] uppercase text-gray-400">
                    function
                  </span>
                </li>
              ))}
            </ul>
          )}

          <div className="flex items-center gap-3">
            <button
              aria-label={`Edit saved connection ${connection.name}`}
              className="text-xs font-medium text-gray-600 transition-colors hover:text-gray-950"
              onClick={() => onEdit(connection)}
              type="button"
            >
              Edit
            </button>
            <button
              aria-label={`Delete saved connection ${connection.name}`}
              className="text-xs font-medium text-red-600 transition-colors hover:text-red-700"
              onClick={() => onDelete(connection.name)}
              type="button"
            >
              Delete saved connection
            </button>
          </div>
        </div>
      )}
    </article>
  );
}

function DatasourceListItem({
  entry,
  onRemove,
}: {
  entry: DatasourceEntry;
  onRemove: (name: string) => void;
}) {
  const hasTables = entry.tables.length > 0;
  const isApiTables = entry.kind === "api_tables";

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
        <div className="flex shrink-0 items-center gap-1">
          <DatasourceKindBadge entry={entry} />
          <button
            aria-label={`Remove ${entry.name}`}
            className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-900"
            onClick={() => onRemove(entry.name)}
            title={`Remove ${entry.name}`}
            type="button"
          >
            <Trash2Icon size={14} strokeWidth={1.5} />
          </button>
        </div>
      </div>

      {hasTables ? (
        <div className="mt-3 space-y-3">
          {entry.tables.map((table) => (
            <div className="space-y-1" key={table.name}>
              <div className="flex items-center justify-between gap-2 text-xs">
                <span className="truncate font-medium text-gray-700">
                  {table.name}
                </span>
                {isApiTables ? (
                  <span className="shrink-0 rounded bg-gray-50 px-1.5 py-0.5 text-[10px] uppercase text-gray-400">
                    function
                  </span>
                ) : (
                  table.rowCount !== null && (
                    <span className="shrink-0 text-gray-400">
                      {table.rowCount.toLocaleString()} rows
                    </span>
                  )
                )}
              </div>
              {isApiTables && table.columns.length > 0 && (
                <p className="text-[10px] uppercase text-gray-400">
                  Output schema
                </p>
              )}
              {table.columns.map((column) => (
                <DatasourceColumnRow column={column} key={column.name} />
              ))}
            </div>
          ))}
        </div>
      ) : (
        <div className="mt-3 space-y-1">
          {entry.columns.map((column) => (
            <DatasourceColumnRow column={column} key={column.name} />
          ))}
        </div>
      )}

      {!hasTables && entry.rowCount !== null && (
        <p className="mt-3 text-xs text-gray-400">
          {entry.rowCount.toLocaleString()} rows
        </p>
      )}
    </article>
  );
}

function DatasourceKindBadge({ entry }: { entry: DatasourceEntry }) {
  if (entry.kind === "api_tables") {
    return (
      <span className="inline-flex items-center gap-1 rounded bg-gray-100 px-1.5 py-0.5 text-[10px] uppercase text-gray-500">
        <PlugIcon size={10} strokeWidth={1.5} />
        API
      </span>
    );
  }

  return (
    <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] uppercase text-gray-500">
      {entry.kind}
    </span>
  );
}

function DatasourceColumnRow({
  column,
}: {
  column: DatasourceEntry["columns"][number];
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr),auto] items-center gap-2 text-xs">
      <span className="truncate text-gray-700">{column.name}</span>
      <span className="truncate text-gray-400">{column.sqlType}</span>
    </div>
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

function isDatasourcePath(path: string): boolean {
  const extension = path.split(/[\\/]/).pop()?.split(".").pop()?.toLowerCase();
  return Boolean(extension && DATASOURCE_EXTENSIONS.includes(extension));
}

function isPositionInsideElement(
  element: HTMLElement | null,
  position: { x: number; y: number },
): boolean {
  if (!element) return false;

  const rect = element.getBoundingClientRect();
  const scale = window.devicePixelRatio || 1;
  const x = position.x / scale;
  const y = position.y / scale;

  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

function errorMessage(caught: unknown): string {
  if (caught instanceof Error) return caught.message;
  return String(caught);
}
