export type DatasourceSourceFamilyKey =
  | "file"
  | "cloud_object_storage"
  | "lakehouse"
  | "database"
  | "rest_api"
  | "rss"
  | "advanced_sql";

export type DatasourceWizardStepKey =
  | "source"
  | "locate"
  | "auth"
  | "inspect"
  | "attach";

export type DatasourceSourceFamily = {
  key: DatasourceSourceFamilyKey;
  label: string;
  shortDetail: string;
  duckDbMechanism: string;
  setupRequirements: readonly string[];
  defaultExampleInput: string;
  attachable: boolean;
  attachUnavailableReason?: string;
};

export type DatasourceWizardStep = {
  key: DatasourceWizardStepKey;
  label: string;
  detail: string;
};

export type RestApiSourceModeKey = "catalog" | "saved" | "openapi" | "manual";

export type RestApiSourceMode = {
  key: RestApiSourceModeKey;
  family: "rest_api";
  label: string;
  shortDetail: string;
};

export type RestApiWizardStepKey = Extract<
  DatasourceWizardStepKey,
  "source" | "auth" | "inspect" | "attach"
>;

export type RestApiWizardStep = DatasourceWizardStep & {
  key: RestApiWizardStepKey;
};

export const DATASOURCE_SOURCE_FAMILIES = [
  {
    key: "file",
    label: "File or folder",
    shortDetail: "Local CSV, JSON, Parquet, SQLite, and DuckDB files.",
    duckDbMechanism: "Native file scans and read_csv/read_json/read_parquet",
    setupRequirements: ["Readable local path"],
    defaultExampleInput: "/Users/me/data/orders.parquet",
    attachable: true,
  },
  {
    key: "cloud_object_storage",
    label: "URL or object storage",
    shortDetail: "S3, GCS, Azure Blob, and HTTPS object paths.",
    duckDbMechanism: "httpfs extension with object-store secrets",
    setupRequirements: ["Bucket URL", "Cloud credentials or signed URL"],
    defaultExampleInput: "s3://company-lake/events/*.parquet",
    attachable: false,
    attachUnavailableReason:
      "Unavailable until a backend attach contract exists for object storage.",
  },
  {
    key: "lakehouse",
    label: "Lakehouse table",
    shortDetail: "Delta, Iceberg, and partitioned data lake tables.",
    duckDbMechanism: "DuckDB lakehouse extensions and parquet metadata scans",
    setupRequirements: ["Table root or catalog URI", "Catalog credentials"],
    defaultExampleInput: "s3://warehouse/tables/orders_delta",
    attachable: false,
    attachUnavailableReason:
      "Unavailable until a backend attach contract exists for lakehouse tables.",
  },
  {
    key: "database",
    label: "External database",
    shortDetail: "Postgres, MySQL, SQLite, and attached database engines.",
    duckDbMechanism: "DuckDB database scanner extensions and ATTACH",
    setupRequirements: ["Connection string", "Database credentials"],
    defaultExampleInput: "postgres://user@localhost:5432/app",
    attachable: false,
    attachUnavailableReason:
      "Unavailable until a backend attach contract exists for external databases.",
  },
  {
    key: "rest_api",
    label: "REST API",
    shortDetail: "OpenAPI specs, provider catalogs, and saved API manifests.",
    duckDbMechanism: "httpfs plus generated table-functions over API responses",
    setupRequirements: ["Base URL or provider", "API credentials or token"],
    defaultExampleInput: "https://api.example.com/openapi.json",
    attachable: true,
  },
  {
    key: "rss",
    label: "RSS / RSSHub",
    shortDetail: "Direct RSS feeds and RSSHub's predefined route catalog.",
    duckDbMechanism: "spur_rest extension table-functions over RSSHub and RSS",
    setupRequirements: [
      "No credentials required",
      "Browse routes with rss.routes",
      "Attach a route card as a zero-argument subscription table",
    ],
    defaultExampleInput: "rsshub://youtube/video/UC123",
    attachable: true,
  },
  {
    key: "advanced_sql",
    label: "Advanced SQL attach",
    shortDetail: "Custom SQL, views, macros, and table-function definitions.",
    duckDbMechanism: "User-authored DuckDB SQL, views, macros, and extensions",
    setupRequirements: ["SQL statement or script", "Referenced attachments"],
    defaultExampleInput: "select * from read_parquet('/data/*.parquet')",
    attachable: false,
    attachUnavailableReason:
      "Unavailable until a backend attach contract exists for advanced SQL attachments.",
  },
] as const satisfies readonly DatasourceSourceFamily[];

export const DATASOURCE_WIZARD_STEPS = [
  { key: "source", label: "Source", detail: "choose family" },
  { key: "locate", label: "Locate", detail: "path or endpoint" },
  { key: "auth", label: "Auth", detail: "credentials" },
  { key: "inspect", label: "Inspect", detail: "schema" },
  { key: "attach", label: "Attach", detail: "register" },
] as const satisfies readonly DatasourceWizardStep[];

export const REST_API_SOURCE_MODES = [
  {
    key: "catalog",
    family: "rest_api",
    label: "Provider catalog",
    shortDetail:
      "Browse Nango providers with auth mode and import tier pre-filled.",
  },
  {
    key: "saved",
    family: "rest_api",
    label: "Saved connections",
    shortDetail: "Attach a reusable API connection template to this notebook.",
  },
  {
    key: "openapi",
    family: "rest_api",
    label: "OpenAPI spec",
    shortDetail: "Paste a spec or URL and preview generated table-functions.",
  },
  {
    key: "manual",
    family: "rest_api",
    label: "Manual",
    shortDetail: "Hand-author the manifest later and add tables when ready.",
  },
] as const satisfies readonly RestApiSourceMode[];

export const REST_API_WIZARD_STEPS = [
  { key: "source", label: "Source", detail: "pick how" },
  { key: "auth", label: "Connect", detail: "auth and URL" },
  { key: "inspect", label: "Tables", detail: "schema" },
  { key: "attach", label: "Review", detail: "add" },
] as const satisfies readonly RestApiWizardStep[];

export const datasourceFamilyByKey = indexByKey(DATASOURCE_SOURCE_FAMILIES);
export const datasourceWizardStepByKey = indexByKey(DATASOURCE_WIZARD_STEPS);

function indexByKey<const TItems extends readonly { key: string }[]>(
  items: TItems,
): { [TKey in TItems[number]["key"]]: Extract<TItems[number], { key: TKey }> } {
  return Object.fromEntries(items.map((item) => [item.key, item])) as {
    [TKey in TItems[number]["key"]]: Extract<TItems[number], { key: TKey }>;
  };
}
