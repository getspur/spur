import clsx from "clsx";
import { ChevronDownIcon, PlusIcon, XIcon } from "lucide-react";
import { useState } from "react";

import type { NotebookTab } from "@/stores/notebook";

type Props = {
  activeTabId?: string;
  tabs: NotebookTab[];
  onCloseTab: (tabId: string) => void;
  onNewTab: () => void | Promise<void>;
  onOpenNotebook: () => void | Promise<void>;
  onSwitchTab: (tabId: string) => void;
};

export default function NotebookTabStrip({
  activeTabId,
  onCloseTab,
  onNewTab,
  onOpenNotebook,
  onSwitchTab,
  tabs,
}: Props) {
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <div className="flex h-9 items-end border-b border-gray-200 bg-gray-50 pl-16 pr-2 text-gray-900">
      <div
        aria-label="Notebook tabs"
        className="flex min-w-0 flex-1 items-end overflow-hidden"
        role="tablist"
      >
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          return (
            <div
              className={clsx(
                "group flex h-8 min-w-[116px] max-w-[188px] items-center rounded-t border border-b-0 px-2 text-xs",
                active
                  ? "relative z-10 border-gray-200 bg-white"
                  : "border-transparent bg-gray-50 text-gray-500 hover:bg-gray-100 hover:text-gray-900",
              )}
              key={tab.id}
            >
              <button
                aria-selected={active}
                className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                onClick={() => onSwitchTab(tab.id)}
                role="tab"
                type="button"
              >
                <span
                  aria-label={kernelStateLabel(tab.kernelState)}
                  className={clsx(
                    "h-2 w-2 shrink-0 rounded-full",
                    tab.kernelState === "idle"
                      ? "bg-orange-500"
                      : "bg-green-500",
                    tab.kernelState === "running" && "animate-pulse",
                  )}
                />
                <span className="rounded bg-gray-100 px-1 py-px font-mono text-[10px] uppercase text-gray-500">
                  {languageBadge(tab.language)}
                </span>
                <span className="min-w-0 truncate">{tab.title}</span>
                {active && (
                  <span
                    aria-hidden="true"
                    className="ml-0.5 shrink-0 text-[13px] leading-none text-violet-500"
                  >
                    ◎
                  </span>
                )}
              </button>
              <button
                aria-label={`Close ${tab.title}`}
                className="ml-1 flex h-5 w-5 shrink-0 items-center justify-center rounded text-gray-500 transition-colors hover:bg-gray-200 hover:text-gray-900"
                onClick={(event) => {
                  event.stopPropagation();
                  onCloseTab(tab.id);
                }}
                type="button"
              >
                {tab.dirty ? (
                  <>
                    <span
                      aria-hidden="true"
                      className="group-hover:hidden h-1.5 w-1.5 rounded-full bg-violet-500"
                    />
                    <XIcon
                      aria-hidden="true"
                      className="group-hover:block hidden"
                      size={13}
                    />
                  </>
                ) : (
                  <XIcon aria-hidden="true" size={13} />
                )}
              </button>
            </div>
          );
        })}
      </div>
      <button
        aria-label="New tab"
        className="mb-1 ml-1 flex h-7 w-7 shrink-0 items-center justify-center rounded text-gray-500 transition-all hover:bg-gray-100 hover:text-gray-900 active:scale-110"
        onClick={() => void onNewTab()}
        title="New tab"
        type="button"
      >
        <PlusIcon size={16} />
      </button>
      <div className="relative mb-1">
        <button
          aria-expanded={menuOpen}
          aria-label="Tab overflow"
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-900"
          onClick={() => setMenuOpen((open) => !open)}
          title="Tab overflow"
          type="button"
        >
          <ChevronDownIcon size={16} />
        </button>
        {menuOpen && (
          <div
            className="absolute right-0 top-full z-20 mt-1 w-44 rounded border border-gray-200 bg-white p-1 text-sm text-gray-700 shadow-lg"
            role="menu"
          >
            <button
              className="w-full rounded px-3 py-2 text-left hover:bg-gray-100 hover:text-gray-950"
              onClick={() => {
                setMenuOpen(false);
                void onNewTab();
              }}
              role="menuitem"
              type="button"
            >
              New notebook
            </button>
            <button
              className="w-full rounded px-3 py-2 text-left hover:bg-gray-100 hover:text-gray-950"
              onClick={() => {
                setMenuOpen(false);
                void onOpenNotebook();
              }}
              role="menuitem"
              type="button"
            >
              Open notebook...
            </button>
          </div>
        )}
      </div>
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

function languageBadge(language: string | undefined): string {
  if (!language) return "PY";
  if (language === "python3") return "PY";
  if (language === "evcxr") return "RS";
  return language.slice(0, 2);
}
