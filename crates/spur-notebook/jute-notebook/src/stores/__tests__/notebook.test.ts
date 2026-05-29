import { describe, expect, test } from "vitest";

import type { RunCellEvent } from "@/bindings";

import {
  type NotebookStoreState,
  type RunCellEventApplicationState,
  applyRunCellEvent,
} from "../notebook";

describe("applyRunCellEvent", () => {
  test("applies a synthetic run event sequence to a cell result", () => {
    const cellId = "cell-1";
    const state: NotebookStoreState = {
      serverState: {
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
      },
      viewState: {
        selectedCellId: null,
        isLoading: false,
      },
      editBuffer: {
        cellSources: {},
      },
    };
    let runState: RunCellEventApplicationState = {
      status: "running",
      timings: { startedAt: 100 },
      executionCount: undefined,
      willClearOutput: false,
    };

    const apply = (event: RunCellEvent) => {
      runState = applyRunCellEvent(state.serverState, cellId, event, runState);
    };

    apply({ event: "started" });
    apply({ event: "stdout", data: "hello\n" });
    apply({ event: "stderr", data: "warning\n" });
    apply({ event: "clear_output", data: { wait: true } });
    apply({
      event: "display_data",
      data: {
        data: { "text/plain": "original" },
        metadata: {},
        transient: { display_id: "display-1" },
      },
    });
    apply({
      event: "update_display_data",
      data: {
        data: { "text/plain": "updated" },
        metadata: { isolated: true },
        transient: { display_id: "display-1" },
      },
    });

    expect(state.serverState.cells[cellId].result?.outputs).toEqual([
      {
        output_type: "display_data",
        data: { "text/plain": "updated" },
        metadata: { isolated: true },
      },
    ]);
    expect(state.serverState.cells[cellId].result?.displays).toEqual({
      "display-1": 0,
    });

    apply({
      event: "execute_result",
      data: {
        execution_count: 3,
        data: { "text/plain": "3" },
        metadata: {},
      },
    });
    apply({ event: "clear_output", data: { wait: false } });
    apply({
      event: "error",
      data: {
        ename: "ValueError",
        evalue: "bad value",
        traceback: ["traceback"],
      },
    });
    apply({ event: "finished", data: { status: "ok", exec_count: null } });

    expect(runState).toEqual({
      status: "success",
      timings: { startedAt: 100 },
      executionCount: 3,
      willClearOutput: false,
    });
    expect(state.serverState.cells[cellId].result).toEqual({
      status: "success",
      timings: { startedAt: 100 },
      executionCount: 3,
      outputs: [
        {
          output_type: "error",
          ename: "ValueError",
          evalue: "bad value",
          traceback: ["traceback"],
        },
      ],
      displays: {},
    });
  });
});
