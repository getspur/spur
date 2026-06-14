import { describe, expect, test, vi } from "vitest";

import type { CellCronTrigger } from "@/bindings/CellCronTrigger";

import {
  listSchedules,
  removeCellSchedule,
  setCellSchedule,
} from "./scheduleApi";

const trigger: CellCronTrigger = {
  enabled: true,
  cron: "*/15 * * * *",
  timezone: "UTC",
  run_target: "cascade",
  skip_if_running: true,
  catch_up: false,
};

describe("scheduleApi", () => {
  test("sets a cell schedule with camelCase Tauri args", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);

    await setCellSchedule("cell-1", trigger, 7, invoke);

    expect(invoke).toHaveBeenCalledWith("notebook_set_cell_schedule", {
      cellId: "cell-1",
      trigger,
      expectedVersion: 7,
    });
  });

  test("removes a cell schedule with camelCase Tauri args", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);

    await removeCellSchedule("cell-1", 7, invoke);

    expect(invoke).toHaveBeenCalledWith("notebook_remove_cell_schedule", {
      cellId: "cell-1",
      expectedVersion: 7,
    });
  });

  test("lists schedules", async () => {
    const entries = [{ cell_id: "cell-1", trigger }];
    const invoke = vi.fn().mockResolvedValue(entries);

    await expect(listSchedules(invoke)).resolves.toBe(entries);

    expect(invoke).toHaveBeenCalledWith("notebook_list_schedules", {});
  });
});
