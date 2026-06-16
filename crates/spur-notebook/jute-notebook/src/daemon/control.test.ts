import { beforeEach, describe, expect, test, vi } from "vitest";

import {
  addApiDatasourceCommand,
  attachSavedConnectionCommand,
  attachedSavedConnectionFromDaemonControlResponse,
  credentialProfilesFromDaemonControlResponse,
  daemonControl,
  datasourceEntryFromDaemonControlResponse,
  deleteSavedConnectionCommand,
  listCredentialProfilesCommand,
  listNangoProvidersCommand,
  listSavedConnectionsCommand,
  nangoProvidersFromDaemonControlResponse,
  pathFromDaemonControlResponse,
  recentEntriesFromDaemonControlResponse,
  savedConnectionsFromDaemonControlResponse,
  updateSavedConnectionCommand,
} from "./control";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

describe("daemon control adapter", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test("opens notebooks through the typed daemon_control envelope", async () => {
    invokeMock.mockResolvedValueOnce({ ok: true, path: "/tmp/demo.ipynb" });

    const response = await daemonControl({
      command: "open",
      path: "/tmp/demo.ipynb",
    });

    expect(pathFromDaemonControlResponse(response, "open")).toBe(
      "/tmp/demo.ipynb",
    );
    expect(invokeMock).toHaveBeenCalledWith("daemon_control", {
      cmd: { command: "open", path: "/tmp/demo.ipynb" },
    });
  });

  test("maps enriched list_recents responses to HomePage entries", () => {
    const entries = recentEntriesFromDaemonControlResponse({
      ok: true,
      entries: [
        {
          path: "/tmp/demo.ipynb",
          lastOpened: "2026-05-28T12:00:00Z",
          isScratch: false,
          pinned: true,
          kernelAlive: true,
          isCurrent: true,
        },
      ],
    });

    expect(entries).toEqual([
      {
        path: "/tmp/demo.ipynb",
        lastOpened: "2026-05-28T12:00:00Z",
        isScratch: false,
        pinned: true,
        kernelAlive: true,
        isCurrent: true,
      },
    ]);
  });

  test("requires path responses for path-returning commands", async () => {
    expect(() => pathFromDaemonControlResponse({ ok: true }, "open")).toThrow(
      "daemon open response did not include path",
    );
  });

  test("unwraps tagged attach_datasource results", () => {
    expect(
      datasourceEntryFromDaemonControlResponse({
        ok: true,
        result: {
          type: "datasource",
          data: {
            name: "sales",
            path: "/tmp/sales.csv",
            kind: "csv",
            group: "quarterly",
            columns: [{ name: "amount", sqlType: "DOUBLE" }],
            rowCount: 42,
            tables: [],
          },
        },
      }),
    ).toEqual({
      name: "sales",
      path: "/tmp/sales.csv",
      kind: "csv",
      group: "quarterly",
      columns: [{ name: "amount", sqlType: "DOUBLE" }],
      rowCount: 42,
      tables: [],
    });
  });

  test("rejects bare attach_datasource results", () => {
    expect(() =>
      datasourceEntryFromDaemonControlResponse({
        ok: true,
        result: {
          name: "sales",
          path: "/tmp/sales.csv",
          kind: "csv",
          group: "quarterly",
          columns: [{ name: "amount", sqlType: "DOUBLE" }],
          rowCount: 42,
          tables: [],
        } as never,
      }),
    ).toThrow("daemon attach_datasource response did not include datasource");
  });

  test("builds add_api_datasource commands", () => {
    expect(
      addApiDatasourceCommand({
        name: "prediction",
        source: "polymarket",
      }),
    ).toEqual({
      command: "add_api_datasource",
      name: "prediction",
      source: "polymarket",
    });
    expect(
      addApiDatasourceCommand({
        name: "rss_work",
        source: "rss",
        rss_subscriptions: [
          {
            table: "youtube_channel_entries",
            url: "rsshub://youtube/channel/UC123",
          },
        ],
      }),
    ).toEqual({
      command: "add_api_datasource",
      name: "rss_work",
      source: "rss",
      rss_subscriptions: [
        {
          table: "youtube_channel_entries",
          url: "rsshub://youtube/channel/UC123",
        },
      ],
    });
  });

  test("builds saved connection commands", () => {
    expect(listNangoProvidersCommand()).toEqual({
      command: "list_nango_providers",
    });
    expect(listSavedConnectionsCommand()).toEqual({
      command: "list_saved_connections",
    });
    expect(listCredentialProfilesCommand("stripe")).toEqual({
      command: "list_credential_profiles",
      provider: "stripe",
    });
    expect(
      attachSavedConnectionCommand({
        name: "stripe_reporting",
      }),
    ).toEqual({
      command: "attach_saved_connection",
      name: "stripe_reporting",
      credentials: [],
    });
    expect(
      attachSavedConnectionCommand({
        name: "stripe_reporting",
        credentials: [["STRIPE_API_KEY", "sk_test_123"]],
        credential_ref: "stripe-live",
        tables: ["stripe_charges", "stripe_customers"],
      }),
    ).toEqual({
      command: "attach_saved_connection",
      name: "stripe_reporting",
      credentials: [["STRIPE_API_KEY", "sk_test_123"]],
      credential_ref: "stripe-live",
      tables: ["stripe_charges", "stripe_customers"],
    });
    expect(
      deleteSavedConnectionCommand({
        name: "stripe_reporting",
      }),
    ).toEqual({
      command: "delete_saved_connection",
      name: "stripe_reporting",
    });
  });

  test("builds update_saved_connection with credentials", () => {
    expect(
      updateSavedConnectionCommand({
        name: "scores",
        spec_text: null,
        credentials: [["SCORES_API_KEY", "x"]],
        credential_ref: "scores-live",
      }),
    ).toEqual({
      command: "update_saved_connection",
      name: "scores",
      spec_text: null,
      credentials: [["SCORES_API_KEY", "x"]],
      credential_ref: "scores-live",
    });
  });

  test("defaults update_saved_connection credentials to []", () => {
    expect(
      updateSavedConnectionCommand({
        name: "scores",
        spec_text: "openapi: 3.0.0",
      }),
    ).toEqual({
      command: "update_saved_connection",
      name: "scores",
      spec_text: "openapi: 3.0.0",
      credentials: [],
    });
  });

  test("unwraps saved connection list results", () => {
    expect(
      savedConnectionsFromDaemonControlResponse({
        ok: true,
        result: {
          type: "savedConnections",
          data: [
            {
              name: "stripe_reporting",
              provider: "stripe",
              group: "API",
              manifestToml: "name = 'stripe'",
              tables: [],
              credentialEnvVars: ["STRIPE_API_KEY"],
              credentialRef: null,
              createdAt: "2026-06-01T12:00:00Z",
              updatedAt: "2026-06-01T12:00:00Z",
            },
          ],
        },
      }),
    ).toEqual([
      {
        name: "stripe_reporting",
        provider: "stripe",
        group: "API",
        manifestToml: "name = 'stripe'",
        tables: [],
        credentialEnvVars: ["STRIPE_API_KEY"],
        credentialRef: null,
        createdAt: "2026-06-01T12:00:00Z",
        updatedAt: "2026-06-01T12:00:00Z",
      },
    ]);
  });

  test("unwraps credential profile list results", () => {
    expect(
      credentialProfilesFromDaemonControlResponse({
        ok: true,
        result: {
          type: "credentialProfiles",
          data: [
            {
              id: "stripe-live",
              provider: "stripe",
              label: "Stripe Live",
              keys: ["STRIPE_API_KEY"],
              createdAt: "2026-06-01T12:00:00Z",
              updatedAt: "2026-06-01T12:00:00Z",
            },
          ],
        },
      }),
    ).toEqual([
      {
        id: "stripe-live",
        provider: "stripe",
        label: "Stripe Live",
        keys: ["STRIPE_API_KEY"],
        createdAt: "2026-06-01T12:00:00Z",
        updatedAt: "2026-06-01T12:00:00Z",
      },
    ]);
  });

  test("unwraps nango provider fulfillment statuses", () => {
    expect(
      nangoProvidersFromDaemonControlResponse({
        ok: true,
        result: {
          type: "nangoProviders",
          data: [
            {
              name: "asana",
              providerKey: "asana",
              displayName: "Asana",
              category: "Project management",
              tier: "B",
              authMode: "OAUTH2",
              supportLevel: "experimental",
              fulfillmentStatus: "Candidate",
              blockedReason: null,
              specSourceKey: "asana.com",
              specUrl:
                "https://api.apis.guru/v2/specs/asana.com/1.0/openapi.json",
              experimentalSpecCount: 1,
              baseUrl: "https://app.asana.com/api/1.0",
              credentialEnvVars: [],
              tables: [],
              actions: [],
            },
            {
              name: "blocked-mail",
              providerKey: "blocked-mail",
              displayName: "Blocked Mail",
              category: "Messaging",
              tier: "C",
              authMode: "API_KEY",
              supportLevel: "experimental",
              fulfillmentStatus: "Blocked",
              blockedReason: "unsupported_auth",
              specSourceKey: "blocked.example.com",
              specUrl: null,
              experimentalSpecCount: 2,
              baseUrl: null,
              credentialEnvVars: [],
              tables: [],
              actions: [],
            },
          ],
        },
      } as never),
    ).toEqual([
      {
        name: "asana",
        providerKey: "asana",
        displayName: "Asana",
        category: "Project management",
        tier: "B",
        authMode: "OAUTH2",
        supportLevel: "experimental",
        fulfillmentStatus: "Candidate",
        blockedReason: null,
        specSourceKey: "asana.com",
        specUrl: "https://api.apis.guru/v2/specs/asana.com/1.0/openapi.json",
        experimentalSpecCount: 1,
        baseUrl: "https://app.asana.com/api/1.0",
        credentialEnvVars: [],
        tables: [],
        actions: [],
      },
      {
        name: "blocked-mail",
        providerKey: "blocked-mail",
        displayName: "Blocked Mail",
        category: "Messaging",
        tier: "C",
        authMode: "API_KEY",
        supportLevel: "experimental",
        fulfillmentStatus: "Blocked",
        blockedReason: "unsupported_auth",
        specSourceKey: "blocked.example.com",
        specUrl: null,
        experimentalSpecCount: 2,
        baseUrl: null,
        credentialEnvVars: [],
        tables: [],
        actions: [],
      },
    ]);
  });

  test("unwraps attached saved connection results", () => {
    expect(
      attachedSavedConnectionFromDaemonControlResponse({
        ok: true,
        result: {
          type: "attachedSavedConnection",
          data: {
            entry: {
              name: "stripe_reporting",
              path: "stripe",
              kind: "api_tables",
              group: "API",
              columns: [],
              rowCount: null,
              tables: [],
            },
            credential_ref: "stripe-live",
            missing_env_vars: ["STRIPE_API_KEY"],
          },
        },
      }),
    ).toEqual({
      entry: {
        name: "stripe_reporting",
        path: "stripe",
        kind: "api_tables",
        group: "API",
        columns: [],
        rowCount: null,
        tables: [],
      },
      credentialRef: "stripe-live",
      missingEnvVars: ["STRIPE_API_KEY"],
    });
  });

  test("sends void commands through daemon_control and discards the response", async () => {
    invokeMock.mockResolvedValueOnce({ ok: true });

    await expect(
      daemonControl({
        command: "set_pinned",
        path: "/tmp/demo.ipynb",
        pinned: true,
      }),
    ).resolves.toEqual({ ok: true });

    expect(invokeMock).toHaveBeenCalledWith("daemon_control", {
      cmd: { command: "set_pinned", path: "/tmp/demo.ipynb", pinned: true },
    });
  });
});
