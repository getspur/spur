import { describe, expect, it } from "vitest";

import {
  closeOthersTargets,
  closeRightTargets,
  cycleTabId,
  jumpTabId,
} from "@/pages/tabActions";
import type { NotebookTab } from "@/stores/notebook";

const tab = (id: string, patch: Partial<NotebookTab> = {}): NotebookTab => ({
  id,
  title: id,
  dirty: false,
  kernelState: "idle",
  mode: "cells",
  ...patch,
});

const tabs = [tab("p", { pinned: true }), tab("a"), tab("b"), tab("c")];

describe("tab actions", () => {
  it("close others keeps the target and all pinned tabs", () => {
    expect(closeOthersTargets(tabs, "b")).toEqual(["a", "c"]);
  });

  it("close to the right excludes pinned tabs", () => {
    expect(closeRightTargets(tabs, "a")).toEqual(["b", "c"]);
    expect(closeRightTargets(tabs, "c")).toEqual([]);
  });

  it("cycles forward and backward with wrap-around", () => {
    expect(cycleTabId(tabs, "c", 1)).toBe("p");
    expect(cycleTabId(tabs, "p", -1)).toBe("c");
  });

  it("jumps 1-8 by index and 9 to the last tab", () => {
    expect(jumpTabId(tabs, 1)).toBe("p");
    expect(jumpTabId(tabs, 9)).toBe("c");
    expect(jumpTabId(tabs, 8)).toBeUndefined();
  });
});
