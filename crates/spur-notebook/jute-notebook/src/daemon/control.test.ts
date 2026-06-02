import { beforeEach, describe, expect, test, vi } from "vitest";

import {
  addApiDatasourceCommand,
  attachSavedConnectionCommand,
  attachedSavedConnectionFromDaemonControlResponse,
  daemonControl,
  datasourceEntryFromDaemonControlResponse,
  deleteSavedConnectionCommand,
  listSavedConnectionsCommand,
  pathFromDaemonControlResponse,
  recentEntriesFromDaemonControlResponse,
  savedConnectionsFromDaemonControlResponse,
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
  });

  test("builds saved connection commands", () => {
    expect(listSavedConnectionsCommand()).toEqual({
      command: "list_saved_connections",
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
      }),
    ).toEqual({
      command: "attach_saved_connection",
      name: "stripe_reporting",
      credentials: [["STRIPE_API_KEY", "sk_test_123"]],
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
        createdAt: "2026-06-01T12:00:00Z",
        updatedAt: "2026-06-01T12:00:00Z",
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
