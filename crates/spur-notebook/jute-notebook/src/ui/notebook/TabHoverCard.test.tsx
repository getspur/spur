import "@testing-library/jest-dom/vitest";
import { act, render, renderHook, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";

import type { NotebookTab } from "@/stores/notebook";
import TabHoverCard, { useTabHoverDelay } from "@/ui/notebook/TabHoverCard";

const tab: NotebookTab = {
  id: "~/notebooks/etl.ipynb",
  path: "~/notebooks/etl.ipynb",
  title: "etl.ipynb",
  dirty: false,
  kernelState: "running",
  kernelGeneration: 2,
  mode: "cells",
};

it("renders kernel and resource lines", () => {
  render(
    <TabHoverCard
      anchor={{ left: 10, bottom: 40 }}
      stats={{
        kernel_id: "k",
        spec_name: "python3",
        generation: 2,
        status: "alive",
        cpu_pct: 64.2,
        mem_mb: 1212,
      }}
      tab={tab}
    />,
  );
  expect(screen.getByText("running · gen 2")).toBeInTheDocument();
  expect(screen.getByText("64% CPU · 1212 MB")).toBeInTheDocument();
});

it("shows placeholder resources without stats and none for idle kernels", () => {
  render(
    <TabHoverCard
      anchor={{ left: 0, bottom: 0 }}
      stats={null}
      tab={{ ...tab, kernelState: "idle", kernelGeneration: undefined }}
    />,
  );
  expect(screen.getByText("none")).toBeInTheDocument();
  expect(screen.getByText("·")).toBeInTheDocument();
});

it("arms the hover card only after the delay", () => {
  vi.useFakeTimers();
  const { result } = renderHook(() => useTabHoverDelay(350));
  act(() => {
    result.current.onTabEnter("t1", { left: 5, bottom: 30 });
  });
  expect(result.current.hoveredTabId).toBeUndefined();
  act(() => {
    vi.advanceTimersByTime(350);
  });
  expect(result.current.hoveredTabId).toBe("t1");
  act(() => {
    result.current.onTabLeave();
  });
  expect(result.current.hoveredTabId).toBeUndefined();
  vi.useRealTimers();
});
