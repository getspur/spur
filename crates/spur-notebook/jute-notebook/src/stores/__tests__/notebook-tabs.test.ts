import { beforeEach, describe, expect, it } from "vitest";

import { useNotebookTabsStore, type NotebookTab } from "@/stores/notebook";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  title: id,
  dirty: false,
  kernelState: "idle",
  mode: "cells",
  ...patch,
});

beforeEach(() => {
  useNotebookTabsStore.setState({
    tabs: [],
    activeTabId: undefined,
    closedTabs: [],
  });
});

describe("pinning", () => {
  it("pins a tab to the end of the pinned group", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("p1", { pinned: true }), tab("a"), tab("b")],
    });
    useNotebookTabsStore.getState().setPinned("b", true);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "p1",
      "b",
      "a",
    ]);
    expect(useNotebookTabsStore.getState().tabs[1].pinned).toBe(true);
  });

  it("unpins a tab to the start of the unpinned group", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("p1", { pinned: true }), tab("p2", { pinned: true }), tab("a")],
    });
    useNotebookTabsStore.getState().setPinned("p1", false);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "p2",
      "p1",
      "a",
    ]);
  });
});

describe("moveTab", () => {
  it("reorders unpinned tabs", () => {
    useNotebookTabsStore.setState({ tabs: [tab("a"), tab("b"), tab("c")] });
    useNotebookTabsStore.getState().moveTab("c", 0);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "c",
      "a",
      "b",
    ]);
  });

  it("clamps unpinned tabs out of the pinned region", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("p1", { pinned: true }), tab("a"), tab("b")],
    });
    useNotebookTabsStore.getState().moveTab("b", 0);
    expect(useNotebookTabsStore.getState().tabs.map((t) => t.id)).toEqual([
      "p1",
      "b",
      "a",
    ]);
  });
});

describe("attention", () => {
  it("marks a background tab when its run finishes", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("a", { kernelState: "running" }), tab("b")],
      activeTabId: "b",
    });
    useNotebookTabsStore.getState().updateTab("a", { kernelState: "live" });
    expect(useNotebookTabsStore.getState().tabs[0].attention).toBe(true);
  });

  it("does not mark the active tab", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("a", { kernelState: "running" })],
      activeTabId: "a",
    });
    useNotebookTabsStore.getState().updateTab("a", { kernelState: "live" });
    expect(useNotebookTabsStore.getState().tabs[0].attention).toBeFalsy();
  });

  it("clears attention when the tab becomes active", () => {
    useNotebookTabsStore.setState({
      tabs: [tab("a", { attention: true }), tab("b")],
      activeTabId: "b",
    });
    useNotebookTabsStore.getState().setActiveTabId("a");
    expect(useNotebookTabsStore.getState().tabs[0].attention).toBe(false);
  });
});

describe("closed-tab stack", () => {
  it("pops in LIFO order and caps at 10", () => {
    const store = useNotebookTabsStore.getState();
    for (let i = 0; i < 12; i += 1) {
      store.pushClosedTab({ tab: tab(`t${i}`), index: i });
    }
    expect(useNotebookTabsStore.getState().closedTabs).toHaveLength(10);
    expect(useNotebookTabsStore.getState().popClosedTab()?.tab.id).toBe("t11");
    expect(useNotebookTabsStore.getState().closedTabs).toHaveLength(9);
  });
});
