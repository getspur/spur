import { Command } from "cmdk";
import {
  ArrowDownIcon,
  ArrowUpIcon,
  FileTypeIcon,
  ListRestartIcon,
  ListVideoIcon,
  PaletteIcon,
  PauseIcon,
  PlayIcon,
  RotateCcw,
} from "lucide-react";
import { useEffect, useState } from "react";
import { useStore } from "zustand";

import { useNotebook } from "@/stores/notebook";

export default function NotebookCommandMenu() {
  const notebook = useNotebook();
  const [open, setOpen] = useState(false);
  const selectedCellId = useStore(
    notebook.store,
    (state) => state.selectedCellId,
  );
  const selectedCellType = useStore(notebook.store, (state) =>
    state.selectedCellId ? state.cells[state.selectedCellId]?.type : null,
  );

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen((open) => !open);
      }
    };

    document.addEventListener("keydown", down);
    return () => document.removeEventListener("keydown", down);
  }, []);

  const closeAndRun = (action: () => void) => {
    setOpen(false);
    action();
  };

  return (
    <Command.Dialog open={open} onOpenChange={setOpen}>
      <Command.Input autoFocus placeholder="Search for an action…" />

      <Command.List>
        {/* {loading && <Command.Loading>Hang on…</Command.Loading>} */}

        <Command.Empty>No results found.</Command.Empty>

        <Command.Group heading="Execution">
          <Command.Item
            disabled={selectedCellId === null}
            onSelect={() => {
              if (!selectedCellId) return;
              closeAndRun(() => void notebook.execute(selectedCellId));
            }}
          >
            <PlayIcon /> Run cell
          </Command.Item>
          {/* TODO: Wire when the notebook store exposes run-all execution. */}
          <Command.Item disabled>
            <ListVideoIcon /> Run all cells
          </Command.Item>
          <Command.Item
            onSelect={() => closeAndRun(() => void notebook.interruptKernel())}
          >
            <PauseIcon />
            Interrupt kernel
          </Command.Item>
          <Command.Item
            onSelect={() => closeAndRun(() => void notebook.restartKernel())}
          >
            <RotateCcw />
            Restart kernel
          </Command.Item>
          {/* TODO: Wire when run-all execution exists after kernel restart. */}
          <Command.Item disabled>
            <ListRestartIcon />
            Restart kernel and run all cells
          </Command.Item>
        </Command.Group>

        <Command.Group heading="Formatting">
          {/* TODO: Wire when the notebook store exposes Black formatting. */}
          <Command.Item disabled>
            <PaletteIcon />
            Format code with Black
          </Command.Item>
        </Command.Group>

        <Command.Group heading="Notebook">
          {/* TODO: Wire when the notebook store exposes cell reordering. */}
          <Command.Item disabled>
            <ArrowUpIcon />
            Move cell up
          </Command.Item>
          {/* TODO: Wire when the notebook store exposes cell reordering. */}
          <Command.Item disabled>
            <ArrowDownIcon />
            Move cell down
          </Command.Item>
          <Command.Item
            disabled={selectedCellId === null}
            onSelect={() => {
              if (!selectedCellId || !selectedCellType) return;
              closeAndRun(() =>
                notebook.setCellType(
                  selectedCellId,
                  selectedCellType === "code" ? "markdown" : "code",
                ),
              );
            }}
          >
            <FileTypeIcon />
            Change cell type
          </Command.Item>
        </Command.Group>
      </Command.List>
    </Command.Dialog>
  );
}
