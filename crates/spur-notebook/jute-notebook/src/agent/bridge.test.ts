import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Notebook } from "@/stores/notebook";

import type { AgentBridgeRequest } from "./types";

type NotebookConstructor = new () => Notebook;

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listener: undefined as
    | ((event: { payload: AgentBridgeRequest }) => void | Promise<void>)
    | undefined,
  releaseDispatch: undefined as (() => void) | undefined,
  seenNotebookSources: [] as (string | undefined)[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, callback: typeof mocks.listener) => {
    mocks.listener = callback;
    return vi.fn();
  }),
}));

vi.mock("./handlers", () => ({
  dispatchAgentRequest: vi.fn(async (notebook: Notebook | undefined) => {
    await new Promise<void>((resolve) => {
      mocks.releaseDispatch = resolve;
    });
    mocks.seenNotebookSources.push(
      notebook?.state.serverState.cells["cell-1"]?.source,
    );
    return { ok: true };
  }),
}));

describe("agent bridge focus binding", () => {
  beforeEach(() => {
    vi.resetModules();
    mocks.invoke.mockReset();
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_kernel") return Promise.resolve("kernel-1");
      if (command === "kernel_slot_info") {
        return Promise.resolve({
          kernel_id: "kernel-1",
          spec_name: "python3",
          generation: 1,
          status: "idle",
          cpu_pct: 0,
          mem_mb: 0,
        });
      }
      return Promise.resolve(undefined);
    });
    mocks.listener = undefined;
    mocks.releaseDispatch = undefined;
    mocks.seenNotebookSources.length = 0;
  });

  it("uses the request-start notebook for the whole in-flight request", async () => {
    const { registerAgentBridge, setActiveAgentNotebook } =
      await import("./bridge");
    const { Notebook } = await import("@/stores/notebook");
    await registerAgentBridge();

    const notebookA = notebookWithSource(Notebook, "A");
    const notebookB = notebookWithSource(Notebook, "B");
    setActiveAgentNotebook(notebookA);

    const requestPromise = mocks.listener!({
      payload: {
        requestId: "req-1",
        method: "notebook.snapshot",
        params: {},
      },
    });
    setActiveAgentNotebook(notebookB);
    mocks.releaseDispatch!();
    await requestPromise;

    expect(mocks.seenNotebookSources).toEqual(["A"]);
    expect(mocks.invoke).toHaveBeenCalledWith("agent_response", {
      payload: {
        requestId: "req-1",
        result: { ok: true },
      },
    });
  });
});

function notebookWithSource(
  NotebookCtor: NotebookConstructor,
  source: string,
): Notebook {
  const Notebook = NotebookCtor;
  const notebook = new Notebook();
  notebook.applyNotebookDelta({
    version: 1,
    kind: {
      type: "loaded",
      root: {
        nbformat: 4,
        nbformat_minor: 5,
        metadata: {},
        cells: [
          {
            cell_type: "code",
            id: "cell-1",
            source,
            execution_count: null,
            outputs: [],
            metadata: {},
          },
        ],
      },
    },
  });
  return notebook;
}
