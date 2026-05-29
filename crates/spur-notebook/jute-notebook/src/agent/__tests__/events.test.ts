import { afterEach, describe, expect, test, vi } from "vitest";

import type { NotebookDelta, RunCellEvent } from "@/bindings";
import { Notebook } from "@/stores/notebook";

import { listenForNotebookEvents } from "../events";

type EventCallback<T> = (event: { payload: T }) => void;

const invokeMock = vi.hoisted(() => vi.fn());
const listeners = vi.hoisted(
  () => new Map<string, Array<EventCallback<unknown>>>(),
);

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class<T> {
    onmessage?: (message: T) => void;
  },
  invoke: invokeMock,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((eventName: string, callback: EventCallback<unknown>) => {
    const callbacks = listeners.get(eventName) ?? [];
    callbacks.push(callback);
    listeners.set(eventName, callbacks);
    return Promise.resolve(() => {
      const current = listeners.get(eventName) ?? [];
      listeners.set(
        eventName,
        current.filter((candidate) => candidate !== callback),
      );
    });
  }),
}));

describe("listenForNotebookEvents", () => {
  afterEach(() => {
    invokeMock.mockReset();
    listeners.clear();
  });

  test("applies run events from notebook deltas", async () => {
    const cellId = "cell-1";
    const runEvent: RunCellEvent = { event: "stdout", data: "hello\n" };

    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "start_kernel") return "kernel-1";
      if (command === "kernel_slot_info") {
        expect(args).toEqual({ kernelId: "kernel-1" });
        return {
          kernel_id: "kernel-1",
          spec_name: "python3",
          generation: 1,
          status: "idle",
          cpu_pct: 0,
          mem_mb: 0,
        };
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const notebook = new Notebook();
    notebook.loadNotebook({
      metadata: {},
      nbformat_minor: 5,
      nbformat: 4,
      cells: [
        {
          cell_type: "code",
          id: cellId,
          metadata: { spur: { version: 1 } },
          source: "",
          execution_count: null,
          outputs: [],
        },
      ],
    });
    await notebook.kernelStartPromise;

    listenForNotebookEvents(notebook);
    await flushPromises();

    emit("notebook://changed", {
      version: 2,
      kind: { type: "runCellEvent", cell_id: cellId, event: runEvent },
    } satisfies NotebookDelta);
    await flushPromises();

    expect(notebook.store.getState().cells[cellId].result?.outputs).toEqual([
      { output_type: "stream", name: "stdout", text: "hello\n" },
    ]);
  });
});

function emit<T>(eventName: string, payload: T) {
  for (const callback of listeners.get(eventName) ?? []) {
    (callback as EventCallback<T>)({ payload });
  }
}

async function flushPromises() {
  await Promise.resolve();
  await Promise.resolve();
}
