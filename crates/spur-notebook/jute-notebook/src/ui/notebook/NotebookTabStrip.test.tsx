import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import NotebookTabStrip from "@/ui/notebook/NotebookTabStrip";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  title: `${id}.ipynb`,
  dirty: false,
  kernelState: "live",
  mode: "cells",
  ...patch,
});

function renderStrip(tabs: NotebookTab[], overrides = {}) {
  const props = {
    activeTabId: tabs[0]?.id,
    tabs,
    onCloseTab: vi.fn(),
    onNewTab: vi.fn(),
    onOpenNotebook: vi.fn(),
    onSwitchTab: vi.fn(),
    ...overrides,
  };
  const view = render(<NotebookTabStrip {...props} />);
  return { ...view, props };
}

function auxClick(element: Element, button: number) {
  fireEvent(
    element,
    new MouseEvent("auxclick", { bubbles: true, button, cancelable: true }),
  );
}

afterEach(() => {
  cleanup();
});

describe("NotebookTabStrip", () => {
  it("renders pinned tabs icon-only without a close button", () => {
    renderStrip([tab("pin", { pinned: true }), tab("a")]);
    expect(screen.queryByLabelText("Close pin.ipynb")).toBeNull();
    expect(screen.queryByText("pin.ipynb")).toBeNull();
  });

  it("closes a non-pinned tab on middle click", () => {
    const { props } = renderStrip([tab("a"), tab("b")]);
    auxClick(screen.getByRole("tab", { name: /b\.ipynb/ }), 1);
    expect(props.onCloseTab).toHaveBeenCalledWith("b");
  });

  it("ignores middle click on pinned tabs", () => {
    const { props } = renderStrip([tab("pin", { pinned: true }), tab("a")]);
    auxClick(screen.getByLabelText("pin.ipynb (pinned)"), 1);
    expect(props.onCloseTab).not.toHaveBeenCalled();
  });

  it("creates a tab on double-clicking empty strip area", () => {
    const { props } = renderStrip([tab("a")]);
    fireEvent.doubleClick(screen.getByRole("tablist"));
    expect(props.onNewTab).toHaveBeenCalled();
  });

  it("marks an attention tab with a tick", () => {
    renderStrip([tab("a"), tab("b", { attention: true })]);
    const attn = screen.getByRole("tab", { name: /b\.ipynb/ }).closest("div");
    expect(attn?.className).toContain("bg-green-50");
    expect(screen.getByRole("tab", { name: /b\.ipynb/ })).toHaveTextContent(
      "✓",
    );
  });

  it("enters width-lock after close and releases on mouse leave", () => {
    const { props } = renderStrip([tab("a"), tab("b"), tab("c")]);
    fireEvent.click(screen.getByLabelText("Close b.ipynb"));
    expect(props.onCloseTab).toHaveBeenCalledWith("b");
    const strip = screen.getByTestId("tab-strip");
    expect(strip.dataset.widthLock).toBe("true");
    fireEvent.mouseLeave(strip);
    expect(strip.dataset.widthLock).toBeUndefined();
  });
});
