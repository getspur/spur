import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { formatDistanceToNow } from "date-fns";
import {
  ArrowRight,
  ArrowUp,
  Circle,
  FolderOpen,
  Pin,
  PinOff,
  Plus,
  Trash2,
} from "lucide-react";
import {
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from "react";
import { useLocation } from "wouter";

import type { RecentNotebookEntry } from "@/bindings";
import Header from "@/ui/shared/Header";

type ContextMenuState = {
  entry: RecentNotebookEntry;
  x: number;
  y: number;
} | null;

function notebookUrl(path: string) {
  return "/notebook?" + new URLSearchParams({ path }).toString();
}

function normalizeSeparators(path: string) {
  return path.replace(/\\/g, "/");
}

function notebookFilename(path: string) {
  const normalized = normalizeSeparators(path);
  const index = normalized.lastIndexOf("/");
  return index >= 0 ? normalized.slice(index + 1) : normalized;
}

function notebookParentPath(path: string) {
  const normalized = normalizeSeparators(path);
  const index = normalized.lastIndexOf("/");
  if (index > 0) return normalized.slice(0, index);
  if (index === 0) return "/";
  return "Unknown folder";
}

function relativeLastOpened(lastOpened: string) {
  const date = new Date(lastOpened);
  if (Number.isNaN(date.getTime())) return "Recently";
  return formatDistanceToNow(date, { addSuffix: true });
}

function sortRecentEntries(entries: RecentNotebookEntry[]) {
  return [...entries].sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;

    const aTime = new Date(a.lastOpened).getTime();
    const bTime = new Date(b.lastOpened).getTime();
    if (aTime !== bTime) return bTime - aTime;

    return a.path.localeCompare(b.path);
  });
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function KernelDot({ alive }: { alive: boolean }) {
  return (
    <Circle
      aria-label={alive ? "Kernel alive" : "Kernel inactive"}
      className={alive ? "text-green-500" : "text-gray-300"}
      fill="currentColor"
      size={10}
      strokeWidth={1.5}
    />
  );
}

type NotebookCardProps = {
  compact?: boolean;
  entry: RecentNotebookEntry;
  onContextMenu: (
    event: MouseEvent<HTMLElement>,
    entry: RecentNotebookEntry,
  ) => void;
  onOpen: (entry: RecentNotebookEntry) => void;
  onTogglePinned: (entry: RecentNotebookEntry) => void;
};

function NotebookCard({
  compact = false,
  entry,
  onContextMenu,
  onOpen,
  onTogglePinned,
}: NotebookCardProps) {
  const handleKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onOpen(entry);
    }
  };

  const PinIcon = entry.pinned ? Pin : PinOff;

  return (
    <article
      className={[
        "group flex cursor-pointer flex-col rounded border border-gray-300 transition-colors hover:border-black",
        compact ? "min-h-28 p-3" : "min-h-44 p-4",
      ].join(" ")}
      onClick={() => onOpen(entry)}
      onContextMenu={(event) => onContextMenu(event, entry)}
      onKeyDown={handleKeyDown}
      role="button"
      tabIndex={0}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3
            className={[
              "truncate text-gray-950",
              compact ? "text-base" : "text-xl",
            ].join(" ")}
            title={notebookFilename(entry.path)}
          >
            {notebookFilename(entry.path)}
          </h3>
          <div
            className="mt-1 flex min-w-0 items-center gap-1.5 text-sm text-gray-400"
            title={notebookParentPath(entry.path)}
          >
            <FolderOpen size={14} strokeWidth={1.5} />
            <span className="truncate">{notebookParentPath(entry.path)}</span>
          </div>
        </div>

        <button
          aria-label={entry.pinned ? "Unpin notebook" : "Pin notebook"}
          className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-950"
          onClick={(event) => {
            event.stopPropagation();
            onTogglePinned(entry);
          }}
          type="button"
        >
          <PinIcon
            fill={entry.pinned ? "currentColor" : "none"}
            size={18}
            strokeWidth={1.5}
          />
        </button>
      </div>

      <div className="mt-auto flex items-center justify-between gap-3 pt-6 text-sm text-gray-400">
        <span>{relativeLastOpened(entry.lastOpened)}</span>
        <KernelDot alive={entry.kernelAlive} />
      </div>
    </article>
  );
}

type ContextMenuItemProps = {
  children: ReactNode;
  disabled?: boolean;
  icon?: ReactNode;
  onClick?: () => void;
  title?: string;
  tone?: "default" | "danger";
};

function ContextMenuItem({
  children,
  disabled = false,
  icon,
  onClick,
  title,
  tone = "default",
}: ContextMenuItemProps) {
  return (
    <li>
      <button
        aria-disabled={disabled}
        className={[
          "flex w-full items-center gap-2 rounded px-3 py-2 text-left text-sm",
          disabled
            ? "cursor-not-allowed text-gray-300"
            : tone === "danger"
              ? "text-red-600 hover:bg-red-50"
              : "text-gray-700 hover:bg-gray-100",
        ].join(" ")}
        onClick={
          disabled
            ? undefined
            : () => {
                onClick?.();
              }
        }
        title={title}
        type="button"
      >
        {icon}
        <span>{children}</span>
      </button>
    </li>
  );
}

export default function HomePage() {
  const [, navigate] = useLocation();
  const [recents, setRecents] = useState<RecentNotebookEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState>(null);

  const refreshRecents = useCallback(async () => {
    try {
      const entries = await invoke<RecentNotebookEntry[]>(
        "list_recent_notebooks",
      );
      setRecents(entries);
      setError(null);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshRecents();

    const handleFocus = () => {
      void refreshRecents();
    };

    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [refreshRecents]);

  useEffect(() => {
    if (!contextMenu) return;

    const closeMenu = () => setContextMenu(null);
    const handleKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") closeMenu();
    };

    window.addEventListener("click", closeMenu);
    window.addEventListener("blur", closeMenu);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("click", closeMenu);
      window.removeEventListener("blur", closeMenu);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [contextMenu]);

  const sortedRecents = useMemo(() => sortRecentEntries(recents), [recents]);
  const currentNotebook = sortedRecents.find((entry) => entry.isCurrent);
  const scratchNotebooks = sortedRecents.filter((entry) => entry.isScratch);
  const regularNotebooks = sortedRecents.filter((entry) => !entry.isScratch);

  const openNotebook = useCallback(
    (entry: RecentNotebookEntry) => {
      navigate(notebookUrl(entry.path));
    },
    [navigate],
  );

  const handleOpenFile = useCallback(async () => {
    const file = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
    });
    if (typeof file === "string") navigate(notebookUrl(file));
  }, [navigate]);

  const handleNewNotebook = useCallback(async () => {
    try {
      const path = await invoke<string>("new_notebook_via_daemon");
      navigate(notebookUrl(path));
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, [navigate]);

  const handleReopenCurrent = useCallback(async () => {
    if (!currentNotebook) return;

    try {
      await invoke<string>("reopen_notebook_via_daemon");
    } catch (caught) {
      setError(errorMessage(caught));
      navigate(notebookUrl(currentNotebook.path));
    }
  }, [currentNotebook, navigate]);

  const handleCloseCurrent = useCallback(async () => {
    try {
      await invoke("close_notebook_via_daemon");
      await refreshRecents();
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, [refreshRecents]);

  const handleTogglePinned = useCallback(
    async (entry: RecentNotebookEntry) => {
      try {
        await invoke("set_notebook_pinned", {
          path: entry.path,
          pinned: !entry.pinned,
        });
        await refreshRecents();
      } catch (caught) {
        setError(errorMessage(caught));
      }
    },
    [refreshRecents],
  );

  const handleRevealInFinder = useCallback(
    async (entry: RecentNotebookEntry) => {
      try {
        await invoke("reveal_notebook_in_finder", { path: entry.path });
      } catch (caught) {
        setError(errorMessage(caught));
      }
    },
    [],
  );

  const handleRemoveFromRecents = useCallback(
    async (entry: RecentNotebookEntry) => {
      try {
        await invoke("remove_notebook_from_recents", { path: entry.path });
        await refreshRecents();
      } catch (caught) {
        setError(errorMessage(caught));
      }
    },
    [refreshRecents],
  );

  const handleMoveToTrash = useCallback(
    async (entry: RecentNotebookEntry) => {
      if (entry.isCurrent) return;

      try {
        await invoke("move_notebook_to_trash", { path: entry.path });
        await refreshRecents();
      } catch (caught) {
        setError(errorMessage(caught));
      }
    },
    [refreshRecents],
  );

  const handleDiscardScratch = useCallback(async () => {
    if (scratchNotebooks.length === 0) return;

    const confirmed = window.confirm(
      `Discard ${scratchNotebooks.length} scratch notebook${
        scratchNotebooks.length === 1 ? "" : "s"
      }?`,
    );
    if (!confirmed) return;

    try {
      await invoke<number>("discard_scratch_notebooks");
      await refreshRecents();
    } catch (caught) {
      setError(errorMessage(caught));
    }
  }, [refreshRecents, scratchNotebooks.length]);

  const handleCardContextMenu = useCallback(
    (event: MouseEvent<HTMLElement>, entry: RecentNotebookEntry) => {
      event.preventDefault();
      setContextMenu({ entry, x: event.clientX, y: event.clientY });
    },
    [],
  );

  return (
    <div className="h-screen overflow-y-auto bg-white">
      <Header />
      <main className="px-8 py-20">
        <h1 className="mb-2.5 text-4xl">Welcome to Jute</h1>

        <h2 className="text-lg text-gray-400">
          A native notebook for interactive computing.
        </h2>

        {error && (
          <div className="mt-6 rounded border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
            {error}
          </div>
        )}

        {currentNotebook && (
          <section className="mt-8 rounded border border-gray-300 p-4">
            <div className="flex flex-wrap items-center justify-between gap-4">
              <div className="min-w-0">
                <div className="mb-2 flex items-center gap-2 text-sm text-gray-400">
                  <KernelDot alive={currentNotebook.kernelAlive} />
                  <span>Current notebook</span>
                </div>
                <h2
                  className="truncate text-2xl text-gray-950"
                  title={notebookFilename(currentNotebook.path)}
                >
                  {notebookFilename(currentNotebook.path)}
                </h2>
                <p
                  className="mt-1 truncate text-sm text-gray-400"
                  title={currentNotebook.path}
                >
                  {currentNotebook.path}
                </p>
              </div>

              <div className="flex items-center gap-2">
                <button
                  className="rounded border border-gray-300 px-3 py-2 text-sm transition-colors hover:border-black"
                  onClick={handleReopenCurrent}
                  type="button"
                >
                  Reopen
                </button>
                <button
                  className="rounded border border-gray-300 px-3 py-2 text-sm text-gray-500 transition-colors hover:border-black hover:text-gray-950"
                  onClick={handleCloseCurrent}
                  type="button"
                >
                  Close
                </button>
              </div>
            </div>
          </section>
        )}

        <div className="my-8 flex flex-wrap gap-4">
          <button
            className="flex h-28 min-w-56 flex-col justify-between rounded border border-gray-300 p-4 text-left transition-colors hover:border-black"
            onClick={handleNewNotebook}
            type="button"
          >
            <Plus size={24} strokeWidth={1.5} />
            <span className="text-xl">New notebook</span>
          </button>

          <button
            className="flex h-28 min-w-48 flex-col justify-between rounded border border-gray-300 p-4 text-left text-gray-600 transition-colors hover:border-black hover:text-gray-950"
            onClick={handleOpenFile}
            type="button"
          >
            <ArrowRight size={22} strokeWidth={1.5} />
            <span className="text-xl">Open...</span>
          </button>
        </div>

        {loading ? (
          <p className="text-sm text-gray-400">Loading recent notebooks...</p>
        ) : recents.length === 0 ? (
          <section className="flex max-w-xl items-center gap-4 rounded border border-dashed border-gray-300 p-5 text-gray-500">
            <ArrowUp className="shrink-0" size={24} strokeWidth={1.5} />
            <p>
              No recent notebooks yet. Start with a new notebook or open an
              existing .ipynb file.
            </p>
          </section>
        ) : (
          regularNotebooks.length > 0 && (
            <section>
              <h2 className="mb-3 text-sm uppercase tracking-wide text-gray-400">
                Recent notebooks
              </h2>
              <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
                {regularNotebooks.map((entry) => (
                  <NotebookCard
                    entry={entry}
                    key={entry.path}
                    onContextMenu={handleCardContextMenu}
                    onOpen={openNotebook}
                    onTogglePinned={handleTogglePinned}
                  />
                ))}
              </div>
            </section>
          )
        )}

        <details className="mt-10 border-t border-gray-200 pt-5">
          <summary className="flex cursor-pointer list-none items-center justify-between gap-4">
            <span className="text-sm uppercase tracking-wide text-gray-400">
              Scratch notebooks ({scratchNotebooks.length})
            </span>
            <button
              className="rounded border border-gray-300 px-3 py-1.5 text-sm text-gray-500 transition-colors hover:border-black hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
              disabled={scratchNotebooks.length === 0}
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                void handleDiscardScratch();
              }}
              type="button"
            >
              Discard all scratch
            </button>
          </summary>

          {scratchNotebooks.length === 0 ? (
            <p className="mt-4 text-sm text-gray-400">
              Scratch notebooks created by the daemon will appear here.
            </p>
          ) : (
            <div className="mt-4 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
              {scratchNotebooks.map((entry) => (
                <NotebookCard
                  compact
                  entry={entry}
                  key={entry.path}
                  onContextMenu={handleCardContextMenu}
                  onOpen={openNotebook}
                  onTogglePinned={handleTogglePinned}
                />
              ))}
            </div>
          )}
        </details>

        {contextMenu && (
          <ul
            className="fixed z-30 min-w-56 rounded border border-gray-200 bg-white p-1 shadow-lg"
            onContextMenu={(event) => event.preventDefault()}
            style={{ left: contextMenu.x, top: contextMenu.y }}
          >
            <ContextMenuItem
              icon={<ArrowRight size={16} strokeWidth={1.5} />}
              onClick={() => openNotebook(contextMenu.entry)}
            >
              Open
            </ContextMenuItem>
            <ContextMenuItem
              icon={<FolderOpen size={16} strokeWidth={1.5} />}
              onClick={() => void handleRevealInFinder(contextMenu.entry)}
            >
              Reveal in Finder
            </ContextMenuItem>
            <ContextMenuItem
              icon={
                contextMenu.entry.pinned ? (
                  <PinOff size={16} strokeWidth={1.5} />
                ) : (
                  <Pin size={16} strokeWidth={1.5} />
                )
              }
              onClick={() => void handleTogglePinned(contextMenu.entry)}
            >
              {contextMenu.entry.pinned ? "Unpin" : "Pin"}
            </ContextMenuItem>
            <ContextMenuItem
              onClick={() => void handleRemoveFromRecents(contextMenu.entry)}
            >
              Remove from Recents
            </ContextMenuItem>
            <ContextMenuItem
              disabled={contextMenu.entry.isCurrent}
              icon={<Trash2 size={16} strokeWidth={1.5} />}
              onClick={() => void handleMoveToTrash(contextMenu.entry)}
              title={
                contextMenu.entry.isCurrent
                  ? "Close the current notebook before moving it to Trash."
                  : undefined
              }
              tone="danger"
            >
              Move to Trash
            </ContextMenuItem>
          </ul>
        )}
      </main>
    </div>
  );
}
