import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, test, vi } from "vitest";

import type { DatasourceEntry } from "@/bindings";

import DatasourceSidebar from "./DatasourceSidebar";

const daemonControlMock = vi.hoisted(() => vi.fn());
const openMock = vi.hoisted(() => vi.fn());

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

function droppedFile(path: string) {
  const file = new File(["region,revenue\nwest,12\n"], "sales.csv", {
    type: "text/csv",
  });
  Object.defineProperty(file, "path", { value: path });
  return file;
}

describe("DatasourceSidebar", () => {
  beforeEach(() => {
    daemonControlMock.mockReset();
    openMock.mockReset();
  });

  test("sidebar_attach_emits_command", async () => {
    daemonControlMock
      .mockResolvedValueOnce({
        ok: true,
        result: datasourceEntry({}),
      })
      .mockResolvedValueOnce({
        ok: true,
        result: datasourceEntry({
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

    fireEvent.change(screen.getByLabelText("Group"), {
      target: { value: "quarterly" },
    });

    fireEvent.drop(screen.getByTestId("datasource-dropzone"), {
      dataTransfer: { files: [droppedFile("/tmp/sales.csv")] },
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
});
