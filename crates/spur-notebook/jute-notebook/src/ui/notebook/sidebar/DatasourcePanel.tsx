import { type Event as TauriEvent, listen } from "@tauri-apps/api/event";
import { type DragDropEvent, getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import {
  ChevronDownIcon,
  ChevronRightIcon,
  FileUpIcon,
  PlayIcon,
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

import type {
  CellCronTrigger,
  CellDagMetadata,
  ConnectionTemplate,
  DatasourceEntry,
} from "@/bindings";
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
import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "@/stores/sidebar";
import AddRestApiWizard, {
  type AddRestApiWizardPrefill,
  type AddRestApiWizardSavedConnectionRecovery,
} from "@/ui/notebook/AddRestApiWizard";

import { setCellSchedule } from "../../dag/scheduleApi";

type DroppedFile = File & {
  path?: string;
  webkitRelativePath?: string;
};

type DatasourceGroup = {
  key: string;
  label: string;
  entries: DatasourceEntry[];
};

type SavedConnectionRecovery = AddRestApiWizardSavedConnectionRecovery;
type SavedConnectionTableSelection = Record<string, string[]>;

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

const DEFAULT_TABLE_QUERY_TRIGGER: CellCronTrigger = {
  enabled: true,
  cron: "*/15 * * * *",
  timezone: "UTC",
  run_target: "cascade",
  skip_if_running: true,
  catch_up: false,
};

// eslint-disable-next-line react-refresh/only-export-components
export function restWizardPrefillFromPayload(
  payload: unknown,
): AddRestApiWizardPrefill | null {
  if (!payload || typeof payload !== "object") return null;

  const record = payload as Record<string, unknown>;
  if (typeof record.name !== "string" || record.name.trim().length === 0) {
    return null;
  }

  const missingEnvVars = Array.isArray(record.missingEnvVars)
    ? record.missingEnvVars
    : Array.isArray(record.missing_env_vars)
      ? record.missing_env_vars
      : [];
  const specText =
    typeof record.specText === "string"
      ? record.specText
      : typeof record.spec_text === "string"
        ? record.spec_text
        : undefined;
  const manifestToml =
    typeof record.manifestToml === "string"
      ? record.manifestToml
      : typeof record.manifest_toml === "string"
        ? record.manifest_toml
        : undefined;
  const connectionOnly =
    typeof record.connectionOnly === "boolean"
      ? record.connectionOnly
      : typeof record.connection_only === "boolean"
        ? record.connection_only
        : undefined;
  const provider =
    typeof record.provider === "string" && record.provider.trim().length > 0
      ? record.provider
      : undefined;

  return {
    name: record.name,
    provider,
    specText,
    manifestToml,
    connectionOnly,
    missingEnvVars: missingEnvVars.filter(
      (envVar): envVar is string => typeof envVar === "string",
    ),
    step: "connect",
  };
}

export default function DatasourcePanel() {
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
  const [selectedSavedConnectionTables, setSelectedSavedConnectionTables] =
    useState<SavedConnectionTableSelection>({});
  const [savedConnectionRecovery, setSavedConnectionRecovery] =
    useState<SavedConnectionRecovery | null>(null);
  const [apiModalOpen, setApiModalOpen] = useState(false);
  const [restWizardPrefill, setRestWizardPrefill] =
    useState<AddRestApiWizardPrefill | null>(null);
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
        return false;
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
        return true;
      } catch (caught) {
        setError(errorMessage(caught));
        return false;
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

  const handleAttachSavedConnection = useCallback(
    async (name: string, tables: string[] = []) => {
      const savedConnection = savedConnections.find(
        (connection) => connection.name === name,
      );
      const selectedTables = tables.filter((table) => table.trim().length > 0);

      setError(null);
      setSavedConnectionRecovery(null);

      try {
        const response = await daemonControl(
          attachSavedConnectionCommand({ name, tables: selectedTables }),
        );
        const { entry, missingEnvVars } =
          attachedSavedConnectionFromDaemonControlResponse(response);
        setEntries((current) => {
          const nextEntries = upsertDatasourceEntry(current, entry);
          entriesRef.current = nextEntries;
          return nextEntries;
        });
        if (missingEnvVars.length > 0) {
          if (savedConnection) {
            setSavedConnectionRecovery({
              connection: savedConnection,
              missingEnvVars,
              tableNames: selectedTables,
            });
          } else {
            setError(
              `Saved connection "${name}" needs ${missingEnvVars.join(
                ", ",
              )}, but it is no longer in the saved list.`,
            );
          }
        } else {
          setSelectedSavedConnectionTables((current) => ({
            ...current,
            [name]: [],
          }));
        }
      } catch (caught) {
        setError(errorMessage(caught));
      }
    },
    [savedConnections],
  );

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
      setSelectedSavedConnectionTables((current) => {
        const { [name]: _removed, ...rest } = current;
        return rest;
      });
      setSavedConnectionRecovery(null);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, []);

  const handleToggleSavedConnectionTable = useCallback(
    (connectionName: string, tableName: string, checked: boolean) => {
      setSelectedSavedConnectionTables((current) => {
        const selected = current[connectionName] ?? [];
        const nextSelected = checked
          ? selected.includes(tableName)
            ? selected
            : [...selected, tableName]
          : selected.filter((name) => name !== tableName);
        return {
          ...current,
          [connectionName]: nextSelected,
        };
      });
    },
    [],
  );

  const handlePickLocalDatasource = useCallback(async () => {
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Datasource", extensions: DATASOURCE_EXTENSIONS }],
    });

    return firstSelectedPath(selected);
  }, []);

  const handleAttachLocalDatasource = useCallback(
    async (path: string) => {
      const attached = await attachPath(path);
      if (!attached) {
        throw new Error("Datasource could not be attached.");
      }
    },
    [attachPath],
  );

  const handleScheduleTableRelation = useCallback(
    async (schemaName: string, tableName: string) => {
      setError(null);
      try {
        const response = await daemonControl({
          command: "insert_cell",
          kind: "code",
          after_id: null,
          source: tableRelationQuerySource(schemaName, tableName),
          last_edited_by: "datasource",
          code_type: "sql",
        });
        if (!response.ok) {
          throw new Error(
            response.error?.message ?? "Failed to create query cell",
          );
        }
        if (
          response.result?.type !== "delta" ||
          response.result.data.kind.type !== "cellInserted"
        ) {
          throw new Error("daemon insert_cell did not return an inserted cell");
        }

        const cell = response.result.data.kind.cell;
        const version = await setCellDagMetadata(
          cell.id,
          tableFunctionDagMetadata(tableName),
          cell.version,
        );
        await setCellSchedule(cell.id, DEFAULT_TABLE_QUERY_TRIGGER, version);
      } catch (caught) {
        setError(errorMessage(caught));
      }
    },
    [],
  );

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

    try {
      void listen("notebook://open_rest_wizard", (event) => {
        const nextPrefill = restWizardPrefillFromPayload(event.payload);
        if (!nextPrefill) return;

        useSidebar.getState().activatePanel(DEFAULT_SIDEBAR_PANEL_ID);
        setEditingConnection(null);
        setSavedConnectionRecovery(null);
        setRestWizardPrefill(nextPrefill);
        setApiModalOpen(true);
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

  return (
    <>
      <div className="flex h-full min-h-0 flex-col gap-3 px-3 pb-16 pt-3 text-gray-700">
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
            onClick={() => {
              setEditingConnection(null);
              setRestWizardPrefill(null);
              setSavedConnectionRecovery(null);
              setApiModalOpen(true);
            }}
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
            <span>{pendingPath ? "Attaching..." : "Drop datasource file"}</span>
          </div>
        </section>

        {error && (
          <div className="rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
            {error}
          </div>
        )}

        <div
          className="min-h-0 flex-1 space-y-3 overflow-y-auto pb-20 pr-1"
          data-testid="datasource-panel-scroll"
        >
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
                          onScheduleTableRelation={handleScheduleTableRelation}
                          onRemove={handleDetachDatasource}
                        />
                      ))}
                    </div>
                  </section>
                ))}
              </div>
            )}
          </section>

          <SavedConnectionsSection
            connections={savedConnections}
            expandedName={expandedSavedConnection}
            onAttach={(name, tables) =>
              void handleAttachSavedConnection(name, tables)
            }
            onDelete={(name) => void handleDeleteSavedConnection(name)}
            onEdit={(connection) => {
              setRestWizardPrefill(null);
              setSavedConnectionRecovery(null);
              setEditingConnection(connection);
            }}
            onFixAndAttach={(recovery) => {
              setEditingConnection(null);
              setRestWizardPrefill(null);
              setSavedConnectionRecovery(recovery);
              setApiModalOpen(true);
            }}
            onToggle={(name) =>
              setExpandedSavedConnection((current) =>
                current === name ? null : name,
              )
            }
            recovery={savedConnectionRecovery}
            selectedTables={selectedSavedConnectionTables}
            onToggleTable={handleToggleSavedConnectionTable}
          />
        </div>
      </div>
      <AddRestApiWizard
        editConnection={editingConnection}
        initialSavedConnectionRecovery={savedConnectionRecovery}
        onAttachLocalFile={handleAttachLocalDatasource}
        open={apiModalOpen || editingConnection !== null}
        onPickLocalFile={handlePickLocalDatasource}
        prefill={restWizardPrefill}
        onClose={() => {
          setApiModalOpen(false);
          setEditingConnection(null);
          setRestWizardPrefill(null);
          setSavedConnectionRecovery(null);
        }}
      />
    </>
  );
}

function SavedConnectionsSection({
  connections,
  expandedName,
  onAttach,
  onDelete,
  onEdit,
  onFixAndAttach,
  onToggle,
  recovery,
  selectedTables,
  onToggleTable,
}: {
  connections: ConnectionTemplate[];
  expandedName: string | null;
  onAttach: (name: string, tables?: string[]) => void;
  onDelete: (name: string) => void;
  onEdit: (connection: ConnectionTemplate) => void;
  onFixAndAttach: (recovery: SavedConnectionRecovery) => void;
  onToggle: (name: string) => void;
  recovery: SavedConnectionRecovery | null;
  selectedTables: SavedConnectionTableSelection;
  onToggleTable: (
    connectionName: string,
    tableName: string,
    checked: boolean,
  ) => void;
}) {
  return (
    <section className="border-t border-gray-200 pt-3">
      <h3 className="mb-2 truncate text-xs uppercase tracking-wide text-gray-400">
        Saved connections
      </h3>
      {recovery && (
        <div className="mb-2 space-y-2 break-words rounded border border-amber-200 bg-amber-50 px-2 py-2 text-xs text-amber-800">
          <p>
            Missing credentials for {recovery.connection.name}:{" "}
            {recovery.missingEnvVars.join(", ")}.
          </p>
          <button
            aria-label={`Fix and attach ${recovery.connection.name}`}
            className="rounded border border-amber-300 bg-white px-2 py-1 font-medium text-amber-900 transition-colors hover:border-amber-500 hover:text-amber-950"
            onClick={() => onFixAndAttach(recovery)}
            type="button"
          >
            Fix and attach
          </button>
        </div>
      )}
      {connections.length === 0 ? (
        <p className="px-1 py-1 text-xs text-gray-400">No saved connections.</p>
      ) : (
        <div className="space-y-1">
          {connections.map((connection) => (
            <SavedConnectionRow
              connection={connection}
              expanded={expandedName === connection.name}
              key={connection.name}
              onAttach={onAttach}
              onDelete={onDelete}
              onEdit={onEdit}
              onToggle={onToggle}
              selectedTables={selectedTables[connection.name] ?? []}
              onToggleTable={onToggleTable}
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
  selectedTables,
  onToggleTable,
}: {
  connection: ConnectionTemplate;
  expanded: boolean;
  onAttach: (name: string, tables?: string[]) => void;
  onDelete: (name: string) => void;
  onEdit: (connection: ConnectionTemplate) => void;
  onToggle: (name: string) => void;
  selectedTables: string[];
  onToggleTable: (
    connectionName: string,
    tableName: string,
    checked: boolean,
  ) => void;
}) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const credentialCount = connection.credentialEnvVars.length;
  const provider = connection.provider?.trim() || "custom";
  const hasTableFunctions = connection.tables.length > 0;
  const tableFunctionLabel = `${connection.tables.length} ${
    connection.tables.length === 1 ? "table-function" : "table-functions"
  }`;

  useEffect(() => {
    if (!expanded) {
      setConfirmingDelete(false);
    }
  }, [expanded]);

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
          aria-label={
            hasTableFunctions
              ? `Select table-functions from saved connection ${connection.name}`
              : `Attach saved connection ${connection.name}`
          }
          className="shrink-0 rounded border border-gray-300 px-2 py-1 text-xs font-medium text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950"
          onClick={() =>
            hasTableFunctions
              ? onToggle(connection.name)
              : onAttach(connection.name)
          }
          type="button"
        >
          {hasTableFunctions ? "Select" : "Attach"}
        </button>
      </div>

      {expanded && (
        <div className="space-y-2 border-t border-gray-100 px-3 py-2 text-xs text-gray-500">
          <p className="truncate">
            {provider} · {tableFunctionLabel}
          </p>

          <div className="flex min-w-0 flex-wrap gap-1">
            {connection.credentialEnvVars.length === 0 ? (
              <span className="max-w-full break-all rounded bg-emerald-50 px-1.5 py-0.5 text-[10px] uppercase text-emerald-700">
                No credentials
              </span>
            ) : (
              connection.credentialEnvVars.map((envVar) => (
                <span
                  className="max-w-full break-all rounded bg-amber-50 px-1.5 py-0.5 text-[10px] uppercase text-amber-700"
                  key={envVar}
                >
                  {envVar}
                </span>
              ))
            )}
          </div>

          {hasTableFunctions && (
            <div className="space-y-2">
              <ul className="space-y-1">
                {connection.tables.map((table) => (
                  <li
                    className="flex min-w-0 items-center justify-between gap-2"
                    key={table.name}
                  >
                    <label className="flex min-w-0 flex-1 items-center gap-2 text-gray-700">
                      <input
                        aria-label={`Select ${table.name} from ${connection.name}`}
                        checked={selectedTables.includes(table.name)}
                        className="h-3.5 w-3.5 shrink-0 rounded border-gray-300 text-gray-900 focus:ring-gray-900"
                        onChange={(event) =>
                          onToggleTable(
                            connection.name,
                            table.name,
                            event.currentTarget.checked,
                          )
                        }
                        type="checkbox"
                      />
                      <span className="min-w-0 truncate">{table.name}</span>
                    </label>
                    <span className="shrink-0 rounded bg-gray-50 px-1.5 py-0.5 text-[10px] uppercase text-gray-400">
                      function
                    </span>
                  </li>
                ))}
              </ul>
              <button
                aria-label={`Attach selected table-functions from ${connection.name}`}
                className="rounded border border-gray-300 px-2 py-1 text-xs font-medium text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
                disabled={selectedTables.length === 0}
                onClick={() => onAttach(connection.name, selectedTables)}
                type="button"
              >
                Attach selected
              </button>
            </div>
          )}

          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <button
              aria-label={`Edit saved connection ${connection.name}`}
              className="text-xs font-medium text-gray-600 transition-colors hover:text-gray-950"
              onClick={() => onEdit(connection)}
              type="button"
            >
              Edit
            </button>
            {confirmingDelete ? (
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <button
                  aria-label={`Delete permanently ${connection.name}`}
                  className="text-left text-xs font-semibold text-red-700 transition-colors hover:text-red-800"
                  onClick={() => {
                    setConfirmingDelete(false);
                    onDelete(connection.name);
                  }}
                  type="button"
                >
                  Delete permanently
                </button>
                <button
                  aria-label={`Cancel delete saved connection ${connection.name}`}
                  className="text-left text-xs font-medium text-gray-500 transition-colors hover:text-gray-800"
                  onClick={() => setConfirmingDelete(false)}
                  type="button"
                >
                  Cancel
                </button>
              </div>
            ) : (
              <button
                aria-label={`Delete saved connection ${connection.name}`}
                className="text-left text-xs font-medium text-red-600 transition-colors hover:text-red-700"
                onClick={() => setConfirmingDelete(true)}
                type="button"
              >
                Delete saved connection
              </button>
            )}
          </div>
        </div>
      )}
    </article>
  );
}

function DatasourceListItem({
  entry,
  onScheduleTableRelation,
  onRemove,
}: {
  entry: DatasourceEntry;
  onScheduleTableRelation: (schemaName: string, tableName: string) => void;
  onRemove: (name: string) => void;
}) {
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const hasTables = entry.tables.length > 0;
  const isApiTables = entry.kind === "api_tables";

  return (
    <article className="rounded border border-gray-200 bg-white p-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0 flex-1">
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
          {confirmingRemove ? (
            <div className="flex items-center gap-1">
              <button
                aria-label={`Confirm remove ${entry.name}`}
                className="rounded border border-red-200 px-1.5 py-0.5 text-[10px] font-semibold text-red-700 transition-colors hover:border-red-300 hover:text-red-800"
                onClick={() => {
                  setConfirmingRemove(false);
                  onRemove(entry.name);
                }}
                type="button"
              >
                Remove
              </button>
              <button
                aria-label={`Cancel remove ${entry.name}`}
                className="rounded border border-gray-200 px-1.5 py-0.5 text-[10px] font-medium text-gray-500 transition-colors hover:border-gray-300 hover:text-gray-800"
                onClick={() => setConfirmingRemove(false)}
                type="button"
              >
                Cancel
              </button>
            </div>
          ) : (
            <button
              aria-label={`Remove ${entry.name}`}
              className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-900"
              onClick={() => setConfirmingRemove(true)}
              title={`Remove ${entry.name}`}
              type="button"
            >
              <Trash2Icon size={14} strokeWidth={1.5} />
            </button>
          )}
        </div>
      </div>

      {hasTables ? (
        <div className="mt-3 space-y-3">
          {entry.tables.map((table) => {
            const relationName = tableRelationName(entry.name, table.name);
            return (
              <div className="space-y-1" key={table.name}>
                <div className="flex items-center justify-between gap-2 text-xs">
                  <span
                    className="truncate font-medium text-gray-700"
                    title={relationName}
                  >
                    {relationName}
                  </span>
                  {isApiTables ? (
                    <div className="flex shrink-0 items-center gap-1">
                      <span className="rounded bg-gray-50 px-1.5 py-0.5 text-[10px] uppercase text-gray-400">
                        table
                      </span>
                      <button
                        aria-label={`Schedule query ${relationName}`}
                        className="inline-flex h-6 w-6 items-center justify-center rounded border border-gray-200 bg-white text-gray-500 transition-colors hover:border-gray-300 hover:bg-gray-50 hover:text-gray-900"
                        onClick={() =>
                          onScheduleTableRelation(entry.name, table.name)
                        }
                        title={`Create scheduled query for ${relationName}`}
                        type="button"
                      >
                        <PlayIcon size={12} strokeWidth={1.8} />
                      </button>
                    </div>
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
            );
          })}
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

function tableRelationName(schemaName: string, tableName: string): string {
  return `${schemaName}.${tableName}`;
}

function tableRelationQuerySource(
  schemaName: string,
  tableName: string,
): string {
  const relation = `${duckDbIdentifier(schemaName)}.${duckDbIdentifier(tableName)}`;
  return `SELECT * FROM ${relation} LIMIT 100;\n`;
}

async function setCellDagMetadata(
  cellId: string,
  dag: CellDagMetadata,
  expectedVersion: number,
): Promise<number> {
  const response = await daemonControl({
    command: "set_cell_metadata",
    id: cellId,
    patch: { spur: { dag } },
    expected_version: expectedVersion,
  });
  if (!response.ok) {
    throw new Error(
      response.error?.message ?? "Failed to mark query as DAG node",
    );
  }
  if (
    response.result?.type !== "delta" ||
    response.result.data.kind.type !== "cellWritten"
  ) {
    throw new Error("daemon set_cell_metadata did not return an updated cell");
  }
  return response.result.data.kind.cell.version;
}

function tableFunctionDagMetadata(tableName: string): CellDagMetadata {
  return {
    produces: [],
    consumes: [],
    source: {
      kind: "api_tables",
      port: tableName,
      class: "dataframe",
    },
  };
}

function duckDbIdentifier(value: string): string {
  if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(value)) {
    return value;
  }
  return `"${value.replaceAll('"', '""')}"`;
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
