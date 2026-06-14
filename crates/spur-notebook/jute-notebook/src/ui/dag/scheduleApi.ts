import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import type { CellCronTrigger } from "@/bindings/CellCronTrigger";

export type ScheduleSnapshotEntry = {
  cell_id: string;
  trigger: CellCronTrigger;
  next_fire?: string | null;
  last_run?: {
    fired_at: string;
    status: string;
    duration_ms?: number | null;
    error?: string | null;
  } | null;
  consecutive_failures?: number;
  recent?: unknown[];
};

type SetScheduleArgs = {
  cellId: string;
  trigger: CellCronTrigger;
  expectedVersion: number;
};

type RemoveScheduleArgs = {
  cellId: string;
  expectedVersion: number;
};

type Invoke = (
  command: string,
  args: Record<string, never> | SetScheduleArgs | RemoveScheduleArgs,
) => Promise<unknown>;

export async function setCellSchedule(
  cellId: string,
  trigger: CellCronTrigger,
  expectedVersion: number,
  invoke: Invoke = tauriInvoke,
): Promise<void> {
  await invoke("notebook_set_cell_schedule", {
    cellId,
    trigger,
    expectedVersion,
  });
}

export async function removeCellSchedule(
  cellId: string,
  expectedVersion: number,
  invoke: Invoke = tauriInvoke,
): Promise<void> {
  await invoke("notebook_remove_cell_schedule", {
    cellId,
    expectedVersion,
  });
}

export async function listSchedules(
  invoke: Invoke = tauriInvoke,
): Promise<ScheduleSnapshotEntry[]> {
  return (await invoke(
    "notebook_list_schedules",
    {},
  )) as ScheduleSnapshotEntry[];
}
