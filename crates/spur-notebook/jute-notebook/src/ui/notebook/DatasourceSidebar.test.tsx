import "@testing-library/jest-dom/vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import type { DatasourceEntry } from "@/bindings";

import DatasourceSidebar from "./DatasourceSidebar";

const daemonControlMock = vi.hoisted(() => vi.fn());
const dragDropCallbacks = vi.hoisted(() => [] as TauriDragDropCallback[]);
const eventCallbacks = vi.hoisted(() => new Map<string, TauriEventCallback>());
const listenMock = vi.hoisted(() => vi.fn());
const onDragDropEventMock = vi.hoisted(() => vi.fn());
const openMock = vi.hoisted(() => vi.fn());
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

describe("DatasourceSidebar", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    daemonControlMock.mockReset();
    daemonControlMock.mockResolvedValue({
      ok: true,
      result: { type: "datasources", data: [] },
    });
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
    unlistenEventMock.mockReset();
    unlistenMock.mockReset();
  });

  test("sidebar_attach_emits_command", async () => {
    daemonControlMock
      .mockResolvedValueOnce({
        ok: true,
        result: { type: "datasources", data: [] },
      })
      .mockResolvedValueOnce({
        ok: true,
        result: datasourceResult(),
      })
      .mockResolvedValueOnce({
        ok: true,
        result: datasourceResult({
          name: "inventory",
          path: "/tmp/inventory.parquet",
          kind: "parquet",
          group: "quarterly",
          columns: [{ name: "sku", sqlType: "VARCHAR" }],
          rowCount: null,
        }),
      });
    openMock.mockResolvedValueOnce("/tmp/inventory.parquet");

    render(<DatasourceSidebar />);

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

    fireEvent.click(screen.getByRole("button", { name: "Add datasource" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenLastCalledWith({
        command: "attach_datasource",
        name: "inventory",
        path: "/tmp/inventory.parquet",
        group: "quarterly",
      }),
    );
    expect(await screen.findByText("sku")).toBeInTheDocument();
  });

  test("datasources_changed_event_replaces_list", async () => {
    render(<DatasourceSidebar />);

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

  test("api_button_opens_modal_and_dispatches_add_api_datasource", async () => {
    daemonControlMock
      .mockResolvedValueOnce({
        ok: true,
        result: { type: "datasources", data: [] },
      })
      .mockResolvedValueOnce({
        ok: true,
        result: datasourceResult({
          name: "prediction",
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
      });

    render(<DatasourceSidebar />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Add API datasource" }),
    );

    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Name"), {
      target: { value: "prediction" },
    });
    fireEvent.change(screen.getByLabelText("Source"), {
      target: { value: "polymarket" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "add_api_datasource",
        name: "prediction",
        source: "polymarket",
      }),
    );
    expect(await screen.findByText("polymarket_markets")).toBeInTheDocument();
  });

  test("mount_fetch_populates_list_without_datasources_changed_event", async () => {
    daemonControlMock.mockResolvedValueOnce({
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

    render(<DatasourceSidebar />);

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "list_datasources",
      }),
    );
    expect(await screen.findByText("restored")).toBeInTheDocument();
    expect(eventCallbacks.has("datasources://changed")).toBe(true);
  });

  test("remove_button_dispatches_detach_datasource", async () => {
    daemonControlMock
      .mockResolvedValueOnce({
        ok: true,
        result: { type: "datasources", data: [] },
      })
      .mockResolvedValueOnce({
        ok: true,
        result: { type: "empty", data: null },
      });

    render(<DatasourceSidebar />);

    await waitFor(() =>
      expect(eventCallbacks.has("datasources://changed")).toBe(true),
    );
    await act(async () => {
      eventCallbacks.get("datasources://changed")?.({
        payload: [datasourceEntry({ name: "sales", path: "/tmp/sales.csv" })],
      });
    });

    fireEvent.click(screen.getByRole("button", { name: "Remove sales" }));

    await waitFor(() =>
      expect(daemonControlMock).toHaveBeenCalledWith({
        command: "detach_datasource",
        name: "sales",
      }),
    );
  });

  test("multi_table_entry_renders_tables", async () => {
    render(<DatasourceSidebar />);

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
    render(<DatasourceSidebar />);

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

    expect(await screen.findAllByText("API")).toHaveLength(2);
    expect(screen.getByText("polymarket_markets")).toBeInTheDocument();
    expect(screen.getByText("market_id")).toBeInTheDocument();
    expect(screen.getByText("polymarket_trades")).toBeInTheDocument();
    expect(screen.getByText("price")).toBeInTheDocument();
  });
});
