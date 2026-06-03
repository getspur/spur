import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import DagInspector from "./DagInspector";
import type { DagNodeData } from "./useDagGraph";

type TestStoreState = {
  serverState: {
    cells: Record<string, { source: string }>;
  };
  editBuffer: {
    cellSources: Record<string, { source: string }>;
  };
};

const storeListeners = vi.hoisted(() => new Set<() => void>());
const storeState = vi.hoisted<TestStoreState>(() => ({
  serverState: {
    cells: {},
  },
  editBuffer: {
    cellSources: {},
  },
}));

vi.mock("@/daemon/control", () => ({
  daemonControl: vi.fn(),
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => ({
    store: {
      getInitialState: () => storeState,
      getState: () => storeState,
      subscribe: (listener: () => void) => {
        storeListeners.add(listener);
        return () => storeListeners.delete(listener);
      },
    },
  }),
}));

vi.mock("./dagStatus", () => ({
  runNotebookCascade: vi.fn(),
  runNotebookCell: vi.fn(),
}));

vi.mock("../notebook/CellInput", () => ({
  default: ({ cellId }: { cellId: string }) => (
    <textarea
      aria-label="Selected DAG node code"
      defaultValue={storeState.serverState.cells[cellId]?.source ?? ""}
    />
  ),
}));

afterEach(() => {
  cleanup();
  storeListeners.clear();
  storeState.serverState.cells = {};
  storeState.editBuffer.cellSources = {};
});

function data(overrides: Partial<DagNodeData> = {}): DagNodeData {
  return {
    id: "ai-summary",
    kind: "ai",
    aiLive: true,
    label: "Summary prompt",
    cellType: "code",
    code: "Summarise sales vs targets.",
    codePreview: "Summarise sales vs targets.",
    produces: [],
    consumes: [],
    state: "fresh",
    ...overrides,
  };
}

function renderInspector(node: DagNodeData) {
  storeState.serverState.cells[node.id] = { source: node.code };

  return render(<DagInspector node={node} portManifest={{}} />);
}

describe("DagInspector", () => {
  test("renders AI badge, disabled live mode, and prompt heading for AI nodes", () => {
    renderInspector(data({ aiLive: true }));

    expect(screen.getByText("✦ AI")).toBeInTheDocument();
    expect(screen.getByText("Prompt")).toBeInTheDocument();
    expect(screen.queryByText("Code")).not.toBeInTheDocument();

    const modeControl = screen.getByTitle(
      "Live auto-run requires backend wiring (bd-1bpb)",
    );
    expect(modeControl).toHaveClass("opacity-60");

    const manual = screen.getByRole("button", { name: "manual" });
    const live = screen.getByRole("button", { name: "live" });
    expect(manual).toBeDisabled();
    expect(manual).toHaveAttribute("aria-pressed", "false");
    expect(live).toBeDisabled();
    expect(live).toHaveAttribute("aria-pressed", "true");
  });

  test("keeps code nodes on the code heading without AI controls", () => {
    renderInspector(
      data({
        id: "code-cell",
        kind: "code",
        aiLive: false,
        label: "Code cell",
        code: "summary = sales.describe()",
      }),
    );

    expect(screen.queryByText("✦ AI")).not.toBeInTheDocument();
    expect(screen.queryByText("Mode")).not.toBeInTheDocument();
    expect(
      screen.queryByTitle("Live auto-run requires backend wiring (bd-1bpb)"),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Code")).toBeInTheDocument();
    expect(screen.queryByText("Prompt")).not.toBeInTheDocument();
  });
});
