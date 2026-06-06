import { Command } from "cmdk";
import {
  ArrowDownIcon,
  ArrowUpIcon,
  FileTypeIcon,
  ListRestartIcon,
  ListVideoIcon,
  MessageSquareIcon,
  PackageIcon,
  PaletteIcon,
  PauseIcon,
  PencilIcon,
  PlayIcon,
  RefreshCwIcon,
  RotateCcw,
  SparklesIcon,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useStore } from "zustand";

import { dispatchDeckCommand } from "@/agent/deck";
import { useNotebook } from "@/stores/notebook";

import { publishSpurApp } from "./publishSpurApp";

type DeckPromptKind = "draft" | "restructure" | "polish" | "notes";

export default function NotebookCommandMenu() {
  const notebook = useNotebook();
  const [open, setOpen] = useState(false);
  const [activePrompt, setActivePrompt] = useState<{
    kind: DeckPromptKind;
    placeholder: string;
  } | null>(null);
  const [promptText, setPromptText] = useState("");
  const selectedCellId = useStore(
    notebook.store,
    (state) => state.viewState.selectedCellId,
  );
  const selectedCellType = useStore(notebook.store, (state) =>
    state.viewState.selectedCellId
      ? state.serverState.cells[state.viewState.selectedCellId]?.type
      : null,
  );
  const notebookPath = useStore(
    notebook.store,
    (state) => state.viewState.path,
  );

  const closeAndRun = useCallback((action: () => void) => {
    setOpen(false);
    action();
  }, []);

  const enterPresentMode = useCallback(() => {
    if (!notebookPath) return;
    closeAndRun(() => {
      window.location.hash = "";
      window.location.assign(
        `/present?path=${encodeURIComponent(notebookPath)}`,
      );
    });
  }, [closeAndRun, notebookPath]);

  const publishCurrentNotebook = useCallback(() => {
    if (!notebookPath) return;
    closeAndRun(() => {
      void publishSpurApp(notebook, notebookPath).catch((error) => {
        console.error("Failed to publish Spur App", error);
      });
    });
  }, [closeAndRun, notebook, notebookPath]);

  const openDeckPrompt = useCallback(
    (kind: DeckPromptKind, placeholder: string) => {
      setActivePrompt({ kind, placeholder });
      setPromptText("");
    },
    [],
  );

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen((open) => !open);
      } else if (e.key === "p" && (e.metaKey || e.ctrlKey) && e.shiftKey) {
        e.preventDefault();
        enterPresentMode();
      }
    };

    document.addEventListener("keydown", down);
    return () => document.removeEventListener("keydown", down);
  }, [enterPresentMode]);

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

        <Command.Group heading="Deck">
          <Command.Item disabled={!notebookPath} onSelect={enterPresentMode}>
            <PlayIcon /> Enter present mode
            <span style={{ marginLeft: "auto", opacity: 0.5 }}>⌘⇧P</span>
          </Command.Item>
          <Command.Item
            onSelect={() => openDeckPrompt("draft", "What's the deck about?")}
          >
            <SparklesIcon /> Draft deck with AI…
          </Command.Item>
          <Command.Item
            onSelect={() =>
              openDeckPrompt(
                "restructure",
                "Tighten to 8 slides; move conclusion last…",
              )
            }
          >
            <RefreshCwIcon /> Restructure deck…
          </Command.Item>
          <Command.Item
            onSelect={() =>
              openDeckPrompt(
                "polish",
                "Rewrite bullets for a non-technical audience",
              )
            }
          >
            <PencilIcon /> Polish slides for audience…
          </Command.Item>
          <Command.Item
            onSelect={() =>
              openDeckPrompt(
                "notes",
                "Tone of speaker notes (defaults to neutral)",
              )
            }
          >
            <MessageSquareIcon /> Generate speaker notes
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
          <Command.Item
            disabled={!notebookPath}
            onSelect={publishCurrentNotebook}
          >
            <PackageIcon />
            Publish Spur App...
          </Command.Item>
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

      {activePrompt && (
        <div style={{ padding: "12px", borderTop: "1px solid #e2e8f0" }}>
          <input
            autoFocus
            placeholder={activePrompt.placeholder}
            value={promptText}
            onChange={(e) => setPromptText(e.target.value)}
            onKeyDown={async (e) => {
              if (e.key === "Enter" && promptText.trim()) {
                e.preventDefault();
                e.stopPropagation();
                await dispatchDeckCommand(
                  notebook,
                  activePrompt.kind,
                  promptText.trim(),
                );
                setActivePrompt(null);
                setPromptText("");
                setOpen(false);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setActivePrompt(null);
              }
            }}
            style={{
              width: "100%",
              padding: "8px",
              border: "1px solid #cbd5e1",
              borderRadius: 4,
            }}
          />
          <div style={{ fontSize: 11, color: "#94a3b8", marginTop: 4 }}>
            Press Enter to send · Esc to cancel
          </div>
        </div>
      )}
    </Command.Dialog>
  );
}
