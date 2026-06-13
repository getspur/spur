import "@testing-library/jest-dom/vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import AddRestApiWizard from "./AddRestApiWizard";

const daemonControlMock = vi.hoisted(() => vi.fn());

vi.mock("@/daemon/control", async () => {
  const actual =
    await vi.importActual<typeof import("@/daemon/control")>(
      "@/daemon/control",
    );

  return {
    ...actual,
    daemonControl: daemonControlMock,
  };
});

const SPEC_TEXT = '{"openapi":"3.1.0","paths":{}}';
const MANIFEST_TOML = `
name = "stripe_reporting"
provider = "stripe"
`;

function providersResponse() {
  return {
    ok: true,
    result: {
      type: "nangoProviders" as const,
      data: [
        {
          name: "github",
          displayName: "GitHub",
          category: "Dev tools",
          tier: "A",
          authMode: "API_KEY",
          supportLevel: "supported",
          providerKey: "github",
          baseUrl: "https://api.github.com",
          specSourceKey: null,
          specUrl: null,
          experimentalSpecCount: 0,
          credentialEnvVars: ["GITHUB_TOKEN"],
          tables: [
            {
              name: "security_advisories",
              method: "GET",
              path: "/advisories",
              responsePath: null,
              columns: [{ name: "ghsaId", ty: "Utf8", json: "$.ghsa_id" }],
            },
            {
              name: "authenticated_repos",
              method: "GET",
              path: "/user/repos",
              responsePath: null,
              columns: [{ name: "name", ty: "Utf8", json: "$.name" }],
            },
          ],
          actions: [],
        },
        {
          name: "stripe",
          displayName: "Stripe",
          category: "Payments",
          tier: "A",
          authMode: "API_KEY",
          supportLevel: "supported",
          providerKey: "stripe",
          baseUrl: "https://api.stripe.com",
          specSourceKey: null,
          specUrl: null,
          experimentalSpecCount: 1,
          credentialEnvVars: ["STRIPE_API_KEY"],
          tables: [
            {
              name: "getaccounts",
              method: "GET",
              path: "/v1/accounts",
              responsePath: "$.data",
              columns: [{ name: "id", ty: "Utf8", json: "$.id" }],
            },
          ],
          actions: [],
        },
        {
          name: "facebook-ads",
          displayName: "Facebook Ads",
          category: "Marketing",
          tier: "B",
          authMode: "OAUTH2",
          supportLevel: "supported",
          providerKey: "facebook-ads",
          baseUrl: "https://graph.facebook.com/v21.0",
          specSourceKey: null,
          specUrl: null,
          experimentalSpecCount: 0,
          credentialEnvVars: [
            "FACEBOOK_ADS_CLIENT_ID",
            "FACEBOOK_ADS_CLIENT_SECRET",
            "FACEBOOK_ADS_REFRESH_TOKEN",
          ],
          tables: [],
          actions: [
            {
              name: "facebook_ads_insights",
              method: "POST",
              path: "/{ad_account_id}/insights",
              responsePath: "$.data",
              columns: [
                { name: "campaign_name", ty: "Utf8", json: "$.campaign_name" },
                { name: "impressions", ty: "Int64", json: "$.impressions" },
              ],
            },
          ],
        },
        {
          name: "1password-events",
          displayName: "1Password (Events API)",
          category: "Productivity",
          tier: "A",
          authMode: "API_KEY",
          supportLevel: "experimental",
          providerKey: "1password-events",
          baseUrl: "https://events.1password.com",
          specSourceKey: "1password.com:events",
          specUrl:
            "https://api.apis.guru/v2/specs/1password.com/events/1.0.0/openapi.json",
          experimentalSpecCount: 1,
          credentialEnvVars: [],
          tables: [],
          actions: [],
        },
        {
          name: "hubspot",
          displayName: "HubSpot",
          category: "CRM",
          tier: "B",
          authMode: "OAUTH2",
          supportLevel: "catalog",
          providerKey: "hubspot",
          baseUrl: null,
          specSourceKey: null,
          specUrl: null,
          experimentalSpecCount: 0,
          credentialEnvVars: [],
          tables: [],
          actions: [],
        },
      ],
    },
  };
}

function tablePreviewResponse() {
  return {
    ok: true,
    result: {
      type: "openApiTablePreview" as const,
      data: {
        tables: [
          {
            name: "stripe_charges",
            method: "GET",
            path: "/v1/charges",
            responsePath: "$.data",
            columns: [
              { name: "id", ty: "Utf8", json: "$.id" },
              {
                name: "billing_details_address_city",
                ty: "Utf8",
                json: "$.billing_details.address.city",
              },
            ],
          },
        ],
      },
    },
  };
}

function datasourceResponse() {
  return {
    ok: true,
    result: {
      type: "datasource" as const,
      data: {
        name: "stripe_reporting",
        path: "api://stripe",
        kind: "api_tables" as const,
        group: null,
        columns: [],
        rowCount: null,
        tables: [],
      },
    },
  };
}

function savedConnectionsResponse() {
  return {
    ok: true,
    result: {
      type: "savedConnections" as const,
      data: [
        {
          name: "stripe_reporting",
          provider: "stripe",
          group: "Payments",
          manifestToml: "name = 'stripe'",
          tables: [
            {
              name: "stripe_charges",
              columns: [],
              rowCount: null,
            },
            {
              name: "stripe_customers",
              columns: [],
              rowCount: null,
            },
          ],
          credentialEnvVars: ["STRIPE_API_KEY", "STRIPE_ACCOUNT"],
          createdAt: "2026-06-01T12:00:00Z",
          updatedAt: "2026-06-01T12:00:00Z",
        },
      ],
    },
  };
}

function attachedSavedConnectionResponse(missingEnvVars: string[]) {
  return {
    ok: true,
    result: {
      type: "attachedSavedConnection" as const,
      data: {
        entry: {
          name: "stripe_reporting",
          path: "stripe",
          kind: "api_tables" as const,
          group: "Payments",
          columns: [],
          rowCount: null,
          tables: [],
        },
        missing_env_vars: missingEnvVars,
      },
    },
  };
}

function renderWizard(onClose = vi.fn()) {
  render(<AddRestApiWizard open onClose={onClose} />);
  return onClose;
}

async function chooseStripeProvider() {
  fireEvent.click(screen.getByRole("button", { name: /Provider catalog/i }));

  const stripeButtons = await screen.findAllByRole("button", {
    name: /Stripe/i,
  });
  const stripe = stripeButtons.find(
    (button) => !button.textContent?.includes("Stripe API"),
  );
  if (!stripe) throw new Error("Stripe provider tile missing");
  fireEvent.click(stripe);
}

async function chooseGithubProvider() {
  fireEvent.click(screen.getByRole("button", { name: /Provider catalog/i }));

  const github = await screen.findByRole("button", { name: /GitHub/i });
  fireEvent.click(github);
}

async function chooseFacebookAdsProvider() {
  fireEvent.click(screen.getByRole("button", { name: /Provider catalog/i }));

  const facebookAds = await screen.findByRole("button", {
    name: /Facebook Ads/i,
  });
  fireEvent.click(facebookAds);
}

async function chooseExperimentalProvider() {
  fireEvent.click(screen.getByRole("button", { name: /Provider catalog/i }));

  const provider = await screen.findByRole("button", {
    name: /1Password \(Events API\)/i,
  });
  fireEvent.click(provider);
}

describe("AddRestApiWizard", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    daemonControlMock.mockReset();
    daemonControlMock.mockImplementation(
      (command: { command: string; credentials?: [string, string][] }) => {
        if (command.command === "list_nango_providers") {
          return Promise.resolve(providersResponse());
        }
        if (command.command === "preview_open_api_tables") {
          return Promise.resolve(tablePreviewResponse());
        }
        if (command.command === "add_api_datasource_from_import") {
          return Promise.resolve(datasourceResponse());
        }
        if (command.command === "add_api_datasource_from_manifest") {
          return Promise.resolve(datasourceResponse());
        }
        if (command.command === "list_saved_connections") {
          return Promise.resolve(savedConnectionsResponse());
        }
        if (command.command === "attach_saved_connection") {
          return Promise.resolve(
            attachedSavedConnectionResponse(
              command.credentials?.length ? [] : ["STRIPE_API_KEY"],
            ),
          );
        }
        return Promise.resolve({
          ok: true,
          result: { type: "empty", data: {} },
        });
      },
    );
  });

  test("source_selection_branches_between_catalog_openapi_and_manual", async () => {
    renderWizard();

    expect(
      screen.getByRole("heading", { name: "Add datasource" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Provider catalog/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /OpenAPI spec/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Manual/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /OpenAPI spec/i }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      await screen.findByRole("heading", {
        name: "Connect a custom REST API",
      }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    fireEvent.click(screen.getByRole("button", { name: /Manual/i }));

    expect(
      screen.getByText(/Hand-author the manifest later/i),
    ).toBeInTheDocument();
  });

  test("source_state_presents_datasource_families_and_preserves_rest_route", () => {
    renderWizard();

    expect(
      screen.getByRole("button", { name: /File or folder/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /URL or object storage/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Lakehouse table/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /External database/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /REST API/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Advanced SQL attach/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Provider catalog/i }),
    ).toBeInTheDocument();
  });

  test("file_family_renders_locate_auth_inspect_and_attach_details", () => {
    renderWizard();

    fireEvent.click(screen.getByRole("button", { name: /File or folder/i }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      screen.getByRole("heading", { name: "Locate file or folder" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("File or folder path")).toHaveValue(
      "/Users/me/data/orders.parquet",
    );
    expect(screen.getByText(/read_csv_auto/i)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(
      screen.getByRole("heading", { name: "Auth for file or folder" }),
    ).toBeInTheDocument();
    expect(screen.getByText("No credentials required")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(
      screen.getByRole("heading", { name: "Inspect file or folder" }),
    ).toBeInTheDocument();
    expect(screen.getByText("orders")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(
      screen.getByRole("heading", { name: "Attach file or folder" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Generated SQL")).toBeInTheDocument();
    expect(
      screen.getByText(/create or replace view orders/i),
    ).toBeInTheDocument();
  });

  test("file_family_can_pick_local_file_and_attach_through_callbacks", async () => {
    const onClose = vi.fn();
    const onPickLocalFile = vi.fn().mockResolvedValue("/tmp/inventory.parquet");
    const onAttachLocalFile = vi.fn().mockResolvedValue(undefined);

    render(
      <AddRestApiWizard
        open
        onAttachLocalFile={onAttachLocalFile}
        onClose={onClose}
        onPickLocalFile={onPickLocalFile}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /File or folder/i }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose local file" }));

    await waitFor(() => expect(onPickLocalFile).toHaveBeenCalledTimes(1));
    expect(screen.getByLabelText("File or folder path")).toHaveValue(
      "/tmp/inventory.parquet",
    );

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Attach datasource" }));

    await waitFor(() =>
      expect(onAttachLocalFile).toHaveBeenCalledWith("/tmp/inventory.parquet"),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("picking_a_catalog_provider_advances_to_resolved_connect_fields", async () => {
    renderWizard();

    await chooseStripeProvider();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      await screen.findByRole("heading", { name: "Connect to Stripe" }),
    ).toBeInTheDocument();
    expect(screen.getByText("API key (Bearer)")).toBeInTheDocument();
    expect(screen.getByText("https://api.stripe.com")).toBeInTheDocument();
    expect(screen.getByLabelText("STRIPE_API_KEY")).toBeInTheDocument();
    expect(screen.getByText(/Drop-in/i)).toBeInTheDocument();
  });

  test("supported_catalog_provider_shows_ready_tables_without_openapi_preview", async () => {
    const onClose = renderWizard();

    await chooseGithubProvider();
    expect(screen.getByText("2 table-functions")).toBeInTheDocument();
    expect(screen.getAllByText("Ready").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      await screen.findByRole("heading", { name: "Connect to GitHub" }),
    ).toBeInTheDocument();
    expect(screen.getByText("https://api.github.com")).toBeInTheDocument();
    expect(screen.getByLabelText("GITHUB_TOKEN")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("GITHUB_TOKEN"), {
      target: { value: "ghp_test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("security_advisories")).toBeInTheDocument();
    expect(screen.getByText("authenticated_repos")).toBeInTheDocument();
    expect(screen.getByText("GET /advisories")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource_from_import",
        name: "github",
        provider: "github",
        spec_text: null,
        credentials: [["GITHUB_TOKEN", "ghp_test"]],
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("supported_tier_b_provider_shows_ready_actions_without_openapi_preview", async () => {
    const onClose = renderWizard();

    await chooseFacebookAdsProvider();
    expect(screen.getByText("1 action-function")).toBeInTheDocument();
    expect(screen.getAllByText("Ready").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      await screen.findByRole("heading", { name: "Connect to Facebook Ads" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("https://graph.facebook.com/v21.0"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("FACEBOOK_ADS_CLIENT_ID")).toBeInTheDocument();
    expect(
      screen.getByLabelText("FACEBOOK_ADS_CLIENT_SECRET"),
    ).toBeInTheDocument();
    expect(
      screen.getByLabelText("FACEBOOK_ADS_REFRESH_TOKEN"),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("FACEBOOK_ADS_CLIENT_ID"), {
      target: { value: "meta-client" },
    });
    fireEvent.change(screen.getByLabelText("FACEBOOK_ADS_CLIENT_SECRET"), {
      target: { value: "meta-secret" },
    });
    fireEvent.change(screen.getByLabelText("FACEBOOK_ADS_REFRESH_TOKEN"), {
      target: { value: "meta-refresh" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("Bundled actions")).toBeInTheDocument();
    expect(screen.getByText("facebook_ads_insights")).toBeInTheDocument();
    expect(
      screen.getByText("POST /{ad_account_id}/insights"),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText("1 action-function")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource_from_import",
        name: "facebook-ads",
        provider: "facebook-ads",
        spec_text: null,
        credentials: [
          ["FACEBOOK_ADS_CLIENT_ID", "meta-client"],
          ["FACEBOOK_ADS_CLIENT_SECRET", "meta-secret"],
          ["FACEBOOK_ADS_REFRESH_TOKEN", "meta-refresh"],
        ],
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("experimental_catalog_provider_uses_spec_url_and_provider_key", async () => {
    const onClose = renderWizard();

    await chooseExperimentalProvider();

    expect(screen.getAllByText("Experimental").length).toBeGreaterThan(0);
    expect(screen.getByText("1 spec candidate")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(
      await screen.findByRole("heading", {
        name: "Connect to 1Password (Events API)",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByLabelText("1PASSWORD_EVENTS_API_KEY"),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("1PASSWORD_EVENTS_API_KEY"), {
      target: { value: "events_test" },
    });

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    const specInput = screen.getByLabelText("OpenAPI spec text or URL");
    expect(specInput).toHaveValue(
      "https://api.apis.guru/v2/specs/1password.com/events/1.0.0/openapi.json",
    );

    fireEvent.click(screen.getByLabelText("Connection only"));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource_from_import",
        name: "1password_events",
        provider: "1password-events",
        spec_text: null,
        credentials: [["1PASSWORD_EVENTS_API_KEY", "events_test"]],
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("prefill_opens_connect_step_with_blank_missing_credentials_and_saves_import", async () => {
    const onClose = vi.fn();

    render(
      <AddRestApiWizard
        open
        onClose={onClose}
        prefill={{
          name: "stripe_reporting",
          provider: "stripe",
          specText: SPEC_TEXT,
          missingEnvVars: ["STRIPE_API_KEY"],
          step: "connect",
        }}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Connect to Stripe" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Datasource name")).toHaveValue(
      "stripe_reporting",
    );
    expect(
      screen.queryByText("How do you want to connect?"),
    ).not.toBeInTheDocument();

    const apiKeyInput = screen.getByLabelText("STRIPE_API_KEY");
    expect(apiKeyInput).toHaveAttribute("type", "password");
    expect(apiKeyInput).toHaveValue("");

    fireEvent.change(apiKeyInput, { target: { value: "sk_test_prefill" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      await screen.findByLabelText("OpenAPI spec text or URL"),
    ).toHaveValue(SPEC_TEXT);
    fireEvent.click(screen.getByRole("button", { name: "Preview tables" }));

    expect(await screen.findByText("stripe_charges")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource_from_import",
        name: "stripe_reporting",
        provider: "stripe",
        spec_text: SPEC_TEXT,
        credentials: [["STRIPE_API_KEY", "sk_test_prefill"]],
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("manifest_prefill_finishes_from_connect_with_credentials", async () => {
    const onClose = vi.fn();

    render(
      <AddRestApiWizard
        open
        onClose={onClose}
        prefill={{
          name: "stripe_reporting",
          provider: "stripe",
          specText: null,
          manifestToml: MANIFEST_TOML,
          connectionOnly: false,
          missingEnvVars: ["STRIPE_API_KEY", "STRIPE_ACCOUNT"],
          step: "connect",
        }}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Connect to Stripe" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Finishing a connection started by the assistant/i),
    ).toBeInTheDocument();

    const addButton = screen.getByRole("button", { name: "Add datasource" });
    expect(addButton).toBeDisabled();

    fireEvent.change(screen.getByLabelText("STRIPE_API_KEY"), {
      target: { value: "sk_test_prefill" },
    });
    fireEvent.change(screen.getByLabelText("STRIPE_ACCOUNT"), {
      target: { value: "acct_123" },
    });
    expect(addButton).toBeEnabled();

    fireEvent.click(addButton);

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource_from_manifest",
        name: "stripe_reporting",
        manifest_toml: MANIFEST_TOML,
        credentials: [
          ["STRIPE_API_KEY", "sk_test_prefill"],
          ["STRIPE_ACCOUNT", "acct_123"],
        ],
      }),
    );
    expect(
      daemonControlMock.mock.calls.some(
        ([command]) => command.command === "add_api_datasource_from_import",
      ),
    ).toBe(false);
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("edit_connection_tables_default_to_get_preview_method", async () => {
    const onClose = vi.fn();
    const editConnection = savedConnectionsResponse().result.data[0];

    render(
      <AddRestApiWizard
        editConnection={editConnection}
        open
        onClose={onClose}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Connect to Stripe" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("stripe_charges")).toBeInTheDocument();
    expect(screen.getAllByText(/^GET\s*$/).length).toBeGreaterThan(0);
  });

  test("preview_openapi_tables_result_renders_tables_and_flattened_columns", async () => {
    renderWizard();

    fireEvent.click(screen.getByRole("button", { name: /OpenAPI spec/i }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("Datasource name"), {
      target: { value: "stripe_reporting" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("OpenAPI spec text or URL"), {
      target: { value: SPEC_TEXT },
    });
    fireEvent.click(screen.getByRole("button", { name: "Preview tables" }));

    expect(await screen.findByText("stripe_charges")).toBeInTheDocument();
    expect(screen.getByText("GET /v1/charges")).toBeInTheDocument();
    expect(
      screen.getByText("billing_details_address_city"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("$.billing_details.address.city"),
    ).toBeInTheDocument();
    expect(screen.getByText("response_path = $.data")).toBeInTheDocument();
  });

  test("review_adds_datasource_from_openapi_spec", async () => {
    const onClose = renderWizard();

    fireEvent.click(screen.getByRole("button", { name: /OpenAPI spec/i }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(await screen.findByLabelText("Datasource name"), {
      target: { value: "stripe_reporting" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("OpenAPI spec text or URL"), {
      target: { value: SPEC_TEXT },
    });
    fireEvent.click(screen.getByRole("button", { name: "Preview tables" }));

    expect(await screen.findByText("stripe_charges")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource_from_import",
        name: "stripe_reporting",
        provider: null,
        spec_text: SPEC_TEXT,
        credentials: [],
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("saved_connections_source_prompts_for_missing_tokens_then_retries_attach", async () => {
    const onClose = renderWizard();

    expect(
      screen.getByRole("button", { name: /Saved connections/i }),
    ).toBeInTheDocument();
    expect(screen.getByText("0 saved")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Saved connections/i }));

    expect(await screen.findByText("1 saved")).toBeInTheDocument();
    const useConnection = screen.getByRole("button", {
      name: "Use connection",
    });
    expect(useConnection).toBeDisabled();

    fireEvent.click(
      await screen.findByRole("button", { name: /stripe_reporting/i }),
    );

    expect(
      screen.getByText("Payments · 2 table-functions"),
    ).toBeInTheDocument();
    expect(screen.getByText("2 credentials")).toBeInTheDocument();
    expect(useConnection).toBeEnabled();

    fireEvent.click(useConnection);

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "attach_saved_connection",
        name: "stripe_reporting",
        credentials: [],
      }),
    );
    expect(await screen.findByLabelText("STRIPE_API_KEY")).toBeInTheDocument();
    expect(screen.queryByLabelText("STRIPE_ACCOUNT")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("STRIPE_API_KEY"), {
      target: { value: "sk_test_saved" },
    });
    fireEvent.click(useConnection);

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "attach_saved_connection",
        name: "stripe_reporting",
        credentials: [["STRIPE_API_KEY", "sk_test_saved"]],
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });
});
