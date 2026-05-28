import { invoke } from "@tauri-apps/api/core";

import type {
  DaemonControlCommand,
  DaemonControlResponse,
  RecentNotebookEntry,
} from "@/bindings";

type PathCommandName = "open" | "rename" | "new" | "new_at" | "reopen";
type EnrichedRecentEntry = NonNullable<
  DaemonControlResponse["entries"]
>[number] &
  Partial<Pick<RecentNotebookEntry, "kernelAlive" | "isCurrent">>;

export async function daemonControl(
  cmd: DaemonControlCommand,
): Promise<DaemonControlResponse> {
  return await invoke<DaemonControlResponse>("daemon_control", { cmd });
}

export function pathFromDaemonControlResponse(
  response: DaemonControlResponse,
  command: PathCommandName,
): string {
  if (response.path) return response.path;
  throw new Error(`daemon ${command} response did not include path`);
}

export function recentEntriesFromDaemonControlResponse(
  response: DaemonControlResponse,
): RecentNotebookEntry[] {
  return (response.entries ?? []).map((entry) => {
    const enriched = entry as EnrichedRecentEntry;
    return {
      path: entry.path,
      lastOpened: entry.lastOpened,
      isScratch: entry.isScratch,
      pinned: entry.pinned,
      kernelAlive: enriched.kernelAlive ?? false,
      isCurrent: enriched.isCurrent ?? false,
    };
  });
}
