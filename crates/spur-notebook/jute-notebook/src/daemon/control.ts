import { invoke } from "@tauri-apps/api/core";

import type {
  ConnectionTemplate,
  DaemonControlCommand,
  DaemonControlResponse,
  DaemonNotebookSnapshot,
  DatasourceEntry,
  OpenApiTablePreview,
  ProviderSummary,
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
export type ListNangoProvidersCommand = Extract<
  DaemonControlCommand,
  { command: "list_nango_providers" }
>;
export type PreviewOpenApiTablesCommand = Extract<
  DaemonControlCommand,
  { command: "preview_open_api_tables" }
>;
export type AddApiDatasourceFromImportCommand = Extract<
  DaemonControlCommand,
  { command: "add_api_datasource_from_import" }
>;
export type ListDatasourcesCommand = Extract<
  DaemonControlCommand,
  { command: "list_datasources" }
>;
export type ListSavedConnectionsCommand = Extract<
  DaemonControlCommand,
  { command: "list_saved_connections" }
>;
export type AttachSavedConnectionCommand = Extract<
  DaemonControlCommand,
  { command: "attach_saved_connection" }
>;
export type DeleteSavedConnectionCommand = Extract<
  DaemonControlCommand,
  { command: "delete_saved_connection" }
>;
export type AttachDatasourceInput = Omit<AttachDatasourceCommand, "command">;
export type DetachDatasourceInput = Omit<DetachDatasourceCommand, "command">;
export type AddApiDatasourceFromImportInput = Omit<
  AddApiDatasourceFromImportCommand,
  "command"
>;
export type AttachSavedConnectionInput = Omit<
  AttachSavedConnectionCommand,
  "command"
>;
export type DeleteSavedConnectionInput = Omit<
  DeleteSavedConnectionCommand,
  "command"
>;
export type AttachedSavedConnection = {
  entry: DatasourceEntry;
  missingEnvVars: string[];
};
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

export function listNangoProvidersCommand(): ListNangoProvidersCommand {
  return {
    command: "list_nango_providers",
  };
}

export function previewOpenApiTablesCommand(
  specText: string,
): PreviewOpenApiTablesCommand {
  return {
    command: "preview_open_api_tables",
    spec_text: specText,
  };
}

export function addApiDatasourceFromImportCommand(
  input: AddApiDatasourceFromImportInput,
): AddApiDatasourceFromImportCommand {
  return {
    command: "add_api_datasource_from_import",
    name: input.name,
    provider: input.provider,
    spec_text: input.spec_text,
    credentials: input.credentials,
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

export function listSavedConnectionsCommand(): ListSavedConnectionsCommand {
  return {
    command: "list_saved_connections",
  };
}

export function attachSavedConnectionCommand(
  input: AttachSavedConnectionInput,
): AttachSavedConnectionCommand {
  return {
    command: "attach_saved_connection",
    name: input.name,
  };
}

export function deleteSavedConnectionCommand(
  input: DeleteSavedConnectionInput,
): DeleteSavedConnectionCommand {
  return {
    command: "delete_saved_connection",
    name: input.name,
  };
}

export function nangoProvidersFromDaemonControlResponse(
  response: DaemonControlResponse,
): ProviderSummary[] {
  if (
    response.ok &&
    response.result?.type === "nangoProviders" &&
    providerSummariesFromUnknown(response.result.data)
  ) {
    return response.result.data;
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error(
    "daemon list_nango_providers response did not include providers",
  );
}

export function openApiTablePreviewFromDaemonControlResponse(
  response: DaemonControlResponse,
): OpenApiTablePreview {
  if (
    response.ok &&
    response.result?.type === "openApiTablePreview" &&
    isOpenApiTablePreview(response.result.data)
  ) {
    return response.result.data;
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error(
    "daemon preview_open_api_tables response did not include table preview",
  );
}

export function importedApiDatasourceFromDaemonControlResponse(
  response: DaemonControlResponse,
): DatasourceEntry {
  return datasourceEntryFromDaemonControlResponse(response);
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

export function savedConnectionsFromDaemonControlResponse(
  response: DaemonControlResponse,
): ConnectionTemplate[] {
  if (
    response.ok &&
    response.result?.type === "savedConnections" &&
    connectionTemplatesFromUnknown(response.result.data)
  ) {
    return response.result.data;
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error(
    "daemon list_saved_connections response did not include saved connections",
  );
}

export function attachedSavedConnectionFromDaemonControlResponse(
  response: DaemonControlResponse,
): AttachedSavedConnection {
  if (
    response.ok &&
    response.result?.type === "attachedSavedConnection" &&
    isAttachedSavedConnectionPayload(response.result.data)
  ) {
    return {
      entry: response.result.data.entry,
      missingEnvVars: response.result.data.missing_env_vars,
    };
  }
  if (response.error) {
    throw new Error(response.error.message);
  }
  throw new Error(
    "daemon attach_saved_connection response did not include saved connection",
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

function connectionTemplatesFromUnknown(
  value: unknown,
): value is ConnectionTemplate[] {
  return Array.isArray(value) && value.every(isConnectionTemplate);
}

function isConnectionTemplate(value: unknown): value is ConnectionTemplate {
  if (typeof value !== "object" || value === null) return false;

  const candidate = value as Partial<ConnectionTemplate>;
  return (
    typeof candidate.name === "string" &&
    (candidate.provider === null || typeof candidate.provider === "string") &&
    (candidate.group === null || typeof candidate.group === "string") &&
    typeof candidate.manifestToml === "string" &&
    Array.isArray(candidate.tables) &&
    Array.isArray(candidate.credentialEnvVars) &&
    candidate.credentialEnvVars.every((envVar) => typeof envVar === "string") &&
    typeof candidate.createdAt === "string" &&
    typeof candidate.updatedAt === "string"
  );
}

function isAttachedSavedConnectionPayload(value: unknown): value is {
  entry: DatasourceEntry;
  missing_env_vars: string[];
} {
  if (typeof value !== "object" || value === null) return false;

  const candidate = value as {
    entry?: unknown;
    missing_env_vars?: unknown;
  };
  return (
    isDatasourceEntry(candidate.entry) &&
    Array.isArray(candidate.missing_env_vars) &&
    candidate.missing_env_vars.every((envVar) => typeof envVar === "string")
  );
}

function providerSummariesFromUnknown(
  value: unknown,
): value is ProviderSummary[] {
  return Array.isArray(value) && value.every(isProviderSummary);
}

function isProviderSummary(value: unknown): value is ProviderSummary {
  if (typeof value !== "object" || value === null) return false;

  const candidate = value as Partial<ProviderSummary>;
  return (
    typeof candidate.name === "string" &&
    typeof candidate.displayName === "string" &&
    typeof candidate.category === "string" &&
    typeof candidate.tier === "string" &&
    typeof candidate.authMode === "string"
  );
}

function isOpenApiTablePreview(value: unknown): value is OpenApiTablePreview {
  if (typeof value !== "object" || value === null) return false;

  const candidate = value as Partial<OpenApiTablePreview>;
  return (
    Array.isArray(candidate.tables) &&
    candidate.tables.every(
      (table) =>
        typeof table === "object" &&
        table !== null &&
        typeof table.name === "string" &&
        typeof table.path === "string" &&
        (table.responsePath === null ||
          typeof table.responsePath === "string") &&
        Array.isArray(table.columns) &&
        table.columns.every(
          (column) =>
            typeof column === "object" &&
            column !== null &&
            typeof column.name === "string" &&
            typeof column.ty === "string" &&
            typeof column.json === "string",
        ),
    )
  );
}
