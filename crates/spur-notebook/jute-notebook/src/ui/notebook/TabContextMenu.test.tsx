import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import TabContextMenu from "@/ui/notebook/TabContextMenu";

const tab: NotebookTab = {
  id: "a",
  path: "~/notebooks/a.ipynb",
  title: "a.ipynb",
  dirty: false,
  kernelState: "live",
  mode: "cells",
};

function renderMenu(overrides = {}) {
  const props = {
    canReopen: true,
    closeOthersCount: 2,
    closeRightCount: 1,
    onClose: vi.fn(),
    onCloseOthers: vi.fn(),
    onCloseRight: vi.fn(),
    onCopyPath: vi.fn(),
    onDismiss: vi.fn(),
    onReopenClosed: vi.fn(),
    onTogglePin: vi.fn(),
    position: { x: 10, y: 10 },
    tab,
    ...overrides,
  };
  render(<TabContextMenu {...props} />);
  return props;
}

afterEach(() => {
  cleanup();
});

describe("TabContextMenu", () => {
  it("renders the browser tab actions in order", () => {
    renderMenu();

    expect(
      screen.getAllByRole("menuitem").map((item) => item.textContent),
    ).toEqual([
      "Pin tab",
      "Duplicate",
      "Close⌘W",
      "Close others (2)",
      "Close to the right (1)",
      "Reopen closed tab⌘⇧T",
      "Copy path",
      "Move to new window",
    ]);
  });

  it("invokes each enabled action before dismissing", () => {
    for (const [name, callbackKey] of [
      ["Pin tab", "onTogglePin"],
      ["Close ⌘W", "onClose"],
      ["Close others (2)", "onCloseOthers"],
      ["Close to the right (1)", "onCloseRight"],
      ["Reopen closed tab ⌘⇧T", "onReopenClosed"],
      ["Copy path", "onCopyPath"],
    ] as const) {
      cleanup();
      const calls: string[] = [];
      const props = renderMenu({
        [callbackKey]: vi.fn(() => calls.push(callbackKey)),
        onDismiss: vi.fn(() => calls.push("dismiss")),
      });

      fireEvent.click(screen.getByRole("menuitem", { name }));

      expect(props[callbackKey]).toHaveBeenCalledTimes(1);
      expect(props.onDismiss).toHaveBeenCalledTimes(1);
      expect(calls).toEqual([callbackKey, "dismiss"]);
    }
  });

  it("disables zero-count, unavailable, and stub items", () => {
    renderMenu({
      canReopen: false,
      closeOthersCount: 0,
      closeRightCount: 0,
      tab: { ...tab, path: undefined },
    });

    expect(
      screen.getByRole("menuitem", { name: "Close others (0)" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("menuitem", { name: "Close to the right (0)" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("menuitem", { name: "Reopen closed tab ⌘⇧T" }),
    ).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Copy path" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Duplicate" })).toBeDisabled();
    expect(
      screen.getByRole("menuitem", { name: "Move to new window" }),
    ).toBeDisabled();
  });

  it("shows Unpin and omits the Close shortcut for pinned tabs", () => {
    renderMenu({ tab: { ...tab, pinned: true } });

    expect(screen.getByRole("menuitem", { name: "Unpin tab" })).toBeVisible();
    expect(screen.getByRole("menuitem", { name: "Close" })).toBeVisible();
    expect(
      screen.queryByRole("menuitem", { name: "Close ⌘W" }),
    ).not.toBeInTheDocument();
  });

  it("dismisses on outside mousedown", () => {
    const props = renderMenu();

    fireEvent.mouseDown(document.body);

    expect(props.onDismiss).toHaveBeenCalledTimes(1);
  });
});
