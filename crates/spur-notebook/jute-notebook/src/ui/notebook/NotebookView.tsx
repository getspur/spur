import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { useNotebook } from "@/stores/notebook";
import DagView from "@/ui/dag/DagView";
import { UnhandledError } from "@/ui/shared/UnhandledError";

import DatasourceSidebar from "./DatasourceSidebar";
import NotebookCells from "./NotebookCells";
import NotebookLocation from "./NotebookLocation";

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

  return (
    <div className="grid h-full grid-rows-1 grid-cols-[minmax(0,1fr),auto] overflow-hidden">
      <div className="min-h-0 min-w-0 overflow-y-auto py-16">
        <NotebookLocation directory={directory} filename={filename} />

        {/* TODO: Handle these errors gracefully. */}
        {loadError ? (
          <UnhandledError error={loadError} />
        ) : viewMode === "dag" ? (
          <DagView />
        ) : (
          <NotebookCells />
        )}
      </div>
      <DatasourceSidebar />
    </div>
  );
}
