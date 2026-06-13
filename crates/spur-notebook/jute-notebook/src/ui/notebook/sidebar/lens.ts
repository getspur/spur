import type { NotebookOpenInfo, NotebookViewMode } from "@/stores/notebook";

export type ChatLens =
  | "notebook_builder"
  | "notebook_deep_dive"
  | "dag_ops"
  | "app_product";

export type LensViewMode = "notebook" | "dag" | "app";

export function mapViewMode(mode: NotebookViewMode): LensViewMode {
  return mode === "cells" ? "notebook" : mode;
}

export function defaultLensFor(
  mode: NotebookViewMode,
  appOpenInfo?: NotebookOpenInfo,
): ChatLens {
  switch (mode) {
    case "dag":
      return "dag_ops";
    case "app":
      return appOpenInfo ? "app_product" : "notebook_deep_dive";
    case "cells":
      return "notebook_builder";
  }
}

export const EMPTY_STATE_COPY: Record<
  ChatLens,
  { heading: string; copy: string }
> = {
  app_product: {
    copy: "Ask about workflow, UI quality, copy, or product behavior.",
    heading: "Improve this app",
  },
  dag_ops: {
    copy: "Ask about failed nodes, stale dependencies, or recomputation order.",
    heading: "Operate this graph",
  },
  notebook_builder: {
    copy: "Ask for the next cell, a cleaner analysis path, or stronger explanation.",
    heading: "Build on this notebook",
  },
  notebook_deep_dive: {
    copy: "Ask how the cells, outputs, and assumptions fit together.",
    heading: "Understand this notebook",
  },
};

const COMPOSER_LENS_LABELS: Record<ChatLens, string> = {
  app_product: "Product",
  dag_ops: "Operations",
  notebook_builder: "Builder",
  notebook_deep_dive: "Deep dive",
};

export function composerLensLabel(lens: ChatLens): string {
  return `${COMPOSER_LENS_LABELS[lens]} lens`;
}
