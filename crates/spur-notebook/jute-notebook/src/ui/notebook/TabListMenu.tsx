import clsx from "clsx";
import { useEffect, useRef, useState } from "react";

import type { NotebookTab } from "@/stores/notebook";

type Props = {
  activeTabId?: string;
  onDismiss: () => void;
  onNewTab: () => void | Promise<void>;
  onOpenNotebook: () => void | Promise<void>;
  onSelect: (tabId: string) => void;
  tabs: NotebookTab[];
};

export default function TabListMenu({
  activeTabId,
  onDismiss,
  onNewTab,
  onOpenNotebook,
  onSelect,
  tabs,
}: Props) {
  const [query, setQuery] = useState("");
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onMouseDown = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as Node)) onDismiss();
    };
    document.addEventListener("mousedown", onMouseDown);
    return () => document.removeEventListener("mousedown", onMouseDown);
  }, [onDismiss]);

  const visible = tabs.filter((tab) =>
    `${tab.title} ${tab.path ?? ""}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );

  return (
    <div
      className="absolute right-0 top-full z-20 mt-1 w-80 rounded-lg border border-gray-200 bg-white p-2 shadow-lg"
      ref={ref}
      role="menu"
    >
      <input
        autoFocus
        className="mb-1.5 w-full rounded border border-gray-200 px-2 py-1.5 text-xs text-gray-900 outline-none focus:border-gray-400"
        onChange={(event) => setQuery(event.target.value)}
        placeholder="Search tabs by name or path"
        value={query}
      />
      {visible.map((tab) => (
        <button
          className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left hover:bg-gray-100"
          key={tab.id}
          onClick={() => {
            onSelect(tab.id);
            onDismiss();
          }}
          role="menuitem"
          type="button"
        >
          <span
            aria-label={kernelStateLabel(tab.kernelState)}
            className={clsx(
              "h-2 w-2 shrink-0 rounded-full",
              tab.kernelState === "idle" ? "bg-orange-500" : "bg-green-500",
              tab.kernelState === "running" && "animate-pulse",
            )}
          />
          <span className="min-w-0 flex-1">
            <span className="flex items-center gap-1.5 truncate text-xs text-gray-900">
              {tab.title}
              {tab.dirty && (
                <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-violet-500" />
              )}
            </span>
            <span className="block truncate font-mono text-[9px] text-gray-400">
              {tab.path ?? "scratch"}
            </span>
          </span>
          {tab.id === activeTabId && (
            <span className="shrink-0 font-mono text-[9px] text-violet-500">
              current
            </span>
          )}
        </button>
      ))}
      <div className="mx-1 my-1 h-px bg-gray-200" />
      <button
        className="w-full rounded px-2 py-1.5 text-left text-xs text-gray-700 hover:bg-gray-100"
        onClick={() => {
          void onNewTab();
          onDismiss();
        }}
        role="menuitem"
        type="button"
      >
        New notebook
      </button>
      <button
        className="w-full rounded px-2 py-1.5 text-left text-xs text-gray-700 hover:bg-gray-100"
        onClick={() => {
          void onOpenNotebook();
          onDismiss();
        }}
        role="menuitem"
        type="button"
      >
        Open notebook...
      </button>
    </div>
  );
}

function kernelStateLabel(state: NotebookTab["kernelState"]): string {
  switch (state) {
    case "idle":
      return "No kernel";
    case "live":
      return "Kernel live";
    case "running":
      return "Kernel running";
  }
}
