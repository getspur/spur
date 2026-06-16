import "@testing-library/jest-dom/vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import type {
  CellDagMetadata,
  ConnectionTemplate,
  DaemonControlCommand,
  DaemonControlResponse,
  DatasourceEntry,
} from "@/bindings";
import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "@/stores/sidebar";

import DatasourcePanel, {
  restWizardPrefillFromPayload,
} from "./DatasourcePanel";

const daemonControlMock = vi.hoisted(() => vi.fn());
const dragDropCallbacks = vi.hoisted(() => [] as TauriDragDropCallback[]);
const eventCallbacks = vi.hoisted(() => new Map<string, TauriEventCallback>());
const listenMock = vi.hoisted(() => vi.fn());
const onDragDropEventMock = vi.hoisted(() => vi.fn());
const openMock = vi.hoisted(() => vi.fn());
const setCellScheduleMock = vi.hoisted(() => vi.fn());
const unlistenEventMock = vi.hoisted(() => vi.fn());
const unlistenMock = vi.hoisted(() => vi.fn());

type TauriDragDropCallback = (event: {
  payload: {
    type: "drop";
    paths: string[];
    position: { x: number; y: number };
  };
}) => void;

type TauriEventCallback = (event: { payload: unknown }) => void;

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

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: openMock,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: listenMock,
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: onDragDropEventMock,
  }),
}));

vi.mock("../../dag/scheduleApi", () => ({
  setCellSchedule: setCellScheduleMock,
}));

function datasourceEntry(overrides: Partial<DatasourceEntry>): DatasourceEntry {
  return {
    name: "sales",
    path: "/tmp/sales.csv",
    kind: "csv",
    group: "quarterly",
    columns: [
      { name: "region", sqlType: "VARCHAR" },
      { name: "revenue", sqlType: "BIGINT" },
    ],
    rowCount: 2,
    tables: [],
    ...overrides,
  };
}

function datasourceResult(overrides: Partial<DatasourceEntry> = {}) {
  return {
    type: "datasource" as const,
    data: datasourceEntry(overrides),
  };
}

function savedConnectionTemplate(
  overrides: Partial<ConnectionTemplate> = {},
): ConnectionTemplate {
  return {
    name: "stripe_reporting",
    provider: "stripe",
    group: "API",
    manifestToml: "name = 'stripe'",
    tables: [
      {
        name: "stripe_charges",
        columns: [{ name: "id", sqlType: "VARCHAR" }],
        rowCount: null,
      },
      {
        name: "stripe_customers",
        columns: [{ name: "customer_id", sqlType: "VARCHAR" }],
        rowCount: null,
      },
    ],
    credentialEnvVars: ["STRIPE_API_KEY"],
    createdAt: "2026-06-01T12:00:00Z",
    updatedAt: "2026-06-01T12:00:00Z",
    ...overrides,
  };
}

function defaultDaemonResponse(
  command: DaemonControlCommand,
): DaemonControlResponse {
  if (command.command === "list_datasources") {
    return { ok: true, result: { type: "datasources", data: [] } };
  }

  if (command.command === "list_saved_connections") {
    return { ok: true, result: { type: "savedConnections", data: [] } };
  }

  if (command.command === "attach_datasource") {
    const overrides: Partial<DatasourceEntry> = {
      name: command.name,
      path: command.path,
      kind: command.path.endsWith(".parquet") ? "parquet" : "csv",
      group: command.group,
    };
    if (command.name === "inventory") {
      overrides.columns = [{ name: "sku", sqlType: "VARCHAR" }];
      overrides.rowCount = null;
    }

    return {
      ok: true,
      result: datasourceResult(overrides),
    };
  }

  if (command.command === "insert_cell") {
    return {
      ok: true,
      result: {
        type: "delta",
        data: {
          version: 2,
          kind: {
            type: "cellInserted",
            after_id: command.after_id,
            cell: {
              id: "scheduled-query-cell",
              kind: command.kind,
              version: 1,
              lastEditedBy: command.last_edited_by,
              datasourceSetup: undefined,
              dagMetadata: undefined,
              codeType: command.code_type,
              frontendMetadata: undefined,
              juteDeckMetadata: undefined,
              source: command.source,
              execCount: null,
              status: "idle",
              outputs: [],
            },
          },
        },
      },
    };
  }

  if (command.command === "set_cell_metadata") {
    const patch = command.patch as { spur?: { dag?: CellDagMetadata } };
    return {
      ok: true,
      result: {
        type: "delta",
        data: {
          version: 3,
          kind: {
            type: "cellWritten",
            cell: {
              id: command.id,
              kind: "code",
              version: 2,
              lastEditedBy: "brain",
              datasourceSetup: undefined,
              dagMetadata: patch.spur?.dag,
              codeType: "python",
              frontendMetadata: undefined,
              juteDeckMetadata: undefined,
              source: "",
              execCount: null,
              status: "idle",
              outputs: [],
            },
          },
        },
      },
    };
  }

  if (command.command === "attach_saved_connection") {
    const savedConnection = savedConnectionTemplate();
    const tables =
      command.tables && command.tables.length > 0
        ? savedConnection.tables.filter((table) =>
            command.tables?.includes(table.name),
          )
        : savedConnection.tables;
    return {
      ok: true,
      result: {
        type: "attachedSavedConnection",
        data: {
          entry: datasourceEntry({
            name: command.name,
            path: `api://${command.name}`,
            kind: "api_tables",
            group: "API",
            columns: [],
            rowCount: null,
            tables,
          }),
          missing_env_vars: [],
        },
      },
    };
  }

  if (
    command.command === "detach_datasource" ||
    command.command === "delete_saved_connection"
  ) {
    return { ok: true, result: { type: "empty", data: {} } };
  }

  return { ok: true, result: { type: "empty", data: {} } };
}

describe("DatasourcePanel", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    useSidebar.setState({
      activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
      collapsed: false,
    });
    daemonControlMock.mockReset();
    daemonControlMock.mockImplementation((command: DaemonControlCommand) =>
      Promise.resolve(defaultDaemonResponse(command)),
    );
    dragDropCallbacks.length = 0;
    eventCallbacks.clear();
    listenMock.mockReset();
    listenMock.mockImplementation(
      (eventName: string, callback: TauriEventCallback) => {
        eventCallbacks.set(eventName, callback);
        return Promise.resolve(unlistenEventMock);
      },
    );
    onDragDropEventMock.mockReset();
    onDragDropEventMock.mockImplementation(
      (callback: TauriDragDropCallback) => {
        dragDropCallbacks.push(callback);
        return Promise.resolve(unlistenMock);
      },
    );
    openMock.mockReset();
    setCellScheduleMock.mockReset();
    setCellScheduleMock.mockResolvedValue(undefined);
    unlistenEventMock.mockReset();
    unlistenMock.mockReset();
  });

  test("rest_wizard_prefill_from_payload_preserves_manifest_metadata", () => {
    const manifestToml = `name = "stripe_reporting"`;

    expect(
      restWizardPrefillFromPayload({
        name: "stripe_reporting",
        provider: "stripe",
        manifest_toml: manifestToml,
        connection_only: false,
        missing_env_vars: ["STRIPE_API_KEY", 123],
      }),
    ).toEqual({
      name: "stripe_reporting",
      provider: "stripe",
      manifestToml,
      connectionOnly: false,
      missingEnvVars: ["STRIPE_API_KEY"],
      specText: undefined,
      step: "connect",
    });

    expect(
      restWizardPrefillFromPayload({
        name: "github_reporting",
        manifestToml,
        connectionOnly: true,
        missingEnvVars: [],
      }),
    ).toMatchObject({
      name: "github_reporting",
      manifestToml,
      connectionOnly: true,
    });
  });

  test("sidebar_drag_drop_attach_emits_command", async () => {
    render(<DatasourcePanel />);

    await waitFor(() => expect(dragDropCallbacks).toHaveLength(1));

    fireEvent.change(screen.getByLabelText("Group"), {
      target: { value: "quarterly" },
    });
    await waitFor(() => expect(dragDropCallbacks).toHaveLength(2));

    const dropzone = screen.getByTestId("datasource-dropzone");
    vi.spyOn(dropzone, "getBoundingClientRect").mockReturnValue({
      bottom: 120,
      height: 100,
      left: 10,
      right: 210,
      top: 20,
      width: 200,
      x: 10,
      y: 20,
      toJSON: () => undefined,
    });
    Object.defineProperty(window, "devicePixelRatio", {
      configurable: true,
      value: 2,
    });

    await act(async () => {
      dragDropCallbacks.at(-1)?.({
        payload: {
          type: "drop",
          paths: ["/tmp/sales.csv"],
          position: { x: 40, y: 60 },
        },
      });
    });

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "attach_datasource",
        name: "sales",
        path: "/tmp/sales.csv",
        group: "quarterly",
      }),
    );
    expect(await screen.findByText("region")).toBeInTheDocument();
    expect(screen.getByText("revenue")).toBeInTheDocument();
  });

  test("add_datasource_opens_generalized_wizard", async () => {
    render(<DatasourcePanel />);

    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Choose a datasource family" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /File or folder/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /REST API/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Provider catalog/i }),
    ).toBeInTheDocument();
    expect(openMock).not.toHaveBeenCalled();
  });

  test("file_family_preserves_dialog_based_local_file_attach", async () => {
    openMock.mockResolvedValueOnce("/tmp/inventory.parquet");

    render(<DatasourcePanel />);

    fireEvent.change(screen.getByLabelText("Group"), {
      target: { value: "quarterly" },
    });
    await waitFor(() => expect(dragDropCallbacks).toHaveLength(2));

    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));
    fireEvent.click(
      await screen.findByRole("button", { name: /File or folder/i }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose local file" }));

    await waitFor(() =>
      expect(openMock).toHaveBeenCalledWith({
        multiple: false,
        directory: false,
        filters: [
          {
            name: "Datasource",
            extensions: [
              "csv",
              "parquet",
              "parq",
              "json",
              "jsonl",
              "ndjson",
              "duckdb",
              "db",
              "sqlite",
            ],
          },
        ],
      }),
    );
    expect(screen.getByLabelText("File or folder path")).toHaveValue(
      "/tmp/inventory.parquet",
    );

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Attach datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "attach_datasource",
        name: "inventory",
        path: "/tmp/inventory.parquet",
        group: "quarterly",
      }),
    );
    expect(await screen.findByText("sku")).toBeInTheDocument();
  });

  test("datasources_changed_event_replaces_list", async () => {
    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(eventCallbacks.has("datasources://changed")).toBe(true),
    );

    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [datasourceEntry({ name: "sales", path: "/tmp/sales.csv" })],
      });
    });

    expect(await screen.findByText("sales")).toBeInTheDocument();

    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [
          datasourceEntry({
            name: "inventory",
            path: "/tmp/inventory.parquet",
            columns: [{ name: "sku", sqlType: "VARCHAR" }],
          }),
        ],
      });
    });

    expect(screen.queryByText("sales")).not.toBeInTheDocument();
    expect(await screen.findByText("inventory")).toBeInTheDocument();
    expect(screen.getByText("sku")).toBeInTheDocument();
  });

  test("open_rest_wizard_event_opens_wizard_and_activates_datasources_panel", async () => {
    useSidebar.getState().activatePanel("chat");

    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(eventCallbacks.has("notebook://open_rest_wizard")).toBe(true),
    );

    await act(async () => {
      eventCallbacks.get("notebook://open_rest_wizard")?.({
        payload: {
          name: "stripe_reporting",
          provider: "stripe",
          manifest_toml: "name = 'stripe_reporting'",
          missing_env_vars: ["STRIPE_API_KEY"],
        },
      });
    });

    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", {
        name: "Connect to Stripe",
      }),
    ).toBeInTheDocument();
    expect(screen.getByDisplayValue("stripe_reporting")).toBeInTheDocument();
    expect(useSidebar.getState().activePanelId).toBe("datasources");
    expect(
      daemonControlMock.mock.calls.some(
        ([command]) => command.command === "add_api_datasource",
      ),
    ).toBe(false);

    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [
          datasourceEntry({
            name: "prediction",
            path: "api://stripe",
            kind: "api_tables",
            group: null,
            columns: [],
            rowCount: null,
            tables: [
              {
                name: "stripe_charges",
                columns: [{ name: "id", sqlType: "VARCHAR" }],
                rowCount: null,
              },
            ],
          }),
        ],
      });
    });

    expect(await screen.findByText("stripe_charges")).toBeInTheDocument();
  });

  test("mount_fetch_populates_list_without_datasources_changed_event", async () => {
    daemonControlMock.mockImplementation((command: DaemonControlCommand) => {
      if (command.command === "list_datasources") {
        return Promise.resolve({
          ok: true,
          result: {
            type: "datasources",
            data: [
              datasourceEntry({
                name: "restored",
                path: "/tmp/restored.duckdb",
                kind: "duck_db",
                group: "SPUR",
                columns: [],
                rowCount: null,
                tables: [],
              }),
            ],
          },
        });
      }

      return Promise.resolve(defaultDaemonResponse(command));
    });

    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "list_datasources",
      }),
    );
    expect(await screen.findByText("restored")).toBeInTheDocument();
    expect(eventCallbacks.has("datasources://changed")).toBe(true);
  });

  test("remove_button_requires_confirmation_before_detach_datasource", async () => {
    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(eventCallbacks.has("datasources://changed")).toBe(true),
    );
    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [datasourceEntry({ name: "sales", path: "/tmp/sales.csv" })],
      });
    });

    fireEvent.click(screen.getByRole("button", { name: "Remove sales" }));

    expect(
      daemonControlMock.mock.calls.some(
        ([command]) =>
          command.command === "detach_datasource" && command.name === "sales",
      ),
    ).toBe(false);
    expect(
      screen.getByRole("button", { name: "Confirm remove sales" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Cancel remove sales" }),
    ).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "Cancel remove sales" }),
    );

    expect(
      screen.queryByRole("button", { name: "Confirm remove sales" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Remove sales" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm remove sales" }),
    );

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "detach_datasource",
        name: "sales",
      }),
    );
  });

  test("saved_connections_render_below_attached_and_support_expand_attach_delete", async () => {
    const savedConnection = savedConnectionTemplate();
    daemonControlMock.mockImplementation((command: DaemonControlCommand) => {
      if (command.command === "list_datasources") {
        return Promise.resolve({
          ok: true,
          result: {
            type: "datasources",
            data: [
              datasourceEntry({
                name: "attached_sales",
                path: "/tmp/attached_sales.csv",
                group: "In notebook",
              }),
            ],
          },
        });
      }

      if (command.command === "list_saved_connections") {
        return Promise.resolve({
          ok: true,
          result: { type: "savedConnections", data: [savedConnection] },
        });
      }

      if (command.command === "attach_saved_connection") {
        const tables =
          command.tables && command.tables.length > 0
            ? savedConnection.tables.filter((table) =>
                command.tables?.includes(table.name),
              )
            : savedConnection.tables;
        return Promise.resolve({
          ok: true,
          result: {
            type: "attachedSavedConnection",
            data: {
              entry: datasourceEntry({
                name: command.name,
                path: `api://${command.name}`,
                kind: "api_tables",
                group: "API",
                columns: [],
                rowCount: null,
                tables,
              }),
              missing_env_vars: ["STRIPE_API_KEY"],
            },
          },
        });
      }

      return Promise.resolve(defaultDaemonResponse(command));
    });

    render(<DatasourcePanel />);

    const attachedHeading = await screen.findByText("In this notebook");
    const savedHeading = await screen.findByText("Saved connections");
    const scrollRegion = screen.getByTestId("datasource-panel-scroll");
    expect(scrollRegion).toHaveClass("overflow-y-auto", "pb-20");
    expect(scrollRegion).toContainElement(savedHeading);
    expect(
      attachedHeading.compareDocumentPosition(savedHeading) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    expect(screen.getByText("attached_sales")).toBeInTheDocument();
    expect(screen.getByText("stripe_reporting")).toBeInTheDocument();
    expect(screen.queryByText("stripe_charges")).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", {
        name: "Expand saved connection stripe_reporting",
      }),
    );

    expect(screen.getByText("stripe · 2 table-functions")).toBeInTheDocument();
    expect(screen.getByText("STRIPE_API_KEY")).toBeInTheDocument();
    expect(screen.getByText("stripe_charges")).toBeInTheDocument();
    expect(screen.getByText("stripe_customers")).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "Select stripe_charges from stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "Select stripe_customers from stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Attach selected table-functions from stripe_reporting",
      }),
    );

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "attach_saved_connection",
        name: "stripe_reporting",
        credentials: [],
        tables: ["stripe_charges", "stripe_customers"],
      }),
    );
    expect(
      await screen.findByText(/Missing credentials for stripe_reporting/i),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Fix and attach stripe_reporting",
      }),
    ).toBeInTheDocument();

    const initialDeleteCalls = daemonControlMock.mock.calls.filter(
      ([command]) =>
        command.command === "delete_saved_connection" &&
        command.name === "stripe_reporting",
    );

    fireEvent.click(
      screen.getByRole("button", {
        name: "Delete saved connection stripe_reporting",
      }),
    );

    expect(
      daemonControlMock.mock.calls.filter(
        ([command]) =>
          command.command === "delete_saved_connection" &&
          command.name === "stripe_reporting",
      ),
    ).toHaveLength(initialDeleteCalls.length);
    expect(
      screen.getByRole("button", {
        name: "Delete permanently stripe_reporting",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Cancel delete saved connection stripe_reporting",
      }),
    ).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", {
        name: "Cancel delete saved connection stripe_reporting",
      }),
    );

    expect(
      screen.queryByRole("button", {
        name: "Delete permanently stripe_reporting",
      }),
    ).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", {
        name: "Delete saved connection stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Delete permanently stripe_reporting",
      }),
    );

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "delete_saved_connection",
        name: "stripe_reporting",
      }),
    );
  });

  test("missing_saved_connection_credentials_open_fix_and_attach_recovery", async () => {
    const savedConnection = savedConnectionTemplate();
    daemonControlMock.mockImplementation((command: DaemonControlCommand) => {
      if (command.command === "list_saved_connections") {
        return Promise.resolve({
          ok: true,
          result: { type: "savedConnections", data: [savedConnection] },
        });
      }

      if (command.command === "attach_saved_connection") {
        const tables =
          command.tables && command.tables.length > 0
            ? savedConnection.tables.filter((table) =>
                command.tables?.includes(table.name),
              )
            : savedConnection.tables;
        return Promise.resolve({
          ok: true,
          result: {
            type: "attachedSavedConnection",
            data: {
              entry: datasourceEntry({
                name: command.name,
                path: `api://${command.name}`,
                kind: "api_tables",
                group: "API",
                columns: [],
                rowCount: null,
                tables,
              }),
              missing_env_vars: ["STRIPE_API_KEY"],
            },
          },
        });
      }

      return Promise.resolve(defaultDaemonResponse(command));
    });

    render(<DatasourcePanel />);

    expect(await screen.findByText("stripe_reporting")).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", {
        name: "Select table-functions from saved connection stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "Select stripe_charges from stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Attach selected table-functions from stripe_reporting",
      }),
    );

    const fixButton = await screen.findByRole("button", {
      name: "Fix and attach stripe_reporting",
    });
    expect(fixButton).toBeInstanceOf(HTMLButtonElement);
    expect((fixButton as HTMLButtonElement).disabled).toBe(false);
    fixButton.focus();
    expect(document.activeElement).toBe(fixButton);
    expect(
      screen.getByText(/Missing credentials for stripe_reporting/i),
    ).toBeInTheDocument();

    fireEvent.click(fixButton);

    const dialog = await screen.findByRole("dialog", {
      name: "Add datasource",
    });
    expect(
      within(dialog).getByRole("button", { name: /Saved connections/i }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(
      within(dialog).getByRole("button", { name: /stripe_reporting/i }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(within(dialog).getByLabelText("STRIPE_API_KEY")).toBeInTheDocument();
    expect(
      within(dialog).queryByLabelText("STRIPE_ACCOUNT"),
    ).not.toBeInTheDocument();
  });

  test("saved_connections_attach_selected_tables_from_multiple_connections", async () => {
    const stripeConnection = savedConnectionTemplate();
    const githubConnection = savedConnectionTemplate({
      name: "github_reporting",
      provider: "github",
      manifestToml: "name = 'github'",
      credentialEnvVars: ["GITHUB_TOKEN"],
      tables: [
        {
          name: "github_issues",
          columns: [{ name: "number", sqlType: "BIGINT" }],
          rowCount: null,
        },
        {
          name: "github_repositories",
          columns: [{ name: "full_name", sqlType: "VARCHAR" }],
          rowCount: null,
        },
      ],
    });
    const saved = [stripeConnection, githubConnection];
    daemonControlMock.mockImplementation((command: DaemonControlCommand) => {
      if (command.command === "list_saved_connections") {
        return Promise.resolve({
          ok: true,
          result: { type: "savedConnections", data: saved },
        });
      }

      if (command.command === "attach_saved_connection") {
        const connection = saved.find(
          (candidate) => candidate.name === command.name,
        );
        const tables =
          command.tables && command.tables.length > 0
            ? (connection?.tables ?? []).filter((table) =>
                command.tables?.includes(table.name),
              )
            : (connection?.tables ?? []);

        return Promise.resolve({
          ok: true,
          result: {
            type: "attachedSavedConnection",
            data: {
              entry: datasourceEntry({
                name: command.name,
                path: `api://${command.name}`,
                kind: "api_tables",
                group: "API",
                columns: [],
                rowCount: null,
                tables,
              }),
              missing_env_vars: [],
            },
          },
        });
      }

      return Promise.resolve(defaultDaemonResponse(command));
    });

    render(<DatasourcePanel />);

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Expand saved connection stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "Select stripe_charges from stripe_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Attach selected table-functions from stripe_reporting",
      }),
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Expand saved connection github_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "Select github_issues from github_reporting",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Attach selected table-functions from github_reporting",
      }),
    );

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "attach_saved_connection",
        name: "stripe_reporting",
        credentials: [],
        tables: ["stripe_charges"],
      }),
    );
    expect(daemonControlMock).toHaveBeenCalledWith({
      command: "attach_saved_connection",
      name: "github_reporting",
      credentials: [],
      tables: ["github_issues"],
    });
    expect(await screen.findAllByText("stripe_charges")).not.toHaveLength(0);
    expect(await screen.findAllByText("github_issues")).not.toHaveLength(0);
  });

  test("saved_connection_details_wrap_long_tokens_in_compact_sidebar", async () => {
    const savedConnection = savedConnectionTemplate({
      credentialEnvVars: [
        "VERY_LONG_SERVICE_ACCOUNT_REFRESH_TOKEN_FOR_COMPACT_SIDEBAR",
      ],
    });
    daemonControlMock.mockImplementation((command: DaemonControlCommand) => {
      if (command.command === "list_saved_connections") {
        return Promise.resolve({
          ok: true,
          result: { type: "savedConnections", data: [savedConnection] },
        });
      }

      return Promise.resolve(defaultDaemonResponse(command));
    });

    render(<DatasourcePanel />);

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Expand saved connection stripe_reporting",
      }),
    );

    expect(
      screen.getByText(
        "VERY_LONG_SERVICE_ACCOUNT_REFRESH_TOKEN_FOR_COMPACT_SIDEBAR",
      ),
    ).toHaveClass("max-w-full", "break-all");
    expect(
      screen.getByRole("button", {
        name: "Edit saved connection stripe_reporting",
      }).parentElement,
    ).toHaveClass("flex-wrap");
  });

  test("multi_table_entry_renders_tables", async () => {
    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(eventCallbacks.has("datasources://changed")).toBe(true),
    );
    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [
          datasourceEntry({
            name: "warehouse",
            path: "/tmp/warehouse.duckdb",
            kind: "duck_db",
            columns: [],
            rowCount: null,
            tables: [
              {
                name: "orders",
                columns: [{ name: "order_id", sqlType: "BIGINT" }],
                rowCount: 12,
              },
              {
                name: "customers",
                columns: [{ name: "customer_name", sqlType: "VARCHAR" }],
                rowCount: null,
              },
            ],
          }),
        ],
      });
    });

    expect(await screen.findByText("orders")).toBeInTheDocument();
    expect(screen.getByText("order_id")).toBeInTheDocument();
    expect(screen.getByText("12 rows")).toBeInTheDocument();
    expect(screen.getByText("customers")).toBeInTheDocument();
    expect(screen.getByText("customer_name")).toBeInTheDocument();
  });

  test("api_tables_entry_renders_table_functions", async () => {
    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(eventCallbacks.has("datasources://changed")).toBe(true),
    );
    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [
          datasourceEntry({
            name: "polymarket",
            path: "api://polymarket",
            kind: "api_tables",
            group: null,
            columns: [],
            rowCount: null,
            tables: [
              {
                name: "polymarket_markets",
                columns: [
                  { name: "market_id", sqlType: "VARCHAR" },
                  { name: "question", sqlType: "VARCHAR" },
                ],
                rowCount: null,
              },
              {
                name: "polymarket_trades",
                columns: [{ name: "price", sqlType: "DOUBLE" }],
                rowCount: null,
              },
            ],
          }),
        ],
      });
    });

    expect(await screen.findAllByText("API")).toHaveLength(1);
    expect(screen.getByText("polymarket_markets")).toBeInTheDocument();
    expect(screen.getByText("market_id")).toBeInTheDocument();
    expect(screen.getByText("polymarket_trades")).toBeInTheDocument();
    expect(screen.getByText("price")).toBeInTheDocument();
  });

  test("schedule_query_button_creates_sql_table_function_cell_with_cron", async () => {
    render(<DatasourcePanel />);

    await waitFor(() =>
      expect(eventCallbacks.has("datasources://changed")).toBe(true),
    );
    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [
          datasourceEntry({
            name: "polymarket",
            path: "api://polymarket",
            kind: "api_tables",
            group: null,
            columns: [],
            rowCount: null,
            tables: [
              {
                name: "polymarket_markets",
                columns: [{ name: "market_id", sqlType: "VARCHAR" }],
                rowCount: null,
              },
            ],
          }),
        ],
      });
    });

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Schedule query polymarket_markets",
      }),
    );

    await waitFor(() => {
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "insert_cell",
        kind: "code",
        after_id: null,
        source: "SELECT * FROM polymarket_markets() LIMIT 100;\n",
        last_edited_by: "datasource",
        code_type: "sql",
      });
    });
    expect(
      daemonControlMock.mock.calls.find(
        ([command]) =>
          (command as DaemonControlCommand).command === "insert_cell",
      )?.[0],
    ).toMatchObject({
      source: expect.not.stringContaining("spur_rest.duckdb_extension"),
    });
    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "set_cell_metadata",
        id: "scheduled-query-cell",
        patch: {
          spur: {
            dag: {
              produces: [],
              consumes: [],
              source: {
                kind: "api_tables",
                port: "polymarket_markets",
                class: "dataframe",
              },
            },
          },
        },
        expected_version: 1,
      }),
    );
    await waitFor(() =>
      expect(setCellScheduleMock).toHaveBeenCalledWith(
        "scheduled-query-cell",
        {
          enabled: true,
          cron: "*/15 * * * *",
          timezone: "UTC",
          run_target: "cascade",
          skip_if_running: true,
          catch_up: false,
        },
        2,
      ),
    );
  });
});
