import clsx from "clsx";
import {
  AlertCircleIcon,
  CheckIcon,
  DatabaseIcon,
  FileTextIcon,
  FolderOpenIcon,
  Loader2Icon,
  PencilIcon,
  PlugIcon,
  RssIcon,
  SearchIcon,
  TableIcon,
  XIcon,
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useState } from "react";

import type {
  ConnectionTemplate,
  OpenApiTablePreview,
  ProviderSummary,
  TablePreview,
} from "@/bindings";
import {
  addApiDatasourceCommand,
  addApiDatasourceFromImportCommand,
  addApiDatasourceFromManifestCommand,
  attachSavedConnectionCommand,
  attachedSavedConnectionFromDaemonControlResponse,
  daemonControl,
  importedApiDatasourceFromDaemonControlResponse,
  listNangoProvidersCommand,
  listSavedConnectionsCommand,
  nangoProvidersFromDaemonControlResponse,
  openApiTablePreviewFromDaemonControlResponse,
  previewOpenApiTablesCommand,
  savedConnectionsFromDaemonControlResponse,
  updateSavedConnectionCommand,
} from "@/daemon/control";
import {
  DATASOURCE_SOURCE_FAMILIES,
  DATASOURCE_WIZARD_STEPS,
  type DatasourceSourceFamily,
  type DatasourceSourceFamilyKey,
  type DatasourceWizardStepKey,
  REST_API_SOURCE_MODES,
  type RestApiSourceModeKey,
  datasourceFamilyByKey,
} from "@/ui/notebook/datasourceWizardModel";

type SourceMode = RestApiSourceModeKey;
type SourceFamily = DatasourceSourceFamilyKey;
type RssSourceMode = "direct" | "rsshub" | "keyword";
type WizardStep = DatasourceWizardStepKey;
type StepVisualState = "active" | "complete" | "skipped" | "locked";
type ProviderLoadState = "idle" | "loading" | "loaded";
type ProviderFulfillmentStatus = "Ready" | "Candidate" | "Blocked" | "Catalog";
type CredentialField = {
  key: string;
  label: string;
  type: "password" | "text";
};
type RssRouteParameter = {
  key: string;
  label: string;
  defaultValue: string;
};
type RssRouteExample = {
  title: string;
  meta: string;
};
type RssHubRoute = {
  id: string;
  name: string;
  template: string;
  category: string;
  heat: string;
  view: string;
  tags: string[];
  parameters: RssRouteParameter[];
  previewTitle: string;
  previewDescription: string;
  examples: RssRouteExample[];
};

export type AddRestApiWizardPrefill = {
  name: string;
  provider?: string | null;
  specText?: string | null;
  manifestToml?: string | null;
  connectionOnly?: boolean;
  missingEnvVars: string[];
  step: "connect";
};

export type AddRestApiWizardSavedConnectionRecovery = {
  connection: ConnectionTemplate;
  missingEnvVars: string[];
  tableNames?: string[];
};

export type AddRestApiWizardProps = {
  editConnection?: ConnectionTemplate | null;
  initialSavedConnectionRecovery?: AddRestApiWizardSavedConnectionRecovery | null;
  onAttachLocalFile?: (path: string) => Promise<void> | void;
  onClose: () => void;
  onPickLocalFile?: () => Promise<string | null> | string | null;
  open: boolean;
  prefill?: AddRestApiWizardPrefill | null;
};

const STEPS: readonly { key: WizardStep; label: string; detail: string }[] =
  DATASOURCE_WIZARD_STEPS;
const ALL_STEP_INDEXES = STEPS.map((_, index) => index);
const REST_API_STEP_INDEXES = [0, 2, 3, 4] as const;
const RSS_STEP_INDEXES = [0, 4] as const;

const KNOWN_BASE_URLS: Record<string, string> = {
  airtable: "https://api.airtable.com",
  github: "https://api.github.com",
  hubspot: "https://api.hubapi.com",
  linear: "https://api.linear.app",
  notion: "https://api.notion.com",
  shopify: "https://${SPUR_CONN_shop}.myshopify.com",
  slack: "https://slack.com/api",
  square: "https://connect.squareup.com",
  stripe: "https://api.stripe.com",
  twilio: "https://api.twilio.com",
  zendesk: "https://{subdomain}.zendesk.com/api/v2",
};

const RSS_SOURCE_MODES: readonly {
  key: RssSourceMode;
  label: string;
  detail: string;
}[] = [
  {
    key: "direct",
    label: "Direct URL",
    detail: "Paste an existing RSS or Atom feed URL.",
  },
  {
    key: "rsshub",
    label: "RSSHub route",
    detail: "Browse a route and fill its path parameters.",
  },
  {
    key: "keyword",
    label: "Keyword discovery",
    detail: "Search route examples before choosing a feed.",
  },
] as const;

const RSSHUB_EXAMPLE_ROUTES: readonly RssHubRoute[] = [
  {
    id: "youtube-channel",
    name: "YouTube Channel",
    template: "youtube/channel/:id",
    category: "Video",
    heat: "278k",
    view: "Videos",
    tags: ["video", "parameterized", "top feed"],
    parameters: [
      {
        key: "id",
        label: "Channel ID",
        defaultValue: "UCYO_jab_esuFRV4b17AJtAw",
      },
    ],
    previewTitle: "3Blue1Brown uploads",
    previewDescription:
      "Validated through RSSHub. Latest entries are fetched with rss_entries(url) before subscription persistence is added.",
    examples: [
      {
        title: "But what is a convolution?",
        meta: "2 days ago - video - canonical link found",
      },
      {
        title: "A visual guide to transformers",
        meta: "1 week ago - author and categories parsed",
      },
    ],
  },
  {
    id: "github-issues",
    name: "GitHub Issues",
    template: "github/issue/:owner/:repo",
    category: "Programming",
    heat: "92k",
    view: "Articles",
    tags: ["programming", "issues", "radar"],
    parameters: [
      { key: "owner", label: "Owner", defaultValue: "spur-dev" },
      { key: "repo", label: "Repository", defaultValue: "spur" },
    ],
    previewTitle: "GitHub Issues radar",
    previewDescription:
      "Issues are exposed as feed entries, with links and timestamps available through rss_entries(url).",
    examples: [
      {
        title: "Add RSSHub subscription onboarding",
        meta: "open issue - labels and author mapped",
      },
      {
        title: "Improve feed preview status states",
        meta: "updated recently - comments linked",
      },
    ],
  },
  {
    id: "reddit-subreddit",
    name: "Reddit Subreddit",
    template: "reddit/subreddit/:name",
    category: "Social",
    heat: "184k",
    view: "Social",
    tags: ["social", "community"],
    parameters: [{ key: "name", label: "Subreddit", defaultValue: "rust" }],
    previewTitle: "r/rust posts",
    previewDescription:
      "Community posts are fetched through RSSHub and normalized into rss_entries(url).",
    examples: [
      {
        title: "Rust 2026 roadmap discussion",
        meta: "hot post - comments linked",
      },
      {
        title: "Async ecosystem release notes",
        meta: "new post - category preserved",
      },
    ],
  },
  {
    id: "hackernews-jobs",
    name: "Hacker News Jobs",
    template: "hackernews/jobs",
    category: "News",
    heat: "61k",
    view: "Jobs",
    tags: ["news", "no params"],
    parameters: [],
    previewTitle: "Hacker News jobs",
    previewDescription:
      "A no-parameter route that can be previewed directly through rss_feed(url).",
    examples: [
      {
        title: "Backend engineer, data systems",
        meta: "today - source link found",
      },
      {
        title: "Product engineer, notebooks",
        meta: "yesterday - company metadata parsed",
      },
    ],
  },
] as const;

export default function AddRestApiWizard({
  editConnection = null,
  initialSavedConnectionRecovery = null,
  onAttachLocalFile,
  onClose,
  onPickLocalFile,
  open,
  prefill = null,
}: AddRestApiWizardProps) {
  const editMode = editConnection !== null;
  const [stepIndex, setStepIndex] = useState(0);
  const [sourceFamily, setSourceFamily] = useState<SourceFamily>("rest_api");
  const [sourceMode, setSourceMode] = useState<SourceMode | null>(null);
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [providerLoadState, setProviderLoadState] =
    useState<ProviderLoadState>("idle");
  const [savedConnections, setSavedConnections] = useState<
    ConnectionTemplate[]
  >([]);
  const [savedConnectionLoadState, setSavedConnectionLoadState] =
    useState<ProviderLoadState>("idle");
  const [providerSearch, setProviderSearch] = useState("");
  const [providerCategory, setProviderCategory] = useState("All");
  const [selectedProvider, setSelectedProvider] =
    useState<ProviderSummary | null>(null);
  const [selectedSavedConnection, setSelectedSavedConnection] =
    useState<ConnectionTemplate | null>(null);
  const [
    selectedSavedConnectionTableNames,
    setSelectedSavedConnectionTableNames,
  ] = useState<string[]>([]);
  const [datasourceName, setDatasourceName] = useState("");
  const [credentials, setCredentials] = useState<Record<string, string>>({});
  const [savedConnectionCredentials, setSavedConnectionCredentials] = useState<
    Record<string, string>
  >({});
  const [missingSavedCredentialKeys, setMissingSavedCredentialKeys] = useState<
    string[]
  >([]);
  const [prefillCredentialKeys, setPrefillCredentialKeys] = useState<string[]>(
    [],
  );
  const [prefillManifestToml, setPrefillManifestToml] = useState<string | null>(
    null,
  );
  const [specText, setSpecText] = useState("");
  const [genericLocation, setGenericLocation] = useState<string>(
    datasourceFamilyByKey.file.defaultExampleInput,
  );
  const [tablePreview, setTablePreview] = useState<OpenApiTablePreview | null>(
    null,
  );
  const [connectionOnly, setConnectionOnly] = useState(false);
  const [pendingLocalFilePick, setPendingLocalFilePick] = useState(false);
  const [pendingPreview, setPendingPreview] = useState(false);
  const [pendingAdd, setPendingAdd] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hasUserChanges, setHasUserChanges] = useState(false);
  const [confirmingClose, setConfirmingClose] = useState(false);
  const dialogTitleId = useId();
  const closeConfirmationTitleId = useId();
  const closeConfirmationDescriptionId = useId();

  const markDirty = useCallback(() => {
    setHasUserChanges(true);
  }, []);

  const requestClose = useCallback(() => {
    if (hasUserChanges) {
      setConfirmingClose(true);
      return;
    }

    onClose();
  }, [hasUserChanges, onClose]);

  useEffect(() => {
    if (!open) return;

    setHasUserChanges(false);
    setConfirmingClose(false);

    if (editConnection) {
      setStepIndex(2);
      setSourceFamily("rest_api");
      setSourceMode("saved");
      setProviderSearch("");
      setProviderCategory("All");
      setSelectedProvider(null);
      setSelectedSavedConnection(editConnection);
      setSelectedSavedConnectionTableNames([]);
      setDatasourceName(editConnection.name);
      setCredentials({});
      setSavedConnectionCredentials({});
      setMissingSavedCredentialKeys([]);
      setPrefillCredentialKeys([]);
      setPrefillManifestToml(null);
      setSpecText("");
      setGenericLocation(datasourceFamilyByKey.rest_api.defaultExampleInput);
      setTablePreview(tablePreviewFromTemplate(editConnection));
      setConnectionOnly(editConnection.tables.length === 0);
      setPendingLocalFilePick(false);
      setPendingPreview(false);
      setPendingAdd(false);
      setError(null);
      return;
    }

    if (initialSavedConnectionRecovery) {
      const recoveryCredentials = credentialsFromKeys(
        initialSavedConnectionRecovery.missingEnvVars,
      );

      setStepIndex(0);
      setSourceFamily("rest_api");
      setSourceMode("saved");
      setProviders([]);
      setProviderLoadState("idle");
      setSavedConnections([initialSavedConnectionRecovery.connection]);
      setSavedConnectionLoadState("loaded");
      setProviderSearch("");
      setProviderCategory("All");
      setSelectedProvider(null);
      setSelectedSavedConnection(initialSavedConnectionRecovery.connection);
      setSelectedSavedConnectionTableNames(
        initialSavedConnectionRecovery.tableNames ?? [],
      );
      setDatasourceName("");
      setCredentials({});
      setSavedConnectionCredentials(recoveryCredentials);
      setMissingSavedCredentialKeys(
        initialSavedConnectionRecovery.missingEnvVars,
      );
      setPrefillCredentialKeys([]);
      setPrefillManifestToml(null);
      setSpecText("");
      setGenericLocation(datasourceFamilyByKey.rest_api.defaultExampleInput);
      setTablePreview(null);
      setConnectionOnly(false);
      setPendingLocalFilePick(false);
      setPendingPreview(false);
      setPendingAdd(false);
      setError(null);
      return;
    }

    if (prefill) {
      setStepIndex(2);
      setSourceFamily("rest_api");
      setSourceMode(prefill.provider ? "catalog" : "openapi");
      setProviderSearch("");
      setProviderCategory("All");
      setSelectedProvider(
        prefill.provider ? providerSummaryFromPrefill(prefill.provider) : null,
      );
      setSelectedSavedConnection(null);
      setSelectedSavedConnectionTableNames([]);
      setDatasourceName(prefill.name);
      setCredentials({});
      setSavedConnectionCredentials({});
      setMissingSavedCredentialKeys([]);
      setPrefillCredentialKeys(prefill.missingEnvVars);
      setPrefillManifestToml(
        prefill.manifestToml && prefill.manifestToml.trim().length > 0
          ? prefill.manifestToml
          : null,
      );
      setSpecText(prefill.specText ?? "");
      setGenericLocation(datasourceFamilyByKey.rest_api.defaultExampleInput);
      setTablePreview(null);
      setConnectionOnly(prefill.connectionOnly ?? false);
      setPendingLocalFilePick(false);
      setPendingPreview(false);
      setPendingAdd(false);
      setError(null);
      return;
    }

    setStepIndex(0);
    setSourceFamily("rest_api");
    setSourceMode(null);
    setProviderSearch("");
    setProviderCategory("All");
    setSelectedProvider(null);
    setSelectedSavedConnection(null);
    setSelectedSavedConnectionTableNames([]);
    setDatasourceName("");
    setCredentials({});
    setSavedConnectionCredentials({});
    setMissingSavedCredentialKeys([]);
    setPrefillCredentialKeys([]);
    setPrefillManifestToml(null);
    setSpecText("");
    setGenericLocation(datasourceFamilyByKey.rest_api.defaultExampleInput);
    setTablePreview(null);
    setConnectionOnly(false);
    setPendingLocalFilePick(false);
    setPendingPreview(false);
    setPendingAdd(false);
    setError(null);
  }, [editConnection, initialSavedConnectionRecovery, open, prefill]);

  useEffect(() => {
    if (!open) return;

    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") requestClose();
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, requestClose]);

  const loadProviders = useCallback(async () => {
    if (providerLoadState === "loading" || providerLoadState === "loaded") {
      return;
    }

    setProviderLoadState("loading");
    setError(null);

    try {
      const response = await daemonControl(listNangoProvidersCommand());
      setProviders(nangoProvidersFromDaemonControlResponse(response));
      setProviderLoadState("loaded");
    } catch (caught) {
      setProviderLoadState("idle");
      setError(errorMessage(caught));
    }
  }, [providerLoadState]);

  const loadSavedConnections = useCallback(async () => {
    if (
      savedConnectionLoadState === "loading" ||
      savedConnectionLoadState === "loaded"
    ) {
      return;
    }

    setSavedConnectionLoadState("loading");
    setError(null);

    try {
      const response = await daemonControl(listSavedConnectionsCommand());
      setSavedConnections(savedConnectionsFromDaemonControlResponse(response));
      setSavedConnectionLoadState("loaded");
    } catch (caught) {
      setSavedConnectionLoadState("idle");
      setError(errorMessage(caught));
    }
  }, [savedConnectionLoadState]);

  const selectSourceMode = useCallback(
    (mode: SourceMode) => {
      if (sourceMode !== mode) markDirty();
      setSourceMode(mode);
      setError(null);
      setTablePreview(null);
      setConnectionOnly(false);
      setMissingSavedCredentialKeys([]);
      setSavedConnectionCredentials({});
      setPrefillCredentialKeys([]);
      setPrefillManifestToml(null);

      if (mode === "catalog") {
        setSelectedSavedConnection(null);
        setSelectedSavedConnectionTableNames([]);
        void loadProviders();
      } else if (mode === "saved") {
        setSelectedProvider(null);
        setCredentials({});
        setDatasourceName("");
        void loadSavedConnections();
      } else {
        setSelectedProvider(null);
        setSelectedSavedConnection(null);
        setSelectedSavedConnectionTableNames([]);
        setCredentials({});
        setDatasourceName((currentName) =>
          !currentName || currentName === selectedProvider?.name
            ? "rest_api"
            : currentName,
        );
      }
    },
    [
      loadProviders,
      loadSavedConnections,
      markDirty,
      selectedProvider?.name,
      sourceMode,
    ],
  );

  const selectSourceFamily = useCallback(
    (family: SourceFamily) => {
      if (sourceFamily !== family) markDirty();
      setSourceFamily(family);
      setError(null);
      setTablePreview(null);
      setConnectionOnly(false);
      setMissingSavedCredentialKeys([]);
      setSavedConnectionCredentials({});
      setPrefillCredentialKeys([]);
      setPrefillManifestToml(null);
      setGenericLocation(datasourceFamilyByKey[family].defaultExampleInput);

      if (family === "rest_api") {
        return;
      }

      setSourceMode(null);
      setSelectedProvider(null);
      setSelectedSavedConnection(null);
      setSelectedSavedConnectionTableNames([]);
      setCredentials({});
      setDatasourceName(defaultDatasourceNameForFamily(family));
      setSpecText("");
    },
    [markDirty, sourceFamily],
  );

  const selectProvider = useCallback(
    (provider: ProviderSummary) => {
      const defaultName = defaultDatasourceNameForProvider(provider);
      if (isBlockedProvider(provider)) return;

      if (selectedProvider?.name !== provider.name) markDirty();
      setSelectedProvider(provider);
      setSelectedSavedConnection(null);
      setSelectedSavedConnectionTableNames([]);
      setDatasourceName((currentName) =>
        !currentName || currentName === "rest_api" ? defaultName : currentName,
      );
      setCredentials({});
      setTablePreview(tablePreviewFromProvider(provider));
      setSpecText(isReadyProvider(provider) ? "" : (provider.specUrl ?? ""));
      setConnectionOnly(false);
      setPrefillCredentialKeys([]);
      setPrefillManifestToml(null);
      setError(null);
    },
    [markDirty, selectedProvider?.name],
  );

  const selectSavedConnection = useCallback(
    (connection: ConnectionTemplate) => {
      if (selectedSavedConnection?.name !== connection.name) markDirty();
      setSelectedSavedConnection(connection);
      setSelectedSavedConnectionTableNames([]);
      setSelectedProvider(null);
      setMissingSavedCredentialKeys([]);
      setSavedConnectionCredentials({});
      setPrefillCredentialKeys([]);
      setPrefillManifestToml(null);
      setError(null);
    },
    [markDirty, selectedSavedConnection?.name],
  );

  const providerCategories = useMemo(() => {
    return [
      "All",
      ...Array.from(
        new Set(
          providers
            .map((provider) => provider.category)
            .filter((category) => category.length > 0),
        ),
      ).sort((left, right) => left.localeCompare(right)),
    ];
  }, [providers]);

  const filteredProviders = useMemo(() => {
    const normalizedSearch = providerSearch.trim().toLowerCase();

    return providers
      .filter((provider) => {
        const matchesCategory =
          providerCategory === "All" || provider.category === providerCategory;
        const matchesSearch =
          normalizedSearch.length === 0 ||
          provider.displayName.toLowerCase().includes(normalizedSearch) ||
          provider.name.toLowerCase().includes(normalizedSearch);

        return matchesCategory && matchesSearch;
      })
      .sort((left, right) => left.displayName.localeCompare(right.displayName));
  }, [providerCategory, providerSearch, providers]);

  const credentialFields = useMemo(
    () =>
      editConnection
        ? editConnection.credentialEnvVars.map(
            (key): CredentialField => ({ key, label: key, type: "password" }),
          )
        : prefillCredentialKeys.length > 0
          ? prefillCredentialKeys.map(
              (key): CredentialField => ({
                key,
                label: key,
                type: "password",
              }),
            )
          : credentialFieldsForProvider(selectedProvider),
    [editConnection, prefillCredentialKeys, selectedProvider],
  );
  const canContinue = canContinueFromStep({
    connectionOnly,
    credentialFields,
    credentials,
    datasourceName,
    genericLocation,
    missingSavedCredentialKeys,
    savedConnectionCredentials,
    selectedProvider,
    selectedSavedConnection,
    sourceFamily,
    sourceMode,
    stepIndex,
    tablePreview,
    editMode,
  });
  const manifestPrefillMode = prefillManifestToml !== null;
  const dialogTitle = editMode ? "Edit saved connection" : "Add datasource";

  if (!open) return null;

  const step = STEPS[stepIndex]?.key ?? "source";
  const selectedFamily = datasourceFamilyByKey[sourceFamily];
  const restFamilySelected = sourceFamily === "rest_api";
  const rssFamilySelected = sourceFamily === "rss";

  const goNext = () => {
    if (!canContinue) return;
    setStepIndex((currentStep) => {
      if (sourceFamily === "rest_api" && currentStep === 0) return 2;
      if (sourceFamily === "rss" && currentStep === 0) return 4;
      return Math.min(STEPS.length - 1, currentStep + 1);
    });
    setError(null);
  };

  const goBack = () => {
    setStepIndex((currentStep) => {
      if (sourceFamily === "rss" && currentStep === 4) return editMode ? 2 : 0;
      if (sourceFamily === "rest_api" && currentStep === 2) {
        return editMode ? 2 : 0;
      }
      return Math.max(editMode ? 2 : 0, currentStep - 1);
    });
    setError(null);
  };

  const handlePreviewTables = async () => {
    const trimmedSpec = specText.trim();
    if (trimmedSpec.length === 0) {
      setError("Paste an OpenAPI spec or URL before previewing tables.");
      return;
    }

    setPendingPreview(true);
    setError(null);

    try {
      const response = await daemonControl(
        previewOpenApiTablesCommand(trimmedSpec),
      );
      setTablePreview(openApiTablePreviewFromDaemonControlResponse(response));
      setConnectionOnly(false);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingPreview(false);
    }
  };

  const handlePickLocalFile = async () => {
    if (!onPickLocalFile) return;

    setPendingLocalFilePick(true);
    setError(null);

    try {
      const path = await onPickLocalFile();
      if (path) setGenericLocation(path);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingLocalFilePick(false);
    }
  };

  const handleAttachGenericDatasource = async () => {
    if (sourceFamily !== "file" || !onAttachLocalFile) {
      setError(
        "Run the generated SQL in a notebook SQL cell to attach this datasource.",
      );
      return;
    }

    const path = genericLocation.trim();
    setPendingAdd(true);
    setError(null);

    try {
      await onAttachLocalFile(path);
      onClose();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingAdd(false);
    }
  };

  const handleAddRssDatasource = async () => {
    const name = datasourceName.trim();
    if (name.length === 0) {
      setError("Datasource name is required.");
      return;
    }

    setPendingAdd(true);
    setError(null);

    try {
      const response = await daemonControl(
        addApiDatasourceCommand({ name, source: "rss" }),
      );
      importedApiDatasourceFromDaemonControlResponse(response);
      onClose();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingAdd(false);
    }
  };

  const handleAddDatasource = async () => {
    const trimmedSpec = specText.trim();
    const credentialPairs = credentialFields
      .map((field): [string, string] => [
        field.key,
        credentials[field.key]?.trim() ?? "",
      ])
      .filter(([, value]) => value.length > 0);

    setPendingAdd(true);
    setError(null);

    try {
      if (prefillManifestToml) {
        const response = await daemonControl(
          addApiDatasourceFromManifestCommand({
            name: datasourceName.trim(),
            manifest_toml: prefillManifestToml,
            credentials: credentialPairs,
          }),
        );
        importedApiDatasourceFromDaemonControlResponse(response);
        onClose();
        return;
      }

      const response = await daemonControl(
        addApiDatasourceFromImportCommand({
          name: datasourceName.trim(),
          provider: selectedProvider?.providerKey ?? null,
          spec_text:
            connectionOnly || trimmedSpec.length === 0 ? null : trimmedSpec,
          credentials: credentialPairs,
        }),
      );
      importedApiDatasourceFromDaemonControlResponse(response);
      onClose();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingAdd(false);
    }
  };

  const handleSaveEdit = async () => {
    if (!editConnection) return;

    const trimmedSpec = specText.trim();
    const credentialPairs = credentialFields
      .map((field): [string, string] => [
        field.key,
        credentials[field.key]?.trim() ?? "",
      ])
      .filter(([, value]) => value.length > 0);

    setPendingAdd(true);
    setError(null);

    try {
      const response = await daemonControl(
        updateSavedConnectionCommand({
          name: editConnection.name,
          spec_text: trimmedSpec.length === 0 ? null : trimmedSpec,
          credentials: credentialPairs,
        }),
      );
      attachedSavedConnectionFromDaemonControlResponse(response);
      onClose();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingAdd(false);
    }
  };

  const handleAttachSavedConnection = async () => {
    if (!selectedSavedConnection) return;

    const credentialPairs = missingSavedCredentialKeys.map(
      (key): [string, string] => [
        key,
        savedConnectionCredentials[key]?.trim() ?? "",
      ],
    );

    if (credentialPairs.some(([, value]) => value.length === 0)) {
      setError("Fill the missing credentials before using this connection.");
      return;
    }

    setPendingAdd(true);
    setError(null);

    try {
      const response = await daemonControl(
        attachSavedConnectionCommand({
          name: selectedSavedConnection.name,
          ...(credentialPairs.length > 0
            ? { credentials: credentialPairs }
            : {}),
          ...(selectedSavedConnectionTableNames.length > 0
            ? { tables: selectedSavedConnectionTableNames }
            : {}),
        }),
      );
      const attached =
        attachedSavedConnectionFromDaemonControlResponse(response);

      if (attached.missingEnvVars.length > 0) {
        setMissingSavedCredentialKeys(attached.missingEnvVars);
        setSavedConnectionCredentials((currentCredentials) => {
          const nextCredentials: Record<string, string> = {};
          for (const key of attached.missingEnvVars) {
            nextCredentials[key] = currentCredentials[key] ?? "";
          }
          return nextCredentials;
        });
        return;
      }

      onClose();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingAdd(false);
    }
  };

  const primaryActionLabel =
    step === "attach"
      ? rssFamilySelected
        ? "Add datasource"
        : !restFamilySelected
          ? "Attach datasource"
          : editMode
            ? "Save changes"
            : "Add datasource"
      : manifestPrefillMode && step === "auth"
        ? "Add datasource"
        : step === "source" && sourceMode === "saved"
          ? "Use connection"
          : "Continue";

  return (
    <div
      aria-labelledby={dialogTitleId}
      aria-modal="true"
      className="fixed inset-0 z-40 flex items-center justify-center bg-gray-950/35 px-3 py-4 sm:px-4"
      onClick={requestClose}
      role="dialog"
    >
      <section
        className="flex h-[min(620px,calc(100vh-32px))] w-full max-w-3xl flex-col overflow-hidden rounded-lg border border-gray-200 bg-white shadow-2xl sm:h-[min(620px,calc(100vh-40px))] sm:flex-row"
        onClick={(event) => event.stopPropagation()}
      >
        <aside className="w-full shrink-0 border-b border-gray-200 bg-gray-50 px-3 py-3 sm:w-48 sm:border-b-0 sm:border-r sm:py-4">
          <div className="mb-4 flex items-center justify-between gap-2">
            <div className="flex min-w-0 items-center gap-2">
              <PlugIcon className="shrink-0 text-indigo-600" size={16} />
              <h2
                className="truncate text-sm font-medium text-gray-950"
                id={dialogTitleId}
              >
                {dialogTitle}
              </h2>
            </div>
            <button
              aria-label={`Close ${dialogTitle.toLowerCase()}`}
              className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-200 hover:text-gray-950"
              onClick={requestClose}
              type="button"
            >
              <XIcon size={15} strokeWidth={1.5} />
            </button>
          </div>
          <ol className="flex gap-1 overflow-x-auto sm:block sm:space-y-1">
            {STEPS.map((wizardStep, index) => {
              const visualState = stepVisualState({
                editMode,
                index,
                sourceFamily,
                stepIndex,
              });
              const active = visualState === "active";
              const complete = visualState === "complete";
              const skipped = visualState === "skipped";
              const navigable = active || complete;
              const visualDetail = skipped
                ? "not needed"
                : editMode && index === 0
                  ? "locked"
                  : wizardStep.detail;
              const stateLabel = skipped ? "not needed" : visualState;
              return (
                <li className="min-w-28 sm:min-w-0" key={wizardStep.key}>
                  <button
                    aria-current={active ? "step" : undefined}
                    aria-label={`${wizardStep.label}, ${stateLabel}, ${visualDetail}`}
                    className={clsx(
                      "flex w-full items-start gap-2 rounded px-2 py-2 text-left transition-colors",
                      active
                        ? "bg-white text-gray-950 shadow-sm ring-1 ring-gray-200"
                        : "text-gray-400",
                      complete && "text-gray-700 hover:bg-white",
                      skipped && "text-gray-500",
                    )}
                    disabled={!navigable}
                    onClick={() => setStepIndex(index)}
                    type="button"
                  >
                    <span
                      className={clsx(
                        "mt-0.5 inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border text-[11px]",
                        active && "border-indigo-600 bg-indigo-600 text-white",
                        complete &&
                          "border-green-200 bg-green-50 text-green-700",
                        skipped && "border-gray-200 bg-gray-50 text-gray-400",
                        !active &&
                          !complete &&
                          !skipped &&
                          "border-gray-300 bg-white text-gray-400",
                      )}
                    >
                      {complete ? (
                        <CheckIcon size={12} strokeWidth={2} />
                      ) : skipped ? (
                        "-"
                      ) : (
                        index + 1
                      )}
                    </span>
                    <span className="min-w-0">
                      <span className="block text-xs font-medium">
                        {wizardStep.label}
                      </span>
                      <span className="block truncate text-[11px] text-gray-400">
                        {visualDetail}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ol>
        </aside>

        <div className="flex min-w-0 flex-1 flex-col">
          <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
            {step === "source" && (
              <SourceStep
                error={error}
                filteredProviders={filteredProviders}
                onCategoryChange={setProviderCategory}
                onProviderSearch={setProviderSearch}
                onSelectFamily={selectSourceFamily}
                onSelectProvider={selectProvider}
                onSelectSavedConnection={selectSavedConnection}
                onSelectSourceMode={selectSourceMode}
                onSavedCredentialChange={(key, value) => {
                  if ((savedConnectionCredentials[key] ?? "") !== value) {
                    markDirty();
                  }
                  setSavedConnectionCredentials((current) => ({
                    ...current,
                    [key]: value,
                  }));
                }}
                providerCategories={providerCategories}
                providerCategory={providerCategory}
                providerLoadState={providerLoadState}
                providerSearch={providerSearch}
                savedConnectionCredentials={savedConnectionCredentials}
                savedConnectionLoadState={savedConnectionLoadState}
                savedConnections={savedConnections}
                selectedProvider={selectedProvider}
                selectedSavedConnection={selectedSavedConnection}
                missingSavedCredentialKeys={missingSavedCredentialKeys}
                sourceFamily={sourceFamily}
                sourceMode={sourceMode}
              />
            )}
            {step === "locate" &&
              (restFamilySelected ? (
                <RestLocateStep
                  selectedProvider={selectedProvider}
                  sourceMode={sourceMode}
                />
              ) : (
                <GenericLocateStep
                  family={selectedFamily}
                  location={genericLocation}
                  onPickLocalFile={
                    sourceFamily === "file" ? handlePickLocalFile : undefined
                  }
                  onLocationChange={setGenericLocation}
                  pendingLocalFilePick={pendingLocalFilePick}
                />
              ))}
            {step === "auth" &&
              (restFamilySelected ? (
                <ConnectStep
                  credentials={credentials}
                  datasourceName={datasourceName}
                  fields={credentialFields}
                  nameReadOnly={editMode}
                  onCredentialChange={(key, value) => {
                    if ((credentials[key] ?? "") !== value) markDirty();
                    setCredentials((current) => ({
                      ...current,
                      [key]: value,
                    }));
                  }}
                  onDatasourceNameChange={(value) => {
                    if (datasourceName !== value) markDirty();
                    setDatasourceName(value);
                  }}
                  assistantPrefillMode={manifestPrefillMode}
                  selectedProvider={selectedProvider}
                  selectedSavedConnection={selectedSavedConnection}
                  sourceMode={sourceMode}
                />
              ) : (
                <GenericAuthStep family={selectedFamily} />
              ))}
            {step === "inspect" &&
              (restFamilySelected ? (
                <TablesStep
                  connectionOnly={connectionOnly}
                  error={error}
                  onConnectionOnlyChange={(checked) => {
                    if (connectionOnly !== checked) markDirty();
                    setConnectionOnly(checked);
                    if (checked) setTablePreview(null);
                  }}
                  onPreviewTables={() => void handlePreviewTables()}
                  onSpecTextChange={(value) => {
                    if (specText !== value) markDirty();
                    setSpecText(value);
                    setTablePreview(
                      editConnection && value.trim().length === 0
                        ? tablePreviewFromTemplate(editConnection)
                        : null,
                    );
                    if (editConnection && value.trim().length === 0) {
                      setConnectionOnly(editConnection.tables.length === 0);
                    }
                  }}
                  pendingPreview={pendingPreview}
                  selectedProvider={selectedProvider}
                  specText={specText}
                  tablePreview={tablePreview}
                />
              ) : (
                <GenericInspectStep family={selectedFamily} />
              ))}
            {step === "attach" &&
              (restFamilySelected ? (
                <ReviewStep
                  connectionOnly={connectionOnly}
                  credentialFields={credentialFields}
                  credentials={credentials}
                  datasourceName={datasourceName}
                  error={error}
                  selectedProvider={selectedProvider}
                  specText={specText}
                  tablePreview={tablePreview}
                />
              ) : rssFamilySelected ? (
                <RssAttachStep
                  datasourceName={datasourceName}
                  error={error}
                  onDatasourceNameChange={(value) => {
                    if (datasourceName !== value) markDirty();
                    setDatasourceName(value);
                  }}
                />
              ) : (
                <GenericAttachStep
                  error={error}
                  family={selectedFamily}
                  location={genericLocation}
                />
              ))}
          </div>

          <footer className="flex min-h-14 shrink-0 items-center justify-between gap-3 border-t border-gray-200 bg-gray-50 px-5 py-2">
            <button
              className={clsx(
                "rounded border border-transparent px-3 py-2 text-sm font-medium text-gray-600 transition-colors hover:text-gray-950",
                stepIndex === 0 && "invisible",
                editMode && stepIndex <= 2 && "invisible",
              )}
              onClick={goBack}
              type="button"
            >
              Back
            </button>
            <button
              className="inline-flex max-w-[65%] items-center justify-center gap-2 whitespace-normal rounded border border-indigo-600 bg-indigo-600 px-4 py-2 text-center text-sm font-medium text-white transition-colors hover:bg-indigo-700 disabled:cursor-not-allowed disabled:border-gray-300 disabled:bg-gray-300 sm:max-w-none"
              disabled={!canContinue || pendingAdd}
              onClick={() => {
                if (step === "source" && sourceMode === "saved" && !editMode) {
                  void handleAttachSavedConnection();
                } else if (manifestPrefillMode && step === "auth") {
                  void handleAddDatasource();
                } else if (step === "attach") {
                  if (rssFamilySelected) void handleAddRssDatasource();
                  else if (!restFamilySelected) {
                    void handleAttachGenericDatasource();
                  } else if (editMode) void handleSaveEdit();
                  else void handleAddDatasource();
                } else {
                  goNext();
                }
              }}
              type="button"
            >
              {pendingAdd && <Loader2Icon className="animate-spin" size={14} />}
              {primaryActionLabel}
            </button>
          </footer>
        </div>
      </section>
      {confirmingClose && (
        <div
          className="absolute inset-0 z-10 flex items-center justify-center bg-gray-950/30 px-4"
          onClick={(event) => event.stopPropagation()}
        >
          <div
            aria-describedby={closeConfirmationDescriptionId}
            aria-labelledby={closeConfirmationTitleId}
            aria-modal="true"
            className="w-full max-w-sm rounded-lg border border-gray-200 bg-white p-4 shadow-xl"
            role="alertdialog"
          >
            <h3
              className="text-sm font-semibold text-gray-950"
              id={closeConfirmationTitleId}
            >
              Discard changes?
            </h3>
            <p
              className="mt-2 text-sm text-gray-600"
              id={closeConfirmationDescriptionId}
            >
              Closing now will discard the connection details entered in this
              wizard.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                className="rounded border border-gray-300 bg-white px-3 py-2 text-sm font-medium text-gray-700 transition-colors hover:border-gray-950 hover:text-gray-950"
                onClick={() => setConfirmingClose(false)}
                type="button"
              >
                Keep editing
              </button>
              <button
                className="rounded border border-red-600 bg-red-600 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-red-700"
                onClick={onClose}
                type="button"
              >
                Discard changes
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function SourceStep({
  error,
  filteredProviders,
  onCategoryChange,
  onSelectFamily,
  onProviderSearch,
  onSelectProvider,
  onSelectSavedConnection,
  onSelectSourceMode,
  onSavedCredentialChange,
  providerCategories,
  providerCategory,
  providerLoadState,
  providerSearch,
  savedConnectionCredentials,
  savedConnectionLoadState,
  savedConnections,
  selectedProvider,
  selectedSavedConnection,
  missingSavedCredentialKeys,
  sourceFamily,
  sourceMode,
}: {
  error: string | null;
  filteredProviders: ProviderSummary[];
  onCategoryChange: (category: string) => void;
  onSelectFamily: (family: SourceFamily) => void;
  onProviderSearch: (value: string) => void;
  onSelectProvider: (provider: ProviderSummary) => void;
  onSelectSavedConnection: (connection: ConnectionTemplate) => void;
  onSelectSourceMode: (mode: SourceMode) => void;
  onSavedCredentialChange: (key: string, value: string) => void;
  providerCategories: string[];
  providerCategory: string;
  providerLoadState: ProviderLoadState;
  providerSearch: string;
  savedConnectionCredentials: Record<string, string>;
  savedConnectionLoadState: ProviderLoadState;
  savedConnections: ConnectionTemplate[];
  selectedProvider: ProviderSummary | null;
  selectedSavedConnection: ConnectionTemplate | null;
  missingSavedCredentialKeys: string[];
  sourceFamily: SourceFamily;
  sourceMode: SourceMode | null;
}) {
  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">
        Choose a datasource family
      </h3>
      <p className="mt-1 text-sm text-gray-500">
        Select the kind of data you want to attach, then fill in only the setup
        details that apply to that source.
      </p>

      <div className="mt-4 grid grid-cols-1 gap-2 sm:grid-cols-2">
        {DATASOURCE_SOURCE_FAMILIES.map((family) => (
          <SourceOption
            active={sourceFamily === family.key}
            detail={family.shortDetail}
            icon={sourceFamilyIcon(family.key)}
            key={family.key}
            label={family.label}
            meta={sourceFamilyMeta(family)}
            onClick={() => onSelectFamily(family.key)}
          />
        ))}
      </div>

      {sourceFamily === "rest_api" && (
        <section className="mt-4 border-t border-gray-200 pt-4">
          <div className="mb-3">
            <h4 className="text-xs font-medium text-gray-950">
              REST API route
            </h4>
            <p className="mt-0.5 text-xs text-gray-500">
              Use the existing REST/API provider catalog, saved connections,
              OpenAPI spec import, or manual connection path.
            </p>
          </div>
          <div className="space-y-2">
            {REST_API_SOURCE_MODES.map((mode) => (
              <SourceOption
                active={sourceMode === mode.key}
                detail={mode.shortDetail}
                icon={sourceModeIcon(mode.key)}
                key={mode.key}
                label={mode.label}
                meta={sourceModeMeta(mode.key, savedConnections.length)}
                onClick={() => onSelectSourceMode(mode.key)}
              />
            ))}
          </div>
        </section>
      )}

      {sourceFamily === "rest_api" && sourceMode === "catalog" && (
        <section className="mt-4 border-t border-gray-200 pt-4">
          <label className="relative block">
            <span className="sr-only">Search providers</span>
            <SearchIcon
              className="pointer-events-none absolute left-2.5 top-2.5 text-gray-400"
              size={14}
              strokeWidth={1.5}
            />
            <input
              aria-label="Search providers"
              className="h-9 w-full rounded border border-gray-300 bg-white pl-8 pr-3 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
              onChange={(event) => onProviderSearch(event.currentTarget.value)}
              placeholder="Search providers"
              value={providerSearch}
            />
          </label>

          <div className="mt-3 flex flex-wrap gap-1.5">
            {providerCategories.map((category) => (
              <button
                className={clsx(
                  "rounded-full border px-2.5 py-1 text-xs transition-colors",
                  providerCategory === category
                    ? "border-gray-950 bg-gray-950 text-white"
                    : "border-gray-300 bg-white text-gray-600 hover:border-gray-900 hover:text-gray-950",
                )}
                key={category}
                onClick={() => onCategoryChange(category)}
                type="button"
              >
                {category}
              </button>
            ))}
          </div>

          <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
            {providerLoadState === "loading" && (
              <div className="col-span-full flex items-center gap-2 rounded border border-gray-200 bg-gray-50 px-3 py-3 text-sm text-gray-500">
                <Loader2Icon className="animate-spin" size={14} />
                Loading providers
              </div>
            )}
            {providerLoadState === "loaded" &&
              filteredProviders.map((provider) => (
                <ProviderTile
                  key={provider.name}
                  onSelect={onSelectProvider}
                  provider={provider}
                  selected={selectedProvider?.name === provider.name}
                />
              ))}
            {providerLoadState === "loaded" &&
              filteredProviders.length === 0 && (
                <div className="col-span-full rounded border border-gray-200 bg-gray-50 px-3 py-3 text-sm text-gray-500">
                  No providers match.
                </div>
              )}
          </div>

          <p className="mt-3 text-xs text-gray-500">
            Tier A is drop-in. Tier B requires a token you bring from the
            provider. Tier C is available for manual review.
          </p>
        </section>
      )}

      {sourceFamily === "rest_api" && sourceMode === "saved" && (
        <section className="mt-4 border-t border-gray-200 pt-4">
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {savedConnectionLoadState === "loading" && (
              <div className="flex items-center gap-2 rounded border border-gray-200 bg-gray-50 px-3 py-3 text-sm text-gray-500">
                <Loader2Icon className="animate-spin" size={14} />
                Loading saved connections
              </div>
            )}
            {savedConnectionLoadState === "loaded" &&
              savedConnections.map((connection) => (
                <SavedConnectionTile
                  connection={connection}
                  key={connection.name}
                  onSelect={onSelectSavedConnection}
                  selected={selectedSavedConnection?.name === connection.name}
                />
              ))}
            {savedConnectionLoadState === "loaded" &&
              savedConnections.length === 0 && (
                <div className="rounded border border-gray-200 bg-gray-50 px-3 py-3 text-sm text-gray-500">
                  No saved connections yet.
                </div>
              )}
          </div>

          {missingSavedCredentialKeys.length > 0 && (
            <SavedCredentialPrompt
              credentials={savedConnectionCredentials}
              credentialKeys={missingSavedCredentialKeys}
              onCredentialChange={onSavedCredentialChange}
            />
          )}
        </section>
      )}

      {error && <ErrorBanner message={error} />}
    </div>
  );
}

function sourceFamilyIcon(family: SourceFamily): React.ReactNode {
  if (family === "rest_api") return <PlugIcon size={18} strokeWidth={1.5} />;
  if (family === "rss") return <RssIcon size={18} strokeWidth={1.5} />;
  if (family === "database") {
    return <DatabaseIcon size={18} strokeWidth={1.5} />;
  }
  if (family === "advanced_sql") {
    return <PencilIcon size={18} strokeWidth={1.5} />;
  }
  if (family === "lakehouse") return <TableIcon size={18} strokeWidth={1.5} />;
  return <FileTextIcon size={18} strokeWidth={1.5} />;
}

function sourceFamilyMeta(family: DatasourceSourceFamily) {
  if (family.key === "rest_api") return "preserved";
  if (family.key === "rss") return "RSSHub";
  if (family.key === "file") return "local";
  if (family.key === "advanced_sql") return "SQL";
  return "DuckDB";
}

function sourceModeIcon(mode: SourceMode): React.ReactNode {
  if (mode === "catalog") return <PlugIcon size={18} strokeWidth={1.5} />;
  if (mode === "saved") return <DatabaseIcon size={18} strokeWidth={1.5} />;
  if (mode === "openapi") return <FileTextIcon size={18} strokeWidth={1.5} />;
  return <PencilIcon size={18} strokeWidth={1.5} />;
}

function sourceModeMeta(mode: SourceMode, savedConnectionCount: number) {
  if (mode === "catalog") return "Nango";
  if (mode === "saved") return `${savedConnectionCount} saved`;
  return undefined;
}

function SourceOption({
  active,
  detail,
  icon,
  label,
  meta,
  onClick,
}: {
  active: boolean;
  detail: string;
  icon: React.ReactNode;
  label: string;
  meta?: string;
  onClick: () => void;
}) {
  return (
    <button
      aria-label={label}
      aria-pressed={active}
      className={clsx(
        "flex w-full items-start gap-3 rounded-lg border bg-white p-3 text-left transition-colors",
        active
          ? "border-indigo-600 bg-indigo-50 ring-1 ring-indigo-600"
          : "border-gray-300 hover:border-gray-900",
      )}
      onClick={onClick}
      type="button"
    >
      <span
        className={clsx(
          "inline-flex h-9 w-9 shrink-0 items-center justify-center rounded bg-gray-100 text-gray-600",
          active && "bg-white text-indigo-600",
        )}
      >
        {icon}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block break-words text-sm font-medium text-gray-950">
          {label}
        </span>
        <span className="mt-0.5 block break-words text-xs text-gray-500">
          {detail}
        </span>
      </span>
      {meta && (
        <span className="shrink-0 rounded-full border border-indigo-200 bg-white px-2 py-1 text-xs font-medium text-indigo-600">
          {meta}
        </span>
      )}
    </button>
  );
}

function RestLocateStep({
  selectedProvider,
  sourceMode,
}: {
  selectedProvider: ProviderSummary | null;
  sourceMode: SourceMode | null;
}) {
  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">Locate REST API</h3>
      <p className="mt-1 text-sm text-gray-500">
        REST/API keeps the existing route: the provider catalog, saved
        connection, OpenAPI spec, or manual manifest supplies endpoint details.
      </p>
      <div className="mt-4 rounded-lg border border-gray-200 bg-gray-50">
        <SummaryRow label="Route">
          {sourceMode ? restSourceModeLabel(sourceMode) : "Select REST source"}
        </SummaryRow>
        <SummaryRow label="Endpoint">
          {selectedProvider
            ? baseUrlForProvider(selectedProvider)
            : "From spec or manifest"}
        </SummaryRow>
        <SummaryRow label="Setup">
          REST imports continue through Auth, Inspect, and Attach.
        </SummaryRow>
      </div>
    </div>
  );
}

function GenericLocateStep({
  family,
  location,
  onPickLocalFile,
  onLocationChange,
  pendingLocalFilePick,
}: {
  family: DatasourceSourceFamily;
  location: string;
  onPickLocalFile?: () => void;
  onLocationChange: (value: string) => void;
  pendingLocalFilePick: boolean;
}) {
  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">
        Locate {family.label.toLowerCase()}
      </h3>
      <p className="mt-1 text-sm text-gray-500">
        Provide the path, URL, catalog URI, connection string, or SQL entry
        point DuckDB will use to discover this source.
      </p>

      <label className="mt-4 block">
        <span className="text-xs font-medium text-gray-600">
          {locationLabelForFamily(family)}
        </span>
        <span className="mt-1 flex flex-col gap-2 sm:flex-row">
          <input
            aria-label={locationLabelForFamily(family)}
            className="h-9 min-w-0 flex-1 rounded border border-gray-300 bg-white px-2 font-mono text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            onChange={(event) => onLocationChange(event.currentTarget.value)}
            value={location}
          />
          {onPickLocalFile && (
            <button
              className="inline-flex h-9 shrink-0 items-center justify-center gap-2 rounded border border-gray-300 bg-white px-3 text-sm font-medium text-gray-700 transition-colors hover:border-gray-950 hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
              disabled={pendingLocalFilePick}
              onClick={onPickLocalFile}
              type="button"
            >
              {pendingLocalFilePick ? (
                <Loader2Icon className="animate-spin" size={14} />
              ) : (
                <FolderOpenIcon size={14} strokeWidth={1.5} />
              )}
              Choose local file
            </button>
          )}
        </span>
      </label>

      <div className="mt-4 grid grid-cols-1 gap-3 sm:grid-cols-2">
        <section className="rounded-lg border border-gray-200 bg-gray-50 px-3 py-3">
          <h4 className="text-xs font-medium text-gray-950">Setup preview</h4>
          <ul className="mt-2 space-y-1 text-xs text-gray-600">
            {family.setupRequirements.map((requirement) => (
              <li className="flex gap-2" key={requirement}>
                <CheckIcon
                  className="mt-0.5 shrink-0 text-green-600"
                  size={12}
                />
                <span>{requirement}</span>
              </li>
            ))}
          </ul>
        </section>
        <SqlPreview sql={generatedSqlForFamily(family, location)} />
      </div>
    </div>
  );
}

function GenericAuthStep({ family }: { family: DatasourceSourceFamily }) {
  const authDetail = authDetailForFamily(family);

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">
        Auth for {family.label.toLowerCase()}
      </h3>
      <p className="mt-1 text-sm text-gray-500">
        Configure credentials only when this source needs DuckDB secrets,
        extension auth, or a database login.
      </p>
      <div className="mt-4 rounded-lg border border-gray-200 bg-gray-50">
        <SummaryRow label="Auth">{authDetail.title}</SummaryRow>
        <SummaryRow label="Mechanism">{family.duckDbMechanism}</SummaryRow>
        <SummaryRow label="Scope">{authDetail.scope}</SummaryRow>
      </div>
    </div>
  );
}

function GenericInspectStep({ family }: { family: DatasourceSourceFamily }) {
  const objectName = previewObjectNameForFamily(family);

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">
        Inspect {family.label.toLowerCase()}
      </h3>
      <p className="mt-1 text-sm text-gray-500">
        Preview the object DuckDB will expose and the first columns the notebook
        can query after attach.
      </p>
      <div className="mt-4 space-y-3">
        <article className="rounded-lg border border-gray-200 bg-white px-3 py-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <h4 className="truncate font-mono text-sm font-medium text-gray-950">
                {objectName}
              </h4>
              <p className="mt-1 text-xs text-gray-500">
                {inspectDetailForFamily(family)}
              </p>
            </div>
            <span className="shrink-0 rounded bg-gray-100 px-2 py-1 text-[11px] font-medium text-gray-600">
              preview
            </span>
          </div>
        </article>
        <div className="grid grid-cols-3 gap-2 text-xs">
          {["id", "created_at", "payload"].map((column) => (
            <div
              className="rounded border border-gray-200 bg-gray-50 px-2 py-2 font-mono text-gray-700"
              key={`${family.key}:${column}`}
            >
              {column}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function GenericAttachStep({
  error,
  family,
  location,
}: {
  error: string | null;
  family: DatasourceSourceFamily;
  location: string;
}) {
  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">
        Attach {family.label.toLowerCase()}
      </h3>
      <p className="mt-1 text-sm text-gray-500">
        Review the DuckDB attachment shape before running it in the notebook.
      </p>
      <div className="mt-4 rounded-lg border border-gray-200 bg-gray-50">
        <SummaryRow label="Family">{family.label}</SummaryRow>
        <SummaryRow label="Location">{location}</SummaryRow>
        <SummaryRow label="Object">
          {previewObjectNameForFamily(family)}
        </SummaryRow>
      </div>
      <div className="mt-4">
        <SqlPreview sql={generatedSqlForFamily(family, location)} />
      </div>
      {error && <ErrorBanner message={error} />}
    </div>
  );
}

function SqlPreview({ sql }: { sql: string }) {
  return (
    <section className="overflow-hidden rounded-lg border border-gray-200 bg-gray-950">
      <div className="border-b border-gray-800 px-3 py-2">
        <h4 className="text-xs font-medium text-white">Generated SQL</h4>
      </div>
      <pre className="overflow-x-auto whitespace-pre-wrap px-3 py-3 font-mono text-[11px] leading-5 text-gray-100">
        {sql}
      </pre>
    </section>
  );
}

function ProviderTile({
  onSelect,
  provider,
  selected,
}: {
  onSelect: (provider: ProviderSummary) => void;
  provider: ProviderSummary;
  selected: boolean;
}) {
  const tier = normalizedTier(provider.tier);
  const tableCount = provider.tables.length;
  const actionCount = provider.actions?.length ?? 0;
  const status = providerFulfillmentStatus(provider);
  const blocked = status === "Blocked";

  return (
    <button
      aria-disabled={blocked}
      className={clsx(
        "min-w-0 rounded-lg border bg-white p-2.5 text-left transition-colors disabled:cursor-not-allowed disabled:bg-gray-50 disabled:opacity-75",
        selected
          ? "border-indigo-600 bg-indigo-50 ring-1 ring-indigo-600"
          : blocked
            ? "border-gray-200"
            : "border-gray-200 hover:border-indigo-300",
      )}
      disabled={blocked}
      onClick={() => onSelect(provider)}
      type="button"
    >
      <span className="flex items-center gap-2">
        <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded bg-gray-900 text-[10px] font-semibold text-white">
          {provider.displayName.charAt(0).toUpperCase()}
        </span>
        <span className="truncate text-xs font-medium text-gray-950">
          {provider.displayName}
        </span>
      </span>
      <span className="mt-2 flex items-center justify-between gap-2">
        <span className="truncate text-[11px] text-gray-400">
          {provider.category}
        </span>
        <ProviderStatusBadge status={status} tier={tier} />
      </span>
      {(status === "Candidate" || status === "Blocked") &&
        provider.experimentalSpecCount > 0 && (
          <span
            className={clsx(
              "mt-2 block truncate text-[11px] font-medium",
              status === "Blocked" ? "text-red-700" : "text-amber-700",
            )}
          >
            {provider.experimentalSpecCount} spec candidate
            {provider.experimentalSpecCount === 1 ? "" : "s"}
          </span>
        )}
      {status === "Blocked" && provider.blockedReason && (
        <span className="mt-2 block truncate text-[11px] font-medium text-red-700">
          {provider.blockedReason}
        </span>
      )}
      {tableCount > 0 && (
        <span className="mt-2 block truncate text-[11px] font-medium text-green-700">
          {tableCount} table-function{tableCount === 1 ? "" : "s"}
        </span>
      )}
      {actionCount > 0 && (
        <span className="mt-2 block truncate text-[11px] font-medium text-blue-700">
          {actionCount} action-function{actionCount === 1 ? "" : "s"}
        </span>
      )}
    </button>
  );
}

function SavedConnectionTile({
  connection,
  onSelect,
  selected,
}: {
  connection: ConnectionTemplate;
  onSelect: (connection: ConnectionTemplate) => void;
  selected: boolean;
}) {
  const provider = connection.provider ?? "custom";
  const category = connection.group ?? provider;
  const tableCount = connection.tables.length;
  const credentialCount = connection.credentialEnvVars.length;

  return (
    <button
      aria-pressed={selected}
      className={clsx(
        "min-w-0 rounded-lg border bg-white p-3 text-left transition-colors",
        selected
          ? "border-indigo-600 bg-indigo-50 ring-1 ring-indigo-600"
          : "border-gray-200 hover:border-indigo-300",
      )}
      onClick={() => onSelect(connection)}
      type="button"
    >
      <span className="flex items-center gap-2">
        <span className="inline-flex h-7 w-7 shrink-0 items-center justify-center rounded bg-gray-900 text-[11px] font-semibold text-white">
          {provider.charAt(0).toUpperCase()}
        </span>
        <span className="min-w-0">
          <span className="block truncate text-sm font-medium text-gray-950">
            {connection.name}
          </span>
          <span className="mt-0.5 block truncate text-[11px] text-gray-400">
            {category} · {tableCount} table-function
            {tableCount === 1 ? "" : "s"}
          </span>
        </span>
      </span>
      <span className="mt-3 inline-flex rounded-full border border-indigo-200 bg-indigo-50 px-2 py-1 text-[11px] font-medium text-indigo-700">
        {credentialCount === 0
          ? "No credentials"
          : `${credentialCount} credential${credentialCount === 1 ? "" : "s"}`}
      </span>
    </button>
  );
}

function SavedCredentialPrompt({
  credentials,
  credentialKeys,
  onCredentialChange,
}: {
  credentials: Record<string, string>;
  credentialKeys: string[];
  onCredentialChange: (key: string, value: string) => void;
}) {
  const fields = credentialKeys.map(
    (key): CredentialField => ({ key, label: key, type: "password" }),
  );

  return (
    <section className="mt-4 rounded-lg border border-amber-200 bg-amber-50 px-3 py-3">
      <div className="flex items-start gap-2 text-sm text-amber-800">
        <AlertCircleIcon className="mt-0.5 shrink-0" size={14} />
        <div>
          <h4 className="font-medium">Missing credentials</h4>
          <p className="mt-0.5">
            Add the required session values before attaching this connection.
          </p>
        </div>
      </div>
      <div className="mt-3 space-y-3">
        <CredentialFieldInputs
          credentials={credentials}
          fields={fields}
          onCredentialChange={onCredentialChange}
        />
      </div>
    </section>
  );
}

function CredentialFieldInputs({
  credentials,
  fields,
  onCredentialChange,
}: {
  credentials: Record<string, string>;
  fields: CredentialField[];
  onCredentialChange: (key: string, value: string) => void;
}) {
  return (
    <>
      {fields.map((field) => (
        <label className="block" key={field.key}>
          <span className="text-xs font-medium text-gray-600">
            {field.label}
          </span>
          <input
            aria-label={field.key}
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 font-mono text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            onChange={(event) =>
              onCredentialChange(field.key, event.currentTarget.value)
            }
            placeholder={field.key}
            type={field.type}
            value={credentials[field.key] ?? ""}
          />
          <span className="mt-1 block font-mono text-[11px] text-gray-400">
            env {field.key}
          </span>
        </label>
      ))}
    </>
  );
}

function ConnectStep({
  assistantPrefillMode,
  credentials,
  datasourceName,
  fields,
  nameReadOnly,
  onCredentialChange,
  onDatasourceNameChange,
  selectedProvider,
  selectedSavedConnection,
  sourceMode,
}: {
  assistantPrefillMode: boolean;
  credentials: Record<string, string>;
  datasourceName: string;
  fields: CredentialField[];
  nameReadOnly: boolean;
  onCredentialChange: (key: string, value: string) => void;
  onDatasourceNameChange: (value: string) => void;
  selectedProvider: ProviderSummary | null;
  selectedSavedConnection: ConnectionTemplate | null;
  sourceMode: SourceMode | null;
}) {
  const authScheme = selectedProvider
    ? authSchemeLabel(selectedProvider.authMode)
    : "API key or bearer token";
  const baseUrl = selectedProvider
    ? baseUrlForProvider(selectedProvider)
    : "https://api.example.com";
  const tier = normalizedTier(selectedProvider?.tier);
  const supportedProvider = selectedProvider
    ? isSupportedProvider(selectedProvider)
    : false;
  const heading = selectedProvider
    ? `Connect to ${selectedProvider.displayName}`
    : selectedSavedConnection?.provider
      ? `Connect to ${displayNameFromProvider(selectedSavedConnection.provider)}`
      : "Connect a custom REST API";

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">{heading}</h3>
      <p className="mt-1 text-sm text-gray-500">
        Auth, base URL, and credentials are passed to the daemon so it can build
        the datasource manifest.
      </p>

      {assistantPrefillMode && (
        <div className="mt-4 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800">
          <div className="flex items-start gap-2">
            <AlertCircleIcon className="mt-0.5 shrink-0" size={14} />
            <p>
              Finishing a connection started by the assistant - add the missing
              credentials and it will be saved automatically.
            </p>
          </div>
        </div>
      )}

      <div
        className={clsx(
          "mt-4 rounded-lg border px-3 py-2 text-sm",
          tier === "A"
            ? "border-green-200 bg-green-50 text-green-800"
            : "border-amber-200 bg-amber-50 text-amber-800",
        )}
      >
        <div className="flex items-start gap-2">
          {tier === "A" ? (
            <CheckIcon className="mt-0.5 shrink-0" size={14} />
          ) : (
            <AlertCircleIcon className="mt-0.5 shrink-0" size={14} />
          )}
          <p>
            <span className="font-medium">
              {supportedProvider
                ? tier === "A"
                  ? "Tables ready. Drop-in."
                  : "Tables ready."
                : tier === "A"
                  ? "Drop-in."
                  : "Bring your own token."}
            </span>{" "}
            {supportedProvider
              ? "This provider has a bundled manifest with table-functions; add credentials and continue."
              : tier === "A"
                ? "Paste the credential once; it stays in session state until the daemon receives it."
                : "Generate a token with the provider and paste it here for this session."}
          </p>
        </div>
      </div>

      <div className="mt-4 rounded-lg border border-gray-200 bg-gray-50">
        <SummaryRow label="Provider">
          {selectedProvider
            ? `${selectedProvider.displayName} · ${selectedProvider.category}`
            : selectedSavedConnection?.provider
              ? displayNameFromProvider(selectedSavedConnection.provider)
              : sourceMode === "manual"
                ? "Manual"
                : "OpenAPI spec"}
        </SummaryRow>
        <SummaryRow label="Auth scheme">{authScheme}</SummaryRow>
        <SummaryRow label="Base URL">{baseUrl}</SummaryRow>
      </div>

      <div className="mt-4 space-y-3">
        <label className="block">
          <span className="text-xs font-medium text-gray-600">
            Datasource name
          </span>
          <input
            aria-label="Datasource name"
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600 disabled:bg-gray-50 disabled:text-gray-500"
            disabled={nameReadOnly}
            onChange={(event) =>
              onDatasourceNameChange(event.currentTarget.value)
            }
            placeholder="stripe_reporting"
            value={datasourceName}
          />
        </label>

        {fields.length > 0 ? (
          <CredentialFieldInputs
            credentials={credentials}
            fields={fields}
            onCredentialChange={onCredentialChange}
          />
        ) : (
          <div className="rounded border border-gray-200 bg-gray-50 px-3 py-2 text-sm text-gray-500">
            Add credentials later if this API requires them.
          </div>
        )}
      </div>
    </div>
  );
}

function TablesStep({
  connectionOnly,
  error,
  onConnectionOnlyChange,
  onPreviewTables,
  onSpecTextChange,
  pendingPreview,
  selectedProvider,
  specText,
  tablePreview,
}: {
  connectionOnly: boolean;
  error: string | null;
  onConnectionOnlyChange: (checked: boolean) => void;
  onPreviewTables: () => void;
  onSpecTextChange: (value: string) => void;
  pendingPreview: boolean;
  selectedProvider: ProviderSummary | null;
  specText: string;
  tablePreview: OpenApiTablePreview | null;
}) {
  const providerName = selectedProvider?.displayName ?? "API";
  const catalogTablesReady =
    selectedProvider !== null &&
    isSupportedProvider(selectedProvider) &&
    tablePreview !== null &&
    specText.trim().length === 0;
  const catalogActionsReady =
    selectedProvider !== null &&
    isSupportedProvider(selectedProvider) &&
    (selectedProvider.actions?.length ?? 0) > 0 &&
    tablePreview === null &&
    specText.trim().length === 0;

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">Tables</h3>
      <p className="mt-1 text-sm text-gray-500">
        {catalogTablesReady
          ? "Bundled provider tables are ready. Review the table-functions before adding the datasource."
          : catalogActionsReady
            ? "Bundled provider actions are ready. Review the action-functions before adding the datasource."
            : "Preview generated table-functions from an OpenAPI spec, or add the connection now and define tables later."}
      </p>

      {!catalogTablesReady && !catalogActionsReady && (
        <fieldset className="mt-4">
          <legend className="text-xs font-medium text-gray-600">
            Datasource mode
          </legend>
          <div className="mt-2 grid grid-cols-1 gap-2 sm:grid-cols-2">
            <label
              className={clsx(
                "flex min-h-24 cursor-pointer gap-3 rounded border bg-white px-3 py-3 text-sm transition-colors",
                !connectionOnly
                  ? "border-indigo-500 ring-1 ring-indigo-200"
                  : "border-gray-200 hover:border-gray-300",
              )}
            >
              <input
                aria-label="Generate tables from OpenAPI"
                checked={!connectionOnly}
                className="mt-0.5 h-4 w-4 border-gray-300 text-indigo-600"
                name="rest-api-table-mode"
                onChange={() => onConnectionOnlyChange(false)}
                type="radio"
              />
              <span className="min-w-0">
                <span className="block font-medium text-gray-950">
                  Generate tables from OpenAPI
                </span>
                <span className="mt-1 block text-xs leading-5 text-gray-500">
                  Paste a spec or URL, preview generated table-functions, then
                  add them with this connection.
                </span>
              </span>
            </label>
            <label
              className={clsx(
                "flex min-h-24 cursor-pointer gap-3 rounded border bg-white px-3 py-3 text-sm transition-colors",
                connectionOnly
                  ? "border-indigo-500 ring-1 ring-indigo-200"
                  : "border-gray-200 hover:border-gray-300",
              )}
            >
              <input
                aria-label="Connection only"
                checked={connectionOnly}
                className="mt-0.5 h-4 w-4 border-gray-300 text-indigo-600"
                name="rest-api-table-mode"
                onChange={() => onConnectionOnlyChange(true)}
                type="radio"
              />
              <span className="min-w-0">
                <span className="block font-medium text-gray-950">
                  Connection only
                </span>
                <span className="mt-1 block text-xs leading-5 text-gray-500">
                  Save the connection without importing table definitions.
                </span>
              </span>
            </label>
          </div>
        </fieldset>
      )}

      {!catalogTablesReady && !catalogActionsReady && (
        <label className="mt-4 block">
          <span className="text-xs font-medium text-gray-600">
            OpenAPI spec text or URL
          </span>
          <textarea
            aria-label="OpenAPI spec text or URL"
            className="mt-1 min-h-28 w-full resize-y rounded border border-gray-300 bg-white px-3 py-2 font-mono text-xs text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            disabled={connectionOnly}
            onChange={(event) => onSpecTextChange(event.currentTarget.value)}
            placeholder={`${providerName.toLowerCase()}.openapi.json or https://...`}
            value={specText}
          />
        </label>
      )}

      {!catalogTablesReady && !catalogActionsReady && (
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button
            className="inline-flex items-center gap-2 rounded border border-gray-300 bg-white px-3 py-2 text-sm font-medium text-gray-700 transition-colors hover:border-gray-950 hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
            disabled={connectionOnly || pendingPreview}
            onClick={onPreviewTables}
            type="button"
          >
            {pendingPreview ? (
              <Loader2Icon className="animate-spin" size={14} />
            ) : (
              <TableIcon size={14} strokeWidth={1.5} />
            )}
            Preview tables
          </button>
        </div>
      )}

      {connectionOnly && !catalogTablesReady && !catalogActionsReady && (
        <div className="mt-3 flex gap-2 rounded border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-900">
          <AlertCircleIcon className="mt-0.5 shrink-0" size={15} />
          <p>
            No tables will be created now. You can define table-functions later
            from this saved connection.
          </p>
        </div>
      )}

      {error && <ErrorBanner message={error} />}

      {tablePreview && !connectionOnly && (
        <div className="mt-4 space-y-4">
          <section>
            <div className="mb-2 flex items-center justify-between gap-2">
              <h4 className="text-xs font-medium text-gray-950">
                Generated tables
              </h4>
              <span className="text-xs text-gray-400">
                {tablePreview.tables.length} table
                {tablePreview.tables.length === 1 ? "" : "s"}
              </span>
            </div>
            <div className="space-y-2">
              {tablePreview.tables.map((table) => (
                <TablePreviewRow
                  key={`${table.name}:${table.path}`}
                  table={table}
                />
              ))}
            </div>
          </section>

          <FlattenPreview table={tablePreview.tables[0] ?? null} />
        </div>
      )}
      {catalogActionsReady && (
        <div className="mt-4 space-y-4">
          <section>
            <div className="mb-2 flex items-center justify-between gap-2">
              <h4 className="text-xs font-medium text-gray-950">
                Bundled actions
              </h4>
              <span className="text-xs text-gray-400">
                {selectedProvider.actions.length} action
                {selectedProvider.actions.length === 1 ? "" : "s"}
              </span>
            </div>
            <div className="space-y-2">
              {selectedProvider.actions.map((action) => (
                <TablePreviewRow
                  key={`action:${action.name}:${action.path}`}
                  table={action}
                />
              ))}
            </div>
          </section>
        </div>
      )}
    </div>
  );
}

function TablePreviewRow({ table }: { table: TablePreview }) {
  return (
    <article className="rounded border border-gray-200 bg-white px-3 py-2">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h5 className="truncate font-mono text-xs font-medium text-gray-950">
            {table.name}
          </h5>
          <p className="mt-0.5 font-mono text-[11px] text-gray-400">
            {table.method} {table.path}
          </p>
        </div>
        <span className="shrink-0 text-xs text-gray-400">
          {table.columns.length} cols
        </span>
      </div>
    </article>
  );
}

function FlattenPreview({ table }: { table: TablePreview | null }) {
  if (!table) return null;

  return (
    <section className="overflow-hidden rounded-lg border border-gray-200">
      <div className="flex items-center justify-between gap-2 border-b border-gray-200 bg-gray-50 px-3 py-2">
        <h4 className="text-xs font-medium text-gray-950">Flatten preview</h4>
        <span className="font-mono text-[11px] text-gray-400">
          nested JSON to columns
        </span>
      </div>
      <div className="grid grid-cols-1 divide-y divide-gray-200 sm:grid-cols-[1fr_1.15fr] sm:divide-x sm:divide-y-0">
        <div className="bg-gray-50 px-3 py-3">
          <pre className="whitespace-pre-wrap font-mono text-[11px] leading-5 text-gray-600">
            {`{
  "id": "item_123",
  "billing_details": {
    "address": {
      "city": "Berlin"
    }
  },
  "metadata": { "...": "..." }
}`}
          </pre>
        </div>
        <div className="px-3 py-3">
          <div className="space-y-1">
            {table.columns.slice(0, 8).map((column) => (
              <div
                className="grid grid-cols-[minmax(0,1fr)_auto] gap-2 font-mono text-[11px]"
                key={`${table.name}:${column.name}:flatten`}
              >
                <span className="truncate text-gray-800">{column.name}</span>
                <span className="text-indigo-600">{column.ty}</span>
                <span className="col-span-2 truncate text-gray-400">
                  {column.json}
                </span>
              </div>
            ))}
          </div>
          {table.responsePath && (
            <p className="mt-3 font-mono text-[11px] text-gray-400">
              response_path = {table.responsePath}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}

function ReviewStep({
  connectionOnly,
  credentialFields,
  credentials,
  datasourceName,
  error,
  selectedProvider,
  specText,
  tablePreview,
}: {
  connectionOnly: boolean;
  credentialFields: CredentialField[];
  credentials: Record<string, string>;
  datasourceName: string;
  error: string | null;
  selectedProvider: ProviderSummary | null;
  specText: string;
  tablePreview: OpenApiTablePreview | null;
}) {
  const credentialCount = credentialFields.filter(
    (field) => (credentials[field.key]?.trim() ?? "").length > 0,
  ).length;
  const tableCount = connectionOnly ? 0 : (tablePreview?.tables.length ?? 0);
  const actionCount =
    selectedProvider && isSupportedProvider(selectedProvider)
      ? (selectedProvider.actions?.length ?? 0)
      : 0;

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">Review</h3>
      <p className="mt-1 text-sm text-gray-500">
        The daemon builds the manifest from this import payload and registers an
        api_tables datasource.
      </p>

      <div className="mt-4 rounded-lg border border-gray-200 bg-gray-50">
        <SummaryRow label="Name">{datasourceName.trim()}</SummaryRow>
        <SummaryRow label="Provider">
          {selectedProvider?.displayName ?? "Custom REST API"}
        </SummaryRow>
        <SummaryRow label="Credentials">
          {credentialCount === 0
            ? "none"
            : credentialFields
                .filter(
                  (field) => (credentials[field.key]?.trim() ?? "").length > 0,
                )
                .map((field) => `env ${field.key}`)
                .join(", ")}
        </SummaryRow>
        <SummaryRow label="Spec">
          {selectedProvider && isSupportedProvider(selectedProvider)
            ? "bundled provider manifest"
            : connectionOnly || specText.trim().length === 0
              ? "connection only"
              : "OpenAPI spec attached"}
        </SummaryRow>
        <SummaryRow label="Tables">
          {tableCount === 0
            ? "none yet"
            : `${tableCount} table-function${tableCount === 1 ? "" : "s"}`}
        </SummaryRow>
        {actionCount > 0 && (
          <SummaryRow label="Actions">
            {actionCount} action-function{actionCount === 1 ? "" : "s"}
          </SummaryRow>
        )}
      </div>

      {tablePreview && !connectionOnly && (
        <div className="mt-4 space-y-2">
          {tablePreview.tables.map((table) => (
            <div
              className="flex items-center justify-between gap-3 rounded border border-gray-200 bg-white px-3 py-2"
              key={`review:${table.name}`}
            >
              <span className="truncate font-mono text-xs text-gray-950">
                {table.name}
              </span>
              <span className="shrink-0 text-xs text-gray-400">
                {table.columns.length} cols
              </span>
            </div>
          ))}
        </div>
      )}

      {error && <ErrorBanner message={error} />}
    </div>
  );
}

function RssAttachStep({
  datasourceName,
  error,
  onDatasourceNameChange,
}: {
  datasourceName: string;
  error: string | null;
  onDatasourceNameChange: (value: string) => void;
}) {
  const [sourceMode, setSourceMode] = useState<RssSourceMode>("rsshub");
  const [sourceInput, setSourceInput] = useState(
    "rsshub://youtube/channel/UCYO_jab_esuFRV4b17AJtAw",
  );
  const [sourceHandleOverride, setSourceHandleOverride] = useState<
    string | null
  >(null);
  const sourceStatusId = useId();
  const sourceModeDetailId = useId();
  const [selectedRouteId, setSelectedRouteId] = useState(
    RSSHUB_EXAMPLE_ROUTES[0]?.id ?? "",
  );
  const [routeParams, setRouteParams] = useState<Record<string, string>>(() =>
    defaultRssRouteParams(RSSHUB_EXAMPLE_ROUTES[0]),
  );

  const selectedRoute =
    RSSHUB_EXAMPLE_ROUTES.find((route) => route.id === selectedRouteId) ??
    RSSHUB_EXAMPLE_ROUTES[0];
  const generatedRssHubUrl = useMemo(
    () => rssHubUrlForRoute(selectedRoute, routeParams),
    [routeParams, selectedRoute],
  );
  const classification = classifyRssInput(sourceInput);
  const derivedSourceHandle = useMemo(
    () =>
      rssSourceHandleForSelection({
        input: sourceInput,
        mode: sourceMode,
        route: selectedRoute,
        routeParams,
      }),
    [routeParams, selectedRoute, sourceInput, sourceMode],
  );
  const sourceHandle = sourceHandleOverride ?? derivedSourceHandle;
  const modeDetail =
    sourceMode === "keyword"
      ? "Searches seeded RSSHub routes for the MVP. Live rss_routes browsing can replace this list later."
      : sourceMode === "direct"
        ? "Direct URLs map straight into rss_feed(url) and rss_entries(url)."
        : "Route parameters generate a canonical rsshub:// URL.";
  const queryUrl =
    classification.mode === "direct" || classification.mode === "rsshub"
      ? sourceInput.trim()
      : generatedRssHubUrl;
  const queryUrlLiteral = sqlStringLiteral(queryUrl || generatedRssHubUrl);
  const feedQuery = `select * from rss_feed(${queryUrlLiteral});`;
  const entriesQuery = `select * from rss_entries(${queryUrlLiteral});`;
  const entriesViewName = `${sourceHandle}_entries`;
  const createEntriesViewSql = [
    `create or replace view ${entriesViewName} as`,
    `select * from rss_entries(${queryUrlLiteral});`,
  ].join("\n");
  const friendlyEntriesQuery = [
    `select * from ${entriesViewName}`,
    "order by published_at desc;",
  ].join("\n");
  const queryTemplates = [
    {
      label: "Friendly entries query",
      sql: friendlyEntriesQuery,
    },
    {
      label: "Generated DuckDB view over rss_entries(url)",
      sql: createEntriesViewSql,
    },
    {
      label: "Raw backend mapping",
      sql: entriesQuery,
    },
    {
      label: "Routes",
      sql: "select * from rss_routes();",
    },
    {
      label: "Feed metadata",
      sql: feedQuery,
    },
    {
      label: "Feed entries",
      sql: entriesQuery,
    },
  ];

  const selectMode = (mode: RssSourceMode) => {
    setSourceMode(mode);
    setSourceHandleOverride(null);
    if (mode === "direct") {
      setSourceInput("https://example.com/feed.xml");
    } else if (mode === "keyword") {
      setSourceInput("rust async release notes");
    } else {
      setSourceInput(generatedRssHubUrl);
    }
  };

  const selectRoute = (route: RssHubRoute) => {
    const nextParams = defaultRssRouteParams(route);
    setSelectedRouteId(route.id);
    setRouteParams(nextParams);
    setSourceMode("rsshub");
    setSourceInput(rssHubUrlForRoute(route, nextParams));
    setSourceHandleOverride(null);
  };

  const updateRouteParam = (key: string, value: string) => {
    const nextParams = { ...routeParams, [key]: value };
    setRouteParams(nextParams);
    setSourceInput(rssHubUrlForRoute(selectedRoute, nextParams));
  };

  return (
    <div>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h3 className="text-base font-semibold text-gray-950">
            Add RSS / RSSHub
          </h3>
          <p className="mt-1 text-sm text-gray-500">
            Register the built-in RSS datasource, classify the source input,
            and prepare the feed query shape for direct RSS URLs or RSSHub
            routes.
          </p>
        </div>
        <label className="w-full sm:w-56">
          <span className="text-xs font-medium text-gray-600">
            Datasource name
          </span>
          <input
            aria-label="Datasource name"
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            onChange={(event) =>
              onDatasourceNameChange(event.currentTarget.value)
            }
            placeholder="rss"
            value={datasourceName}
          />
        </label>
      </div>

      <section className="mt-4 rounded-lg border border-gray-200 bg-gray-50 p-3">
        <div
          aria-label="RSS source mode"
          className="grid grid-cols-1 gap-2 sm:grid-cols-3"
          role="group"
        >
          {RSS_SOURCE_MODES.map((mode) => (
            <button
              aria-label={mode.label}
              aria-pressed={sourceMode === mode.key}
              className={clsx(
                "rounded border bg-white px-3 py-2 text-left transition-colors",
                sourceMode === mode.key
                  ? "border-indigo-600 bg-indigo-50 ring-1 ring-indigo-600"
                  : "border-gray-200 hover:border-indigo-300",
              )}
              key={mode.key}
              onClick={() => selectMode(mode.key)}
              type="button"
            >
              <span className="block text-xs font-medium text-gray-950">
                {mode.label}
              </span>
              <span className="mt-1 block text-[11px] leading-4 text-gray-500">
                {mode.detail}
              </span>
            </button>
          ))}
        </div>

        <label className="mt-3 block">
          <span className="text-xs font-medium text-gray-600">
            Source URL or keyword
          </span>
          <input
            aria-describedby={`${sourceStatusId} ${sourceModeDetailId}`}
            aria-label="Source URL or keyword"
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 font-mono text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            onChange={(event) => {
              setSourceInput(event.currentTarget.value);
              setSourceHandleOverride(null);
            }}
            value={sourceInput}
          />
        </label>

        <label className="mt-3 block">
          <span className="text-xs font-medium text-gray-600">
            Friendly view name
          </span>
          <input
            aria-label="Friendly view name"
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 font-mono text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            onChange={(event) =>
              setSourceHandleOverride(
                rssSourceHandleFromParts([event.currentTarget.value]),
              )
            }
            value={sourceHandle}
          />
          <span className="mt-1 block text-[11px] leading-4 text-gray-500">
            Used only to name the generated DuckDB view; it does not register a
            dynamic backend table-function.
          </span>
        </label>

        <div className="mt-3 grid grid-cols-1 gap-2 text-xs sm:grid-cols-3">
          {(["direct", "rsshub", "keyword"] as const).map((kind) => (
            <div
              className={clsx(
                "rounded border px-2 py-2",
                classification.mode === kind
                  ? "border-green-200 bg-green-50 text-green-800"
                  : "border-gray-200 bg-white text-gray-500",
              )}
              key={kind}
            >
              <div className="font-medium">{rssClassificationLabel(kind)}</div>
              <div className="mt-0.5 leading-4">
                {rssClassificationDetail(kind)}
              </div>
            </div>
          ))}
        </div>
        <p
          aria-atomic="true"
          className="mt-2 text-xs font-medium text-gray-700"
          id={sourceStatusId}
          role="status"
        >
          Detected: {classification.label}
        </p>
        <p className="mt-1 text-xs text-gray-500" id={sourceModeDetailId}>
          {modeDetail}
        </p>
      </section>

      <div className="mt-4 grid grid-cols-1 gap-3 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
        <section className="overflow-hidden rounded-lg border border-gray-200 bg-white">
          <div className="border-b border-gray-200 bg-gray-50 px-3 py-2">
            <h4 className="text-xs font-medium text-gray-950">
              RSSHub route cards
            </h4>
          </div>
          <div className="max-h-64 space-y-2 overflow-y-auto p-3">
            {RSSHUB_EXAMPLE_ROUTES.map((route) => (
              <button
                aria-label={[
                  `Select ${route.name} RSSHub route`,
                  route.category,
                  `${route.heat} heat`,
                ].join(", ")}
                aria-pressed={selectedRoute.id === route.id}
                className={clsx(
                  "w-full rounded-lg border bg-white px-3 py-2 text-left transition-colors",
                  selectedRoute.id === route.id
                    ? "border-indigo-600 bg-indigo-50 ring-1 ring-indigo-600"
                    : "border-gray-200 hover:border-indigo-300",
                )}
                key={route.id}
                onClick={() => selectRoute(route)}
                type="button"
              >
                <span className="flex items-start justify-between gap-2">
                  <span className="min-w-0">
                    <span className="block text-sm font-medium text-gray-950">
                      {route.name}
                    </span>
                    <span className="mt-0.5 block break-all font-mono text-[11px] text-gray-500">
                      {route.template}
                    </span>
                  </span>
                  <span className="shrink-0 rounded bg-amber-50 px-2 py-1 text-[11px] font-medium text-amber-700">
                    {route.heat}
                  </span>
                </span>
                <span className="mt-2 flex flex-wrap gap-1.5">
                  <span className="rounded-full border border-gray-200 bg-white px-2 py-0.5 text-[10px] font-medium text-gray-600">
                    {route.category}
                  </span>
                  {route.tags.map((tag) => (
                    <span
                      className="rounded-full border border-gray-200 bg-white px-2 py-0.5 text-[10px] text-gray-500"
                      key={`${route.id}:${tag}`}
                    >
                      {tag}
                    </span>
                  ))}
                </span>
              </button>
            ))}
          </div>
        </section>

        <section className="space-y-3">
          <div className="rounded-lg border border-gray-200 bg-white">
            <div className="border-b border-gray-200 bg-gray-50 px-3 py-2">
              <h4 className="text-xs font-medium text-gray-950">
                Route setup
              </h4>
            </div>
            <div className="space-y-3 px-3 py-3">
              <div className="rounded border border-gray-200 bg-gray-50 px-3 py-2">
                <div className="text-[11px] font-medium uppercase text-gray-400">
                  Template
                </div>
                <div className="mt-1 break-all font-mono text-xs text-gray-950">
                  {selectedRoute.template}
                </div>
              </div>
              {selectedRoute.parameters.length === 0 ? (
                <div className="rounded border border-gray-200 bg-gray-50 px-3 py-2 text-xs text-gray-500">
                  This RSSHub route has no parameters.
                </div>
              ) : (
                selectedRoute.parameters.map((parameter) => (
                  <label className="block" key={parameter.key}>
                    <span className="text-xs font-medium text-gray-600">
                      Parameter: {parameter.key}
                    </span>
                    <input
                      aria-label={`Parameter: ${parameter.key}`}
                      className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 font-mono text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
                      onChange={(event) =>
                        updateRouteParam(
                          parameter.key,
                          event.currentTarget.value,
                        )
                      }
                      value={routeParams[parameter.key] ?? ""}
                    />
                    <span className="mt-1 block text-[11px] text-gray-400">
                      {parameter.label}
                    </span>
                  </label>
                ))
              )}
              <div>
                <div className="text-xs font-medium text-gray-600">
                  Generated feed URL
                </div>
                <div className="mt-1 break-all rounded border border-gray-200 bg-gray-950 px-3 py-2 font-mono text-xs leading-5 text-white">
                  {generatedRssHubUrl}
                </div>
              </div>
            </div>
          </div>

          <div className="rounded-lg border border-gray-200 bg-white">
            <div className="flex items-center justify-between gap-2 border-b border-gray-200 bg-gray-50 px-3 py-2">
              <h4 className="text-xs font-medium text-gray-950">
                Feed status
              </h4>
              <span className="rounded bg-green-50 px-2 py-1 text-[11px] font-medium text-green-700">
                Preview ready
              </span>
            </div>
            <div className="px-3 py-3">
              <div className="flex gap-3">
                <span className="inline-flex h-10 w-10 shrink-0 items-center justify-center rounded bg-orange-100 text-xs font-semibold text-orange-700">
                  RSS
                </span>
                <div className="min-w-0">
                  <div className="text-sm font-medium text-gray-950">
                    {selectedRoute.previewTitle}
                  </div>
                  <p className="mt-1 text-xs leading-5 text-gray-500">
                    {selectedRoute.previewDescription}
                  </p>
                </div>
              </div>
              <div className="mt-3 divide-y divide-gray-200 rounded border border-gray-200">
                {selectedRoute.examples.map((entry) => (
                  <div className="bg-white px-3 py-2" key={entry.title}>
                    <div className="text-xs font-medium text-gray-950">
                      {entry.title}
                    </div>
                    <div className="mt-0.5 text-[11px] text-gray-500">
                      {entry.meta}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          </div>
        </section>
      </div>

      <div className="mt-4 grid grid-cols-1 gap-3 lg:grid-cols-2">
        <section className="rounded-lg border border-gray-200 bg-gray-50">
          <div className="border-b border-gray-200 px-3 py-2">
            <h4 className="text-xs font-medium text-gray-950">
              Source registration
            </h4>
          </div>
          <SummaryRow label="Feed">{selectedRoute.previewTitle}</SummaryRow>
          <SummaryRow label="Source">{classification.label}</SummaryRow>
          <SummaryRow label="Friendly view name">{entriesViewName}</SummaryRow>
          <SummaryRow label="Entry function">rss_entries(url)</SummaryRow>
          <SummaryRow label="Entry query">{friendlyEntriesQuery}</SummaryRow>
          <SummaryRow label="View">{selectedRoute.view}</SummaryRow>
          <SummaryRow label="Folder">{selectedRoute.category}</SummaryRow>
          <SummaryRow label="Next action">create view, then query entries</SummaryRow>
        </section>

        <section className="rounded-lg border border-gray-200 bg-gray-50">
          <div className="border-b border-gray-200 px-3 py-2">
            <h4 className="text-xs font-medium text-gray-950">
              Raw backend mapping
            </h4>
          </div>
          <SummaryRow label="Attach">add_api_datasource source = rss</SummaryRow>
          <SummaryRow label="Discovery">rss_routes</SummaryRow>
          <SummaryRow label="Preview">rss_feed(url)</SummaryRow>
          <SummaryRow label="Entries">rss_entries(url)</SummaryRow>
          <SummaryRow label="Mapping">{entriesQuery}</SummaryRow>
          <SummaryRow label="Persist">
            backend subscription tables and user state are out of scope here
          </SummaryRow>
        </section>
      </div>

      <section className="mt-4 overflow-hidden rounded-lg border border-gray-200">
        <div className="border-b border-gray-200 bg-gray-50 px-3 py-2">
          <h4 className="text-xs font-medium text-gray-950">
            Notebook query handoff
          </h4>
          <p className="mt-1 text-xs text-gray-500">
            Query the friendly entries view after creating the generated DuckDB
            view over rss_entries(url).
          </p>
        </div>
        <div className="divide-y divide-gray-200">
          {queryTemplates.map((template) => (
            <div className="bg-white px-3 py-2" key={template.label}>
              <div className="text-[11px] font-medium uppercase text-gray-400">
                {template.label}
              </div>
              <pre className="mt-1 overflow-x-auto whitespace-pre-wrap break-all rounded border border-gray-200 bg-gray-950 px-3 py-2 font-mono text-xs leading-5 text-white">
                {template.sql.split("\n").map((line) => (
                  <div key={`${template.label}:${line}`}>{line}</div>
                ))}
              </pre>
            </div>
          ))}
        </div>
        <p className="border-t border-gray-200 bg-amber-50 px-3 py-2 text-xs leading-5 text-amber-800">
          This registers the RSS table-functions only; it does not create a
          durable shared feed subscription.
        </p>
      </section>

      <section className="mt-4 overflow-hidden rounded-lg border border-gray-200">
        <div className="border-b border-gray-200 bg-gray-50 px-3 py-2">
          <h4 className="text-xs font-medium text-gray-950">
            Available table-functions
          </h4>
        </div>
        <div className="divide-y divide-gray-200">
          <RssTableFunctionRow
            detail="Browse RSSHub predefined sources, routes, categories, examples, and generated feed URLs."
            name="rss_routes"
          />
          <RssTableFunctionRow
            detail="Fetch channel-level metadata for a direct RSS URL or an rsshub:// route."
            name="rss_feed"
          />
          <RssTableFunctionRow
            detail="Fetch feed entries from a direct RSS URL or an rsshub:// route."
            name="rss_entries"
          />
        </div>
      </section>

      {error && <ErrorBanner message={error} />}
    </div>
  );
}

function classifyRssInput(input: string): {
  mode: RssSourceMode;
  label: string;
} {
  const trimmed = input.trim();
  if (/^rsshub:\/\//i.test(trimmed)) {
    return { mode: "rsshub", label: "RSSHub route" };
  }
  if (/^https?:\/\//i.test(trimmed)) {
    return { mode: "direct", label: "Direct RSS URL" };
  }
  return { mode: "keyword", label: "Keyword discovery" };
}

function rssSourceHandleForSelection({
  input,
  mode,
  route,
  routeParams,
}: {
  input: string;
  mode: RssSourceMode;
  route: RssHubRoute | undefined;
  routeParams: Record<string, string>;
}) {
  if (mode === "rsshub" && route) {
    return rssSourceHandleFromParts([
      route.id,
      ...route.parameters.map((parameter) => routeParams[parameter.key] ?? ""),
    ]);
  }

  if (mode === "direct") {
    return rssSourceHandleFromParts(directRssSourceHandleParts(input));
  }

  return rssSourceHandleFromParts([input]);
}

function directRssSourceHandleParts(input: string) {
  try {
    const url = new URL(input.trim());
    const hostnameParts = url.hostname
      .split(".")
      .filter(Boolean)
      .filter((part, index) => index !== 0 || part !== "www");
    const hostWithoutSuffix =
      hostnameParts.length > 1 ? hostnameParts.slice(0, -1) : hostnameParts;
    const pathParts = url.pathname.split("/").filter(Boolean);

    return ["direct", ...hostWithoutSuffix, ...pathParts];
  } catch {
    return ["direct", input];
  }
}

function rssSourceHandleFromParts(parts: string[]) {
  const value = parts
    .join("_")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return value || "rss_source";
}

function rssClassificationLabel(mode: RssSourceMode) {
  if (mode === "direct") return "Direct RSS";
  if (mode === "rsshub") return "RSSHub";
  return "Search";
}

function rssClassificationDetail(mode: RssSourceMode) {
  if (mode === "direct") return "http or https feed URL";
  if (mode === "rsshub") return "rsshub:// route gateway";
  return "plain text discovery";
}

function defaultRssRouteParams(
  route: RssHubRoute | undefined,
): Record<string, string> {
  if (!route) return {};
  return Object.fromEntries(
    route.parameters.map((parameter) => [
      parameter.key,
      parameter.defaultValue,
    ]),
  );
}

function rssHubUrlForRoute(
  route: RssHubRoute | undefined,
  params: Record<string, string>,
) {
  if (!route) return "rsshub://";

  const path = route.parameters.reduce((currentPath, parameter) => {
    const value = params[parameter.key]?.trim() || `{${parameter.key}}`;
    return currentPath.replace(`:${parameter.key}`, encodeRssHubPathPart(value));
  }, route.template);

  return `rsshub://${path}`;
}

function sqlStringLiteral(value: string) {
  return `'${value.replaceAll("'", "''")}'`;
}

function encodeRssHubPathPart(value: string) {
  return value
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
}

function RssTableFunctionRow({
  detail,
  name,
}: {
  detail: string;
  name: string;
}) {
  return (
    <div className="bg-white px-3 py-2">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h5 className="truncate font-mono text-xs font-medium text-gray-950">
            {name}
          </h5>
          <p className="mt-0.5 text-xs text-gray-500">{detail}</p>
        </div>
        <span className="shrink-0 rounded bg-green-50 px-2 py-1 text-[11px] font-medium text-green-700">
          Ready
        </span>
      </div>
    </div>
  );
}

function SummaryRow({
  children,
  label,
}: {
  children: React.ReactNode;
  label: string;
}) {
  return (
    <div className="grid grid-cols-1 gap-1 border-b border-gray-200 px-3 py-2 last:border-b-0 sm:grid-cols-[112px_minmax(0,1fr)] sm:gap-3">
      <span className="text-xs text-gray-400">{label}</span>
      <span className="min-w-0 break-words font-mono text-xs text-gray-900">
        {children}
      </span>
    </div>
  );
}

function ProviderStatusBadge({
  status,
  tier,
}: {
  status: ProviderFulfillmentStatus;
  tier: string;
}) {
  if (status === "Ready") {
    return (
      <span className="rounded bg-green-50 px-1.5 py-0.5 text-[10px] font-semibold text-green-700">
        Ready
      </span>
    );
  }

  if (status === "Candidate") {
    return (
      <span className="rounded bg-amber-50 px-1.5 py-0.5 text-[10px] font-semibold text-amber-700">
        Candidate
      </span>
    );
  }

  if (status === "Blocked") {
    return (
      <span className="rounded bg-red-50 px-1.5 py-0.5 text-[10px] font-semibold text-red-700">
        Blocked
      </span>
    );
  }

  if (status === "Catalog") {
    return (
      <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] font-semibold text-gray-600">
        Catalog
      </span>
    );
  }

  return <TierBadge tier={tier} />;
}

function TierBadge({ tier }: { tier: string }) {
  return (
    <span
      className={clsx(
        "rounded px-1.5 py-0.5 text-[10px] font-semibold",
        tier === "A" && "bg-green-50 text-green-700",
        tier === "B" && "bg-amber-50 text-amber-700",
        tier !== "A" && tier !== "B" && "bg-gray-100 text-gray-600",
      )}
    >
      Tier {tier}
    </span>
  );
}

function ErrorBanner({ message }: { message: string }) {
  return (
    <div className="mt-4 flex items-start gap-2 rounded border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
      <AlertCircleIcon className="mt-0.5 shrink-0" size={14} />
      <span>{message}</span>
    </div>
  );
}

function stepVisualState({
  editMode,
  index,
  sourceFamily,
  stepIndex,
}: {
  editMode: boolean;
  index: number;
  sourceFamily: SourceFamily;
  stepIndex: number;
}): StepVisualState {
  if (index === stepIndex) return "active";
  if (editMode && index === 0) return "locked";

  const flowStepIndexes = stepIndexesForSourceFamily(sourceFamily);
  if (!flowStepIndexes.includes(index)) {
    return index < stepIndex ? "skipped" : "locked";
  }

  if (index < stepIndex) return "complete";
  return "locked";
}

function stepIndexesForSourceFamily(
  sourceFamily: SourceFamily,
): readonly number[] {
  if (sourceFamily === "rest_api") return REST_API_STEP_INDEXES;
  if (sourceFamily === "rss") return RSS_STEP_INDEXES;
  return ALL_STEP_INDEXES;
}

function canContinueFromStep({
  connectionOnly,
  credentialFields,
  credentials,
  datasourceName,
  genericLocation,
  missingSavedCredentialKeys,
  savedConnectionCredentials,
  selectedProvider,
  selectedSavedConnection,
  sourceFamily,
  sourceMode,
  stepIndex,
  tablePreview,
  editMode,
}: {
  connectionOnly: boolean;
  credentialFields: CredentialField[];
  credentials: Record<string, string>;
  datasourceName: string;
  genericLocation: string;
  missingSavedCredentialKeys: string[];
  savedConnectionCredentials: Record<string, string>;
  selectedProvider: ProviderSummary | null;
  selectedSavedConnection: ConnectionTemplate | null;
  sourceFamily: SourceFamily;
  sourceMode: SourceMode | null;
  stepIndex: number;
  tablePreview: OpenApiTablePreview | null;
  editMode: boolean;
}) {
  if (stepIndex === 0) {
    if (sourceFamily !== "rest_api") return true;

    if (sourceMode === "saved") {
      return (
        Boolean(selectedSavedConnection) &&
        missingSavedCredentialKeys.every(
          (key) => (savedConnectionCredentials[key]?.trim() ?? "").length > 0,
        )
      );
    }

    return Boolean(
      sourceMode &&
      (sourceMode !== "catalog" ||
        (selectedProvider && !isBlockedProvider(selectedProvider))),
    );
  }

  if (sourceFamily !== "rest_api") {
    if (sourceFamily === "rss" && stepIndex === 4) {
      return datasourceName.trim().length > 0;
    }
    if (stepIndex === 1) return genericLocation.trim().length > 0;
    return true;
  }

  if (stepIndex === 1) {
    return true;
  }

  if (stepIndex === 2) {
    const hasName = datasourceName.trim().length > 0;
    const credentialsSatisfied =
      editMode ||
      credentialFields.length === 0 ||
      credentialFields.every(
        (field) => (credentials[field.key]?.trim() ?? "").length > 0,
      );
    return hasName && credentialsSatisfied;
  }

  if (stepIndex === 3) {
    if (
      selectedProvider &&
      isSupportedProvider(selectedProvider) &&
      (selectedProvider.actions?.length ?? 0) > 0
    ) {
      return true;
    }
    return (
      connectionOnly ||
      tablePreview !== null ||
      (selectedProvider !== null && isReadyProvider(selectedProvider))
    );
  }

  return datasourceName.trim().length > 0;
}

function credentialsFromKeys(keys: string[]): Record<string, string> {
  return Object.fromEntries(keys.map((key) => [key, ""]));
}

function credentialFieldsForProvider(
  provider: ProviderSummary | null,
): CredentialField[] {
  if (!provider) return [];

  if (provider.credentialEnvVars.length > 0) {
    return provider.credentialEnvVars.map((key) => ({
      key,
      label: key,
      type: key.endsWith("_USER") ? "text" : "password",
    }));
  }

  const prefix = envPrefix(provider.providerKey);
  const authMode = provider.authMode.toLowerCase();

  if (authMode === "none" || authMode.includes("no_auth")) {
    return [];
  }

  if (authMode.includes("basic")) {
    return [
      { key: `${prefix}_USER`, label: `${prefix}_USER`, type: "text" },
      { key: `${prefix}_PASS`, label: `${prefix}_PASS`, type: "password" },
    ];
  }

  if (authMode.includes("oauth") || authMode.includes("token")) {
    return [
      { key: `${prefix}_TOKEN`, label: `${prefix}_TOKEN`, type: "password" },
    ];
  }

  return [
    { key: `${prefix}_API_KEY`, label: `${prefix}_API_KEY`, type: "password" },
  ];
}

function providerSummaryFromPrefill(providerName: string): ProviderSummary {
  const name = providerName.trim();

  return {
    name,
    providerKey: name,
    displayName: displayNameFromProvider(name),
    category: "Imported",
    tier: "B",
    authMode: "API_KEY",
    supportLevel: "catalog",
    fulfillmentStatus: "Catalog",
    specSourceKey: null,
    specUrl: null,
    experimentalSpecCount: 0,
    baseUrl: null,
    credentialEnvVars: [],
    tables: [],
    actions: [],
  };
}

function defaultDatasourceNameForFamily(family: SourceFamily) {
  if (family === "file") return "orders";
  if (family === "cloud_object_storage") return "events";
  if (family === "lakehouse") return "orders_delta";
  if (family === "database") return "external_database";
  if (family === "rss") return "rss";
  if (family === "advanced_sql") return "custom_query";
  return "rest_api";
}

function defaultDatasourceNameForProvider(provider: ProviderSummary) {
  if (/^\d/.test(provider.name)) {
    return provider.name.replace(/[^a-zA-Z0-9]+/g, "_");
  }

  return provider.name;
}

function restSourceModeLabel(mode: SourceMode) {
  return REST_API_SOURCE_MODES.find((sourceMode) => sourceMode.key === mode)
    ?.label;
}

function locationLabelForFamily(family: DatasourceSourceFamily) {
  if (family.key === "file") return "File or folder path";
  if (family.key === "cloud_object_storage") {
    return "URL or object storage path";
  }
  if (family.key === "lakehouse") return "Lakehouse table URI";
  if (family.key === "database") return "Database connection string";
  if (family.key === "advanced_sql") return "SQL attach statement";
  return "REST API endpoint";
}

function authDetailForFamily(family: DatasourceSourceFamily) {
  if (family.key === "file") {
    return {
      title: "No credentials required",
      scope: "DuckDB reads from the local filesystem.",
    };
  }
  if (family.key === "cloud_object_storage" || family.key === "lakehouse") {
    return {
      title: "DuckDB secret or signed URL",
      scope:
        "Credentials stay in the notebook session and are referenced by generated SQL.",
    };
  }
  if (family.key === "database") {
    return {
      title: "Database login",
      scope:
        "Use a connection string or a DuckDB secret for the attached engine.",
    };
  }
  if (family.key === "advanced_sql") {
    return {
      title: "Depends on referenced SQL",
      scope: "Any required secrets should be created before the SQL runs.",
    };
  }
  return {
    title: "API credentials",
    scope: "REST/API credentials continue through the preserved route.",
  };
}

function inspectDetailForFamily(family: DatasourceSourceFamily) {
  if (family.key === "file") return "Local object inferred from file extension";
  if (family.key === "cloud_object_storage") {
    return "Remote object set resolved through httpfs";
  }
  if (family.key === "lakehouse") return "Lakehouse table metadata preview";
  if (family.key === "database") return "Attached database schema preview";
  if (family.key === "advanced_sql") return "SQL result-set preview";
  return "REST/API table-function preview";
}

function previewObjectNameForFamily(family: DatasourceSourceFamily) {
  if (family.key === "file") return "orders";
  if (family.key === "cloud_object_storage") return "events";
  if (family.key === "lakehouse") return "orders_delta";
  if (family.key === "database") return "app.public.orders";
  if (family.key === "advanced_sql") return "custom_query";
  return "api_table";
}

function generatedSqlForFamily(
  family: DatasourceSourceFamily,
  location: string,
) {
  const escapedLocation = location.replaceAll("'", "''");

  if (family.key === "file") {
    return `create or replace view orders as
select *
from read_parquet('${escapedLocation}');

-- CSV alternative:
-- select * from read_csv_auto('${escapedLocation}');`;
  }

  if (family.key === "cloud_object_storage") {
    return `install httpfs;
load httpfs;

create or replace view events as
select *
from read_parquet('${escapedLocation}');`;
  }

  if (family.key === "lakehouse") {
    return `install delta;
load delta;

create or replace view orders_delta as
select *
from delta_scan('${escapedLocation}');`;
  }

  if (family.key === "database") {
    return `attach '${escapedLocation}' as external_db;

create or replace view external_database as
select *
from external_db.public.orders;`;
  }

  if (family.key === "advanced_sql") {
    return `create or replace view custom_query as
${location.trim().length > 0 ? location : "select 1 as id"};`;
  }

  return "-- REST/API uses the preserved import flow.";
}

function displayNameFromProvider(providerName: string) {
  const displayName = providerName
    .split(/[-_]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");

  return displayName || providerName;
}

function authSchemeLabel(authMode: string) {
  const normalized = authMode.toLowerCase();
  if (normalized === "none" || normalized.includes("no_auth")) {
    return "No auth";
  }
  if (normalized.includes("basic")) return "HTTP Basic";
  if (normalized.includes("oauth")) return "OAuth2 bearer token";
  if (normalized.includes("token")) return "Bearer token";
  if (normalized.includes("api") && normalized.includes("key")) {
    return "API key (Bearer)";
  }
  return authMode;
}

function baseUrlForProvider(provider: ProviderSummary) {
  if (provider.baseUrl) return provider.baseUrl;

  const known = KNOWN_BASE_URLS[provider.name.toLowerCase()];
  if (known) return known;

  return `https://api.${provider.name.toLowerCase().replaceAll("_", "-")}.com`;
}

function envPrefix(value: string) {
  return value
    .trim()
    .replace(/[^a-zA-Z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .toUpperCase();
}

function normalizedTier(tier: string | undefined) {
  const normalized = (tier ?? "C").trim().toUpperCase();
  if (normalized === "A" || normalized === "B") return normalized;
  return "C";
}

function tablePreviewFromTemplate(
  connection: ConnectionTemplate,
): OpenApiTablePreview | null {
  if (connection.tables.length === 0) return null;
  return {
    tables: connection.tables.map((table) => ({
      name: table.name,
      method: "GET",
      path: "",
      responsePath: null,
      columns: table.columns.map((column) => ({
        name: column.name,
        ty: column.sqlType,
        json: "",
      })),
    })),
  };
}

function tablePreviewFromProvider(
  provider: ProviderSummary,
): OpenApiTablePreview | null {
  if (!isReadyProvider(provider) || provider.tables.length === 0) return null;
  return { tables: provider.tables };
}

function isSupportedProvider(provider: ProviderSummary) {
  return isReadyProvider(provider);
}

function isReadyProvider(provider: ProviderSummary) {
  return providerFulfillmentStatus(provider) === "Ready";
}

function isBlockedProvider(provider: ProviderSummary) {
  return providerFulfillmentStatus(provider) === "Blocked";
}

function providerFulfillmentStatus(
  provider: ProviderSummary,
): ProviderFulfillmentStatus {
  switch (provider.fulfillmentStatus) {
    case "Ready":
    case "Candidate":
    case "Blocked":
    case "Catalog":
      return provider.fulfillmentStatus;
    default:
      break;
  }

  if (provider.supportLevel === "supported") return "Ready";
  if (provider.supportLevel === "experimental") return "Candidate";
  return "Catalog";
}

function errorMessage(caught: unknown) {
  return caught instanceof Error ? caught.message : "Unable to add datasource";
}
