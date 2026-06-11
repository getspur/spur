import clsx from "clsx";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { useNotebook } from "@/stores/notebook";
import DagView from "@/ui/dag/DagView";
import { UnhandledError } from "@/ui/shared/UnhandledError";

import AppMode from "./AppMode";
import NotebookCells from "./NotebookCells";
import NotebookLocation from "./NotebookLocation";
import NotebookSidebar from "./sidebar/NotebookSidebar";

export default function NotebookView() {
  const notebook = useNotebook();

  const [path, loadError, viewMode] = useStore(
    notebook.store,
    useShallow((state) => [
      state.viewState.path,
      state.viewState.loadError,
      state.viewState.viewMode,
    ]),
  );

  // should be set to default home directory, and kernel should start there too
  let directory = "/fill/in_this_later";
  let filename: string | null = null;
  if (path) {
    const idx = path.lastIndexOf("/");
    directory = path.slice(0, idx);
    filename = path.slice(idx + 1);
  }

  const appMode = viewMode === "app";

  return (
    <div
      className={clsx(
        "grid h-full grid-rows-1 overflow-hidden",
        "grid-cols-[minmax(0,1fr),auto]",
      )}
    >
      <div
        className={clsx(
          "min-h-0 min-w-0",
          appMode ? "overflow-hidden" : "overflow-y-auto py-16",
        )}
      >
        {!appMode && (
          <NotebookLocation directory={directory} filename={filename} />
        )}
        {/* TODO: Handle these errors gracefully. */}
        {loadError ? (
          <UnhandledError error={loadError} />
        ) : viewMode === "dag" ? (
          <DagView />
        ) : viewMode === "app" ? (
          <AppMode />
        ) : (
          <NotebookCells />
        )}
      </div>
      <NotebookSidebar />
    </div>
  );
}
