import { type UnlistenFn, listen } from "@tauri-apps/api/event";

import type { NotebookDelta, RunCellEvent } from "@/bindings";
import {
  type Notebook,
  loadNotebookRuntimeConfig,
  reconcileNotebookDelta,
} from "@/stores/notebook";

type RunCellEventPayload = {
  cell_id: string;
  kernel_id: string;
  event: RunCellEvent;
};

type SavedPayload = {
  path?: string;
};

function listenForAll(
  registrations: Promise<UnlistenFn>[],
  label: string,
): () => void {
  let disposed = false;
  let unlisteners: UnlistenFn[] = [];

  void Promise.allSettled(registrations).then((results) => {
    for (const result of results) {
      if (result.status === "fulfilled") {
        if (disposed) {
          result.value();
        } else {
          unlisteners.push(result.value);
        }
      } else {
        console.error(
          `Failed to register ${label} event listener`,
          result.reason,
        );
      }
    }
  });

  return () => {
    disposed = true;
    for (const unlisten of unlisteners) {
      unlisten();
    }
    unlisteners = [];
  };
}

function runAsync(label: string, action: () => Promise<void>) {
  try {
    void action().catch((error) => {
      console.error(`Failed to handle ${label} event`, error);
    });
  } catch (error) {
    console.error(`Failed to handle ${label} event`, error);
  }
}

export function listenForNotebookEvents(notebook: Notebook): () => void {
  const registrations = [
    listen<RunCellEventPayload>("notebook://run_cell_event", (event) => {
      if (event.payload.kernel_id !== notebook.state.kernelId) return;
      notebook.handleRunCellEvent(event.payload.cell_id, event.payload.event);
    }),
    listen("notebook://kernel_changed", () => {
      runAsync("notebook://kernel_changed", async () => {
        await notebook.refreshKernelSlotInfo();
      });
    }),
    listen<SavedPayload>("notebook://saved", (event) => {
      // TODO: Clear the unsaved indicator once notebook dirty state exists.
      void event.payload;
    }),
  ];

  // Warm the runtime-config cache so the first delta is gated on the
  // Rust-resolved flag rather than the env fallback.
  void loadNotebookRuntimeConfig();
  registrations.push(
    listen<NotebookDelta>("notebook://changed", (event) => {
      runAsync("notebook://changed", async () => {
        await reconcileNotebookDelta(notebook, event.payload);
      });
    }),
  );

  return listenForAll(registrations, "notebook");
}

export function listenForRecentNotebookChanges(
  refreshRecents: () => Promise<void>,
): () => void {
  return listenForAll(
    [
      listen("notebook://recents_changed", () => {
        runAsync("notebook://recents_changed", refreshRecents);
      }),
    ],
    "recent notebook",
  );
}
