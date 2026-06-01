import clsx from "clsx";
import {
  AlertCircleIcon,
  CheckIcon,
  FileTextIcon,
  Loader2Icon,
  PencilIcon,
  PlugIcon,
  SearchIcon,
  TableIcon,
  XIcon,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import type {
  OpenApiTablePreview,
  ProviderSummary,
  TablePreview,
} from "@/bindings";
import {
  addApiDatasourceFromImportCommand,
  daemonControl,
  importedApiDatasourceFromDaemonControlResponse,
  listNangoProvidersCommand,
  nangoProvidersFromDaemonControlResponse,
  openApiTablePreviewFromDaemonControlResponse,
  previewOpenApiTablesCommand,
} from "@/daemon/control";

type SourceMode = "catalog" | "openapi" | "manual";
type WizardStep = "source" | "connect" | "tables" | "review";
type ProviderLoadState = "idle" | "loading" | "loaded";
type CredentialField = {
  key: string;
  label: string;
  type: "password" | "text";
};

export type AddRestApiWizardProps = {
  onClose: () => void;
  open: boolean;
};

const STEPS: { key: WizardStep; label: string; detail: string }[] = [
  { key: "source", label: "Source", detail: "pick how" },
  { key: "connect", label: "Connect", detail: "auth and URL" },
  { key: "tables", label: "Tables", detail: "schema" },
  { key: "review", label: "Review", detail: "add" },
];

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

export default function AddRestApiWizard({
  onClose,
  open,
}: AddRestApiWizardProps) {
  const [stepIndex, setStepIndex] = useState(0);
  const [sourceMode, setSourceMode] = useState<SourceMode | null>(null);
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [providerLoadState, setProviderLoadState] =
    useState<ProviderLoadState>("idle");
  const [providerSearch, setProviderSearch] = useState("");
  const [providerCategory, setProviderCategory] = useState("All");
  const [selectedProvider, setSelectedProvider] =
    useState<ProviderSummary | null>(null);
  const [datasourceName, setDatasourceName] = useState("");
  const [credentials, setCredentials] = useState<Record<string, string>>({});
  const [specText, setSpecText] = useState("");
  const [tablePreview, setTablePreview] = useState<OpenApiTablePreview | null>(
    null,
  );
  const [connectionOnly, setConnectionOnly] = useState(false);
  const [pendingPreview, setPendingPreview] = useState(false);
  const [pendingAdd, setPendingAdd] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setStepIndex(0);
    setSourceMode(null);
    setProviderSearch("");
    setProviderCategory("All");
    setSelectedProvider(null);
    setDatasourceName("");
    setCredentials({});
    setSpecText("");
    setTablePreview(null);
    setConnectionOnly(false);
    setPendingPreview(false);
    setPendingAdd(false);
    setError(null);
  }, [open]);

  useEffect(() => {
    if (!open) return;

    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose, open]);

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

  const selectSourceMode = useCallback(
    (mode: SourceMode) => {
      setSourceMode(mode);
      setError(null);
      setTablePreview(null);
      setConnectionOnly(false);

      if (mode === "catalog") {
        void loadProviders();
      } else {
        setSelectedProvider(null);
        setCredentials({});
        setDatasourceName((currentName) =>
          !currentName || currentName === selectedProvider?.name
            ? "rest_api"
            : currentName,
        );
      }
    },
    [loadProviders, selectedProvider?.name],
  );

  const selectProvider = useCallback((provider: ProviderSummary) => {
    setSelectedProvider(provider);
    setDatasourceName((currentName) =>
      !currentName || currentName === "rest_api" ? provider.name : currentName,
    );
    setCredentials({});
    setError(null);
  }, []);

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
    () => credentialFieldsForProvider(selectedProvider),
    [selectedProvider],
  );
  const canContinue = canContinueFromStep({
    connectionOnly,
    credentialFields,
    credentials,
    datasourceName,
    selectedProvider,
    sourceMode,
    stepIndex,
    tablePreview,
  });

  if (!open) return null;

  const step = STEPS[stepIndex]?.key ?? "source";

  const goNext = () => {
    if (!canContinue) return;
    setStepIndex((currentStep) => Math.min(STEPS.length - 1, currentStep + 1));
    setError(null);
  };

  const goBack = () => {
    setStepIndex((currentStep) => Math.max(0, currentStep - 1));
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
      const response = await daemonControl(
        addApiDatasourceFromImportCommand({
          name: datasourceName.trim(),
          provider: selectedProvider?.name ?? null,
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

  return (
    <div
      aria-modal="true"
      className="fixed inset-0 z-40 flex items-center justify-center bg-gray-950/35 px-4"
      onClick={onClose}
      role="dialog"
    >
      <section
        className="flex h-[min(620px,calc(100vh-40px))] w-full max-w-3xl overflow-hidden rounded-lg border border-gray-200 bg-white shadow-2xl"
        onClick={(event) => event.stopPropagation()}
      >
        <aside className="w-48 shrink-0 border-r border-gray-200 bg-gray-50 px-3 py-4">
          <div className="mb-4 flex items-center justify-between gap-2">
            <div className="flex min-w-0 items-center gap-2">
              <PlugIcon className="shrink-0 text-indigo-600" size={16} />
              <h2 className="truncate text-sm font-medium text-gray-950">
                Add REST API
              </h2>
            </div>
            <button
              aria-label="Close"
              className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-200 hover:text-gray-950"
              onClick={onClose}
              type="button"
            >
              <XIcon size={15} strokeWidth={1.5} />
            </button>
          </div>
          <ol className="space-y-1">
            {STEPS.map((wizardStep, index) => {
              const active = index === stepIndex;
              const complete = index < stepIndex;
              return (
                <li key={wizardStep.key}>
                  <button
                    className={clsx(
                      "flex w-full items-start gap-2 rounded px-2 py-2 text-left transition-colors",
                      active
                        ? "bg-white text-gray-950 shadow-sm ring-1 ring-gray-200"
                        : "text-gray-400",
                      complete && "text-gray-700 hover:bg-white",
                    )}
                    disabled={!complete && !active}
                    onClick={() => setStepIndex(index)}
                    type="button"
                  >
                    <span
                      className={clsx(
                        "mt-0.5 inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border text-[11px]",
                        active && "border-indigo-600 bg-indigo-600 text-white",
                        complete &&
                          "border-green-200 bg-green-50 text-green-700",
                        !active &&
                          !complete &&
                          "border-gray-300 bg-white text-gray-400",
                      )}
                    >
                      {complete ? (
                        <CheckIcon size={12} strokeWidth={2} />
                      ) : (
                        index + 1
                      )}
                    </span>
                    <span className="min-w-0">
                      <span className="block text-xs font-medium">
                        {wizardStep.label}
                      </span>
                      <span className="block truncate text-[11px] text-gray-400">
                        {wizardStep.detail}
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
                onSelectProvider={selectProvider}
                onSelectSourceMode={selectSourceMode}
                providerCategories={providerCategories}
                providerCategory={providerCategory}
                providerLoadState={providerLoadState}
                providerSearch={providerSearch}
                selectedProvider={selectedProvider}
                sourceMode={sourceMode}
              />
            )}
            {step === "connect" && (
              <ConnectStep
                credentials={credentials}
                datasourceName={datasourceName}
                onCredentialChange={(key, value) =>
                  setCredentials((current) => ({
                    ...current,
                    [key]: value,
                  }))
                }
                onDatasourceNameChange={setDatasourceName}
                selectedProvider={selectedProvider}
                sourceMode={sourceMode}
              />
            )}
            {step === "tables" && (
              <TablesStep
                connectionOnly={connectionOnly}
                error={error}
                onConnectionOnlyChange={(checked) => {
                  setConnectionOnly(checked);
                  if (checked) setTablePreview(null);
                }}
                onPreviewTables={() => void handlePreviewTables()}
                onSpecTextChange={(value) => {
                  setSpecText(value);
                  setTablePreview(null);
                }}
                pendingPreview={pendingPreview}
                selectedProvider={selectedProvider}
                specText={specText}
                tablePreview={tablePreview}
              />
            )}
            {step === "review" && (
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
            )}
          </div>

          <footer className="flex h-14 shrink-0 items-center justify-between border-t border-gray-200 bg-gray-50 px-5">
            <button
              className={clsx(
                "rounded border border-transparent px-3 py-2 text-sm font-medium text-gray-600 transition-colors hover:text-gray-950",
                stepIndex === 0 && "invisible",
              )}
              onClick={goBack}
              type="button"
            >
              Back
            </button>
            <button
              className="inline-flex items-center gap-2 rounded border border-indigo-600 bg-indigo-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-indigo-700 disabled:cursor-not-allowed disabled:border-gray-300 disabled:bg-gray-300"
              disabled={!canContinue || pendingAdd}
              onClick={() => {
                if (step === "review") {
                  void handleAddDatasource();
                } else {
                  goNext();
                }
              }}
              type="button"
            >
              {pendingAdd && <Loader2Icon className="animate-spin" size={14} />}
              {step === "review" ? "Add datasource" : "Continue"}
            </button>
          </footer>
        </div>
      </section>
    </div>
  );
}

function SourceStep({
  error,
  filteredProviders,
  onCategoryChange,
  onProviderSearch,
  onSelectProvider,
  onSelectSourceMode,
  providerCategories,
  providerCategory,
  providerLoadState,
  providerSearch,
  selectedProvider,
  sourceMode,
}: {
  error: string | null;
  filteredProviders: ProviderSummary[];
  onCategoryChange: (category: string) => void;
  onProviderSearch: (value: string) => void;
  onSelectProvider: (provider: ProviderSummary) => void;
  onSelectSourceMode: (mode: SourceMode) => void;
  providerCategories: string[];
  providerCategory: string;
  providerLoadState: ProviderLoadState;
  providerSearch: string;
  selectedProvider: ProviderSummary | null;
  sourceMode: SourceMode | null;
}) {
  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">
        How do you want to connect?
      </h3>
      <p className="mt-1 text-sm text-gray-500">
        Pick a provider catalog entry, start from an OpenAPI spec, or create a
        connection shell manually.
      </p>

      <div className="mt-4 space-y-2">
        <SourceOption
          active={sourceMode === "catalog"}
          detail="Browse Nango providers with auth mode and import tier pre-filled."
          icon={<PlugIcon size={18} strokeWidth={1.5} />}
          label="Provider catalog"
          meta="Nango"
          onClick={() => onSelectSourceMode("catalog")}
        />
        <SourceOption
          active={sourceMode === "openapi"}
          detail="Paste a spec or URL and preview generated table-functions."
          icon={<FileTextIcon size={18} strokeWidth={1.5} />}
          label="OpenAPI spec"
          onClick={() => onSelectSourceMode("openapi")}
        />
        <SourceOption
          active={sourceMode === "manual"}
          detail="Hand-author the manifest later and add tables when ready."
          icon={<PencilIcon size={18} strokeWidth={1.5} />}
          label="Manual"
          onClick={() => onSelectSourceMode("manual")}
        />
      </div>

      {sourceMode === "catalog" && (
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

      {error && <ErrorBanner message={error} />}
    </div>
  );
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
        <span className="block text-sm font-medium text-gray-950">{label}</span>
        <span className="mt-0.5 block text-xs text-gray-500">{detail}</span>
      </span>
      {meta && (
        <span className="rounded-full border border-indigo-200 bg-white px-2 py-1 text-xs font-medium text-indigo-600">
          {meta}
        </span>
      )}
    </button>
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

  return (
    <button
      className={clsx(
        "min-w-0 rounded-lg border bg-white p-2.5 text-left transition-colors",
        selected
          ? "border-indigo-600 bg-indigo-50 ring-1 ring-indigo-600"
          : "border-gray-200 hover:border-indigo-300",
      )}
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
        <TierBadge tier={tier} />
      </span>
    </button>
  );
}

function ConnectStep({
  credentials,
  datasourceName,
  onCredentialChange,
  onDatasourceNameChange,
  selectedProvider,
  sourceMode,
}: {
  credentials: Record<string, string>;
  datasourceName: string;
  onCredentialChange: (key: string, value: string) => void;
  onDatasourceNameChange: (value: string) => void;
  selectedProvider: ProviderSummary | null;
  sourceMode: SourceMode | null;
}) {
  const authScheme = selectedProvider
    ? authSchemeLabel(selectedProvider.authMode)
    : "API key or bearer token";
  const baseUrl = selectedProvider
    ? baseUrlForProvider(selectedProvider)
    : "https://api.example.com";
  const tier = normalizedTier(selectedProvider?.tier);
  const fields = credentialFieldsForProvider(selectedProvider);
  const heading = selectedProvider
    ? `Connect to ${selectedProvider.displayName}`
    : "Connect a custom REST API";

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">{heading}</h3>
      <p className="mt-1 text-sm text-gray-500">
        Auth, base URL, and credentials are passed to the daemon so it can build
        the datasource manifest.
      </p>

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
              {tier === "A" ? "Drop-in." : "Bring your own token."}
            </span>{" "}
            {tier === "A"
              ? "Paste the credential once; it stays in session state until the daemon receives it."
              : "Generate a token with the provider and paste it here for this session."}
          </p>
        </div>
      </div>

      <div className="mt-4 rounded-lg border border-gray-200 bg-gray-50">
        <SummaryRow label="Provider">
          {selectedProvider
            ? `${selectedProvider.displayName} · ${selectedProvider.category}`
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
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600"
            onChange={(event) =>
              onDatasourceNameChange(event.currentTarget.value)
            }
            placeholder="stripe_reporting"
            value={datasourceName}
          />
        </label>

        {fields.length > 0 ? (
          fields.map((field) => (
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
          ))
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

  return (
    <div>
      <h3 className="text-base font-semibold text-gray-950">Tables</h3>
      <p className="mt-1 text-sm text-gray-500">
        Preview generated table-functions from an OpenAPI spec, or add the
        connection now and define tables later.
      </p>

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
        <label className="inline-flex items-center gap-2 text-sm text-gray-600">
          <input
            checked={connectionOnly}
            className="h-4 w-4 rounded border-gray-300 text-indigo-600"
            onChange={(event) =>
              onConnectionOnlyChange(event.currentTarget.checked)
            }
            type="checkbox"
          />
          Connection only
        </label>
      </div>

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
            GET {table.path}
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
          {connectionOnly || specText.trim().length === 0
            ? "connection only"
            : "OpenAPI spec attached"}
        </SummaryRow>
        <SummaryRow label="Tables">
          {tableCount === 0
            ? "none yet"
            : `${tableCount} table-function${tableCount === 1 ? "" : "s"}`}
        </SummaryRow>
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

function SummaryRow({
  children,
  label,
}: {
  children: React.ReactNode;
  label: string;
}) {
  return (
    <div className="grid grid-cols-[112px_minmax(0,1fr)] gap-3 border-b border-gray-200 px-3 py-2 last:border-b-0">
      <span className="text-xs text-gray-400">{label}</span>
      <span className="min-w-0 break-words font-mono text-xs text-gray-900">
        {children}
      </span>
    </div>
  );
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

function canContinueFromStep({
  connectionOnly,
  credentialFields,
  credentials,
  datasourceName,
  selectedProvider,
  sourceMode,
  stepIndex,
  tablePreview,
}: {
  connectionOnly: boolean;
  credentialFields: CredentialField[];
  credentials: Record<string, string>;
  datasourceName: string;
  selectedProvider: ProviderSummary | null;
  sourceMode: SourceMode | null;
  stepIndex: number;
  tablePreview: OpenApiTablePreview | null;
}) {
  if (stepIndex === 0) {
    return Boolean(
      sourceMode && (sourceMode !== "catalog" || selectedProvider),
    );
  }

  if (stepIndex === 1) {
    const hasName = datasourceName.trim().length > 0;
    const credentialsSatisfied =
      credentialFields.length === 0 ||
      credentialFields.every(
        (field) => (credentials[field.key]?.trim() ?? "").length > 0,
      );
    return hasName && credentialsSatisfied;
  }

  if (stepIndex === 2) {
    return connectionOnly || tablePreview !== null;
  }

  return datasourceName.trim().length > 0;
}

function credentialFieldsForProvider(
  provider: ProviderSummary | null,
): CredentialField[] {
  if (!provider) return [];

  const prefix = envPrefix(provider.name);
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

function errorMessage(caught: unknown) {
  return caught instanceof Error ? caught.message : "Unable to add datasource";
}
