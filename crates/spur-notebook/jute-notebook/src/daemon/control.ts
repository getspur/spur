import { invoke } from "@tauri-apps/api/core";

import type {
  DaemonControlCommand,
  DaemonControlResponse,
  DaemonNotebookSnapshot,
  DatasourceEntry,
  RecentNotebookEntry,
} from "@/bindings";

type PathCommandName = "open" | "rename" | "new" | "new_at" | "reopen";
export type AttachDatasourceCommand = Extract<
  DaemonControlCommand,
  { command: "attach_datasource" }
>;
export type DetachDatasourceCommand = Extract<
  DaemonControlCommand,
  { command: "detach_datasource" }
>;
export type AddApiDatasourceCommand = Extract<
  DaemonControlCommand,
  { command: "add_api_datasource" }
>;
export type ListDatasourcesCommand = Extract<
  DaemonControlCommand,
  { command: "list_datasources" }
>;
export type AttachDatasourceInput = Omit<AttachDatasourceCommand, "command">;
export type DetachDatasourceInput = Omit<DetachDatasourceCommand, "command">;
type EnrichedRecentEntry = NonNullable<
  DaemonControlResponse["entries"]
>[number] &
  Partial<Pick<RecentNotebookEntry, "kernelAlive" | "isCurrent">>;

export async function daemonControl(
  cmd: DaemonControlCommand,
): Promise<DaemonControlResponse> {
  return await invoke<DaemonControlResponse>("daemon_control", { cmd });
}

export function attachDatasourceCommand(
  input: AttachDatasourceInput,
): AttachDatasourceCommand {
  return {
    command: "attach_datasource",
    name: input.name,
    path: input.path,
    group: input.group,
  };
}

export function addApiDatasourceCommand(input: {
  name: string;
  source: string;
}): AddApiDatasourceCommand {
  return {
    command: "add_api_datasource" as const,
    name: input.name,
    source: input.source,
  };
}

export function detachDatasourceCommand(
  input: DetachDatasourceInput,
): DetachDatasourceCommand {
  return {
    command: "detach_datasource",
    name: input.name,
  };
}

export function listDatasourcesCommand(): ListDatasourcesCommand {
  return {
    command: "list_datasources",
  };
}

export function datasourceEntryFromDaemonControlResponse(
  response: DaemonControlResponse,
): DatasourceEntry {
  if (
    response.ok &&
    response.result?.type === "datasource" &&
    isDatasourceEntry(response.result.data)
  ) {
    return response.result.data;
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error(
    "daemon attach_datasource response did not include datasource",
  );
}

export function datasourceEntriesFromDaemonControlResponse(
  response: DaemonControlResponse,
): DatasourceEntry[] {
  if (
    response.ok &&
    response.result?.type === "datasources" &&
    datasourceEntriesFromUnknown(response.result.data)
  ) {
    return response.result.data;
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error(
    "daemon list_datasources response did not include datasources",
  );
}

export function datasourceEntriesFromEventPayload(
  payload: unknown,
): DatasourceEntry[] {
  if (datasourceEntriesFromUnknown(payload)) {
    return payload;
  }
  throw new Error("datasources://changed payload did not include entries");
}

export function pathFromDaemonControlResponse(
  response: DaemonControlResponse,
  command: PathCommandName,
): string {
  if (response.path) return response.path;
  throw new Error(`daemon ${command} response did not include path`);
}

export function snapshotFromDaemonControlResponse(
  response: DaemonControlResponse,
): DaemonNotebookSnapshot {
  if (response.ok && response.result?.type === "snapshot") {
    return response.result.data;
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error("daemon snapshot response did not include snapshot");
}

export function recentEntriesFromDaemonControlResponse(
  response: DaemonControlResponse,
): RecentNotebookEntry[] {
  return (response.entries ?? []).map((entry) => {
    const enriched = entry as EnrichedRecentEntry;
    return {
      path: entry.path,
      lastOpened: entry.lastOpened,
      isScratch: entry.isScratch,
      pinned: entry.pinned,
      kernelAlive: enriched.kernelAlive ?? false,
      isCurrent: enriched.isCurrent ?? false,
    };
  });
}

function isDatasourceEntry(value: unknown): value is DatasourceEntry {
  if (typeof value !== "object" || value === null) return false;

  const candidate = value as Partial<DatasourceEntry>;
  return (
    typeof candidate.name === "string" &&
    typeof candidate.path === "string" &&
    (candidate.kind === "csv" ||
      candidate.kind === "parquet" ||
      candidate.kind === "json" ||
      candidate.kind === "duck_db" ||
      candidate.kind === "sqlite" ||
      candidate.kind === "api_tables") &&
    (candidate.group === null || typeof candidate.group === "string") &&
    Array.isArray(candidate.columns) &&
    candidate.columns.every(
      (column) =>
        typeof column === "object" &&
        column !== null &&
        typeof column.name === "string" &&
        typeof column.sqlType === "string",
    ) &&
    (candidate.rowCount === null || typeof candidate.rowCount === "number") &&
    (candidate.tables === undefined ||
      (Array.isArray(candidate.tables) &&
        candidate.tables.every(
          (table) =>
            typeof table === "object" &&
            table !== null &&
            typeof table.name === "string" &&
            Array.isArray(table.columns) &&
            table.columns.every(
              (column) =>
                typeof column === "object" &&
                column !== null &&
                typeof column.name === "string" &&
                typeof column.sqlType === "string",
            ) &&
            (table.rowCount === null || typeof table.rowCount === "number"),
        )))
  );
}

function datasourceEntriesFromUnknown(
  value: unknown,
): value is DatasourceEntry[] {
  return Array.isArray(value) && value.every(isDatasourceEntry);
}
