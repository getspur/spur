import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import TabListMenu from "@/ui/notebook/TabListMenu";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  path: `~/notebooks/${id}.ipynb`,
  title: `${id}.ipynb`,
  dirty: false,
  kernelState: "live",
  mode: "cells",
  ...patch,
});

function renderMenu(overrides = {}) {
  const props = {
    activeTabId: "etl",
    onDismiss: vi.fn(),
    onNewTab: vi.fn(),
    onOpenNotebook: vi.fn(),
    onSelect: vi.fn(),
    tabs: [tab("etl"), tab("sales"), tab("scratch", { dirty: true })],
    ...overrides,
  };
  render(<TabListMenu {...props} />);
  return props;
}

afterEach(() => {
  cleanup();
});

describe("TabListMenu", () => {
  it("filters rows by search query", () => {
    renderMenu();
    fireEvent.change(screen.getByPlaceholderText(/Search tabs/), {
      target: { value: "sal" },
    });
    expect(screen.queryByText("etl.ipynb")).toBeNull();
    expect(screen.getByText("sales.ipynb")).toBeVisible();
  });

  it("filters rows by path case-insensitively", () => {
    renderMenu({
      tabs: [
        tab("etl", { path: "~/Workflows/ETL.ipynb" }),
        tab("sales", { path: "~/Finance/sales.ipynb" }),
      ],
    });
    fireEvent.change(screen.getByPlaceholderText(/Search tabs/), {
      target: { value: "finance" },
    });
    expect(screen.queryByText("etl.ipynb")).toBeNull();
    expect(screen.getByText("sales.ipynb")).toBeVisible();
  });

  it("selects a tab and dismisses", () => {
    const props = renderMenu();
    fireEvent.click(screen.getByText("sales.ipynb"));
    expect(props.onSelect).toHaveBeenCalledWith("sales");
    expect(props.onDismiss).toHaveBeenCalled();
  });

  it("marks the current tab", () => {
    renderMenu();
    expect(screen.getByText("current")).toBeVisible();
  });

  it("keeps new and open actions", () => {
    const props = renderMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "New notebook" }));
    expect(props.onNewTab).toHaveBeenCalled();
    expect(props.onDismiss).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("menuitem", { name: "Open notebook..." }));
    expect(props.onOpenNotebook).toHaveBeenCalled();
    expect(props.onDismiss).toHaveBeenCalledTimes(2);
  });

  it("dismisses on outside mousedown", () => {
    const props = renderMenu();
    fireEvent.mouseDown(document.body);
    expect(props.onDismiss).toHaveBeenCalledTimes(1);
  });
});
