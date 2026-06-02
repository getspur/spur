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

function providersResponse() {
  return {
    ok: true,
    result: {
      type: "nangoProviders" as const,
      data: [
        {
          name: "stripe",
          displayName: "Stripe",
          category: "Payments",
          tier: "A",
          authMode: "API_KEY",
        },
        {
          name: "hubspot",
          displayName: "HubSpot",
          category: "CRM",
          tier: "B",
          authMode: "OAUTH2",
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

  const stripe = await screen.findByRole("button", { name: /Stripe/i });
  fireEvent.click(stripe);
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

  test("review_adds_datasource_from_import_with_provider_spec_and_credentials", async () => {
    const onClose = renderWizard();

    await chooseStripeProvider();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(await screen.findByLabelText("Datasource name"), {
      target: { value: "stripe_reporting" },
    });
    fireEvent.change(screen.getByLabelText("STRIPE_API_KEY"), {
      target: { value: "sk_test_123" },
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
        provider: "stripe",
        spec_text: SPEC_TEXT,
        credentials: [["STRIPE_API_KEY", "sk_test_123"]],
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
