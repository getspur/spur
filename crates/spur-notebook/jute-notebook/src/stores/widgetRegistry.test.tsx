import { afterEach, describe, expect, test, vi } from "vitest";

import type { RunCellEvent } from "@/bindings";

import {
  type NotebookServerState,
  type RunCellEventApplicationState,
  applyRunCellEvent,
} from "./notebook";
import { dispose, emit, get, off, on, set } from "./widgetRegistry";

const touchedModelIds = new Set<string>();

function track(modelId: string): string {
  touchedModelIds.add(modelId);
  return modelId;
}

afterEach(() => {
  for (const modelId of touchedModelIds) {
    dispose(modelId);
  }
  touchedModelIds.clear();
});

function serverStateWithRunningCell(cellId: string): NotebookServerState {
  return {
    lastAppliedVersion: 0,
    notebookMetadata: {},
    cellIds: [cellId],
    cells: {
      [cellId]: {
        type: "code",
        initialText: "",
        source: "",
        version: 1,
        result: {
          status: "running",
          timings: { startedAt: 100 },
          executionCount: undefined,
          outputs: [],
          displays: {},
        },
      },
    },
  };
}

function runningState(): RunCellEventApplicationState {
  return {
    status: "running",
    timings: { startedAt: 100 },
    executionCount: undefined,
    willClearOutput: false,
  };
}

function commOpen(
  modelId: string,
  data: Extract<RunCellEvent, { event: "comm_open" }>["data"]["data"],
): RunCellEvent {
  return {
    event: "comm_open",
    data: {
      comm_id: modelId,
      target_name: "jupyter.widget",
      data,
      buffers: [],
    },
  };
}

describe("widgetRegistry", () => {
  test("stores models and notifies change listeners from set", () => {
    const modelId = track("registry-model");
    const listener = vi.fn();

    on(modelId, "change", listener);
    set(modelId, {
      state: { value: 1 },
      esm: "export default { render() {} }",
      css: ".widget { color: red; }",
    });

    expect(get(modelId)).toMatchObject({
      state: { value: 1 },
      esm: "export default { render() {} }",
      css: ".widget { color: red; }",
    });
    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith(get(modelId));
  });

  test("supports on/off/emit and clears listeners on dispose", () => {
    const modelId = track("registry-events");
    const listener = vi.fn();

    on(modelId, "msg:custom", listener);
    emit(modelId, "msg:custom", { ping: true }, [[1, 2, 3]]);
    expect(listener).toHaveBeenCalledWith({ ping: true }, [[1, 2, 3]]);

    off(modelId, "msg:custom", listener);
    emit(modelId, "msg:custom", { ping: false }, []);
    expect(listener).toHaveBeenCalledTimes(1);

    on(modelId, "msg:custom", listener);
    dispose(modelId);
    expect(get(modelId)).toBeUndefined();
    emit(modelId, "msg:custom", { afterDispose: true }, []);
    expect(listener).toHaveBeenCalledTimes(1);
  });
});

describe("applyRunCellEvent comm handling", () => {
  test("comm_open registers a widget model with initial state and assets", () => {
    const cellId = "cell-1";
    const modelId = track("comm-open-model");
    const state = serverStateWithRunningCell(cellId);
    const runState = runningState();

    const nextRunState = applyRunCellEvent(
      state,
      cellId,
      commOpen(modelId, {
        state: {
          value: 42,
          label: "forecast",
          _esm: "export default { render() {} }",
          _css: ".forecast { display: block; }",
        },
      }),
      runState,
    );

    expect(nextRunState).toEqual(runState);
    expect(state.cells[cellId].result?.outputs).toEqual([]);
    expect(get(modelId)).toMatchObject({
      state: { value: 42, label: "forecast" },
      esm: "export default { render() {} }",
      css: ".forecast { display: block; }",
    });
  });

  test("comm_msg update merges state and reinserts buffers by buffer path", () => {
    const cellId = "cell-1";
    const modelId = track("comm-update-model");
    const state = serverStateWithRunningCell(cellId);
    const runState = runningState();
    const listener = vi.fn();

    applyRunCellEvent(
      state,
      cellId,
      commOpen(modelId, {
        state: {
          value: 1,
          preserved: "yes",
          _esm: "export default { render() {} }",
        },
      }),
      runState,
    );
    on(modelId, "change", listener);

    applyRunCellEvent(
      state,
      cellId,
      {
        event: "comm_msg",
        data: {
          comm_id: modelId,
          data: {
            method: "update",
            state: {
              value: 2,
              payload: null,
              nested: { bytes: null },
              _css: ".updated { color: blue; }",
            },
            buffer_paths: [["payload"], ["nested", "bytes"]],
          },
          buffers: [
            [1, 2, 3],
            [4, 5],
          ],
        },
      },
      runState,
    );

    expect(get(modelId)).toMatchObject({
      state: {
        value: 2,
        preserved: "yes",
        payload: [1, 2, 3],
        nested: { bytes: [4, 5] },
      },
      esm: "export default { render() {} }",
      css: ".updated { color: blue; }",
    });
    expect(listener).toHaveBeenCalledWith(get(modelId));
  });

  test("comm_msg custom delivers content and buffers to model listeners", () => {
    const cellId = "cell-1";
    const modelId = track("comm-custom-model");
    const state = serverStateWithRunningCell(cellId);
    const runState = runningState();
    const listener = vi.fn();

    applyRunCellEvent(
      state,
      cellId,
      commOpen(modelId, { state: { value: 1 } }),
      runState,
    );
    on(modelId, "msg:custom", listener);

    applyRunCellEvent(
      state,
      cellId,
      {
        event: "comm_msg",
        data: {
          comm_id: modelId,
          data: {
            method: "custom",
            content: { type: "port.updated", port: "forecast" },
          },
          buffers: [[9, 8, 7]],
        },
      },
      runState,
    );

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith(
      { type: "port.updated", port: "forecast" },
      [[9, 8, 7]],
    );
  });

  test("comm_close disposes the widget model", () => {
    const cellId = "cell-1";
    const modelId = track("comm-close-model");
    const state = serverStateWithRunningCell(cellId);
    const runState = runningState();

    applyRunCellEvent(
      state,
      cellId,
      commOpen(modelId, { state: { value: 1 } }),
      runState,
    );
    expect(get(modelId)).toBeDefined();

    applyRunCellEvent(
      state,
      cellId,
      {
        event: "comm_close",
        data: {
          comm_id: modelId,
          data: {},
          buffers: [],
        },
      },
      runState,
    );

    expect(get(modelId)).toBeUndefined();
  });
});
