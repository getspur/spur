import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
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
    onCloseOthers: vi.fn(),
    onCloseRight: vi.fn(),
    onReopenClosed: vi.fn(),
    onNewTab: vi.fn(),
    onOpenNotebook: vi.fn(),
    onReorder: vi.fn(),
    onSwitchTab: vi.fn(),
    canReopen: false,
    onTogglePin: vi.fn(),
    getKernelStats: vi.fn().mockResolvedValue(null),
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

  it("pinned tabs are not draggable", () => {
    renderStrip([tab("pin", { pinned: true }), tab("a")]);
    expect(screen.getByLabelText("pin.ipynb (pinned)")).toHaveAttribute(
      "draggable",
      "false",
    );
    expect(
      screen.getByRole("tab", { name: /a\.ipynb/ }).closest("[draggable]"),
    ).toHaveAttribute("draggable", "true");
  });

  it("emits onReorder with the drop index", () => {
    const onReorder = vi.fn();
    renderStrip([tab("a"), tab("b"), tab("c")], { onReorder });
    const tabA = screen.getByRole("tab", { name: /a\.ipynb/ }).closest("div")!;
    const tabC = screen.getByRole("tab", { name: /c\.ipynb/ }).closest("div")!;
    fireEvent.dragStart(tabA);
    fireEvent.dragOver(tabC, { clientX: 1000 });
    fireEvent.drop(tabC, { clientX: 1000 });
    expect(onReorder).toHaveBeenCalledWith("a", 2);
  });

  it("opens the context menu on right-click and pins through it", () => {
    const onTogglePin = vi.fn();
    renderStrip([tab("a"), tab("b")], { onTogglePin });
    fireEvent.contextMenu(screen.getByRole("tab", { name: /b\.ipynb/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Pin tab" }));
    expect(onTogglePin).toHaveBeenCalledWith("b");
  });

  it("shows the hover card after 350ms with fetched stats", async () => {
    vi.useFakeTimers();
    const getKernelStats = vi.fn().mockResolvedValue({
      kernel_id: "k",
      spec_name: "python3",
      generation: 1,
      status: "alive",
      cpu_pct: 12,
      mem_mb: 96,
    });
    renderStrip([tab("a")], { getKernelStats });
    fireEvent.mouseEnter(screen.getByRole("tab", { name: /a\.ipynb/ }));
    await act(async () => {
      vi.advanceTimersByTime(350);
    });
    expect(screen.getByRole("tooltip")).toBeVisible();
    expect(getKernelStats).toHaveBeenCalledWith("a");
    vi.useRealTimers();
  });

  it("opens the searchable tab list from the overflow button", () => {
    const onSwitchTab = vi.fn();
    renderStrip([tab("a"), tab("b")], { onSwitchTab });
    fireEvent.click(screen.getByLabelText("Tab overflow"));
    fireEvent.click(screen.getByRole("menuitem", { name: /b\.ipynb/ }));
    expect(onSwitchTab).toHaveBeenCalledWith("b");
  });
});
