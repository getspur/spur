import clsx from "clsx";
import { ChevronDownIcon, PlusIcon, XIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { closeOthersTargets, closeRightTargets } from "@/pages/tabActions";
import type { KernelSlotInfo, NotebookTab } from "@/stores/notebook";
import TabContextMenu from "@/ui/notebook/TabContextMenu";
import TabHoverCard, { useTabHoverDelay } from "@/ui/notebook/TabHoverCard";
import TabListMenu from "@/ui/notebook/TabListMenu";

type Props = {
  activeTabId?: string;
  canReopen: boolean;
  getKernelStats: (tabId: string) => Promise<KernelSlotInfo | null>;
  tabs: NotebookTab[];
  onCloseTab: (tabId: string) => void;
  onCloseOthers: (tabId: string) => void;
  onCloseRight: (tabId: string) => void;
  onNewTab: () => void | Promise<void>;
  onOpenNotebook: () => void | Promise<void>;
  onReopenClosed: () => void | Promise<void>;
  onReorder: (tabId: string, toIndex: number) => void;
  onSwitchTab: (tabId: string) => void;
  onTogglePin: (tabId: string) => void;
};

export default function NotebookTabStrip({
  activeTabId,
  canReopen,
  getKernelStats,
  onCloseTab,
  onCloseOthers,
  onCloseRight,
  onNewTab,
  onOpenNotebook,
  onReopenClosed,
  onReorder,
  onSwitchTab,
  onTogglePin,
  tabs,
}: Props) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [contextTarget, setContextTarget] = useState<{
    tabId: string;
    x: number;
    y: number;
  } | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<{
    id: string;
    before: boolean;
  } | null>(null);
  const [stats, setStats] = useState<KernelSlotInfo | null>(null);
  const [lockedWidths, setLockedWidths] = useState<Record<
    string,
    number
  > | null>(null);
  const [rowWidth, setRowWidth] = useState<number | null>(null);
  const rowRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef(new Map<string, HTMLDivElement>());
  const { anchor, cancel, hoveredTabId, onTabEnter, onTabLeave } =
    useTabHoverDelay(350);

  useEffect(() => {
    if (typeof ResizeObserver === "undefined" || !rowRef.current) return;
    const observer = new ResizeObserver((entries) => {
      setRowWidth(entries[0]?.contentRect.width ?? null);
    });
    observer.observe(rowRef.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (dragId || menuOpen || contextTarget) cancel();
  }, [cancel, contextTarget, dragId, menuOpen]);

  const pinnedCount = tabs.filter((tab) => tab.pinned).length;
  const unpinnedCount = tabs.length - pinnedCount;
  const estimatedTabWidth =
    rowWidth === null || unpinnedCount === 0
      ? null
      : (rowWidth - 42 * pinnedCount - 30) / unpinnedCount;
  const hideCloseSlot = estimatedTabWidth !== null && estimatedTabWidth < 96;
  const hideBadge = estimatedTabWidth !== null && estimatedTabWidth < 66;

  const captureWidths = () => {
    const widths: Record<string, number> = {};
    tabRefs.current.forEach((el, id) => {
      widths[id] = el.getBoundingClientRect().width;
    });
    setLockedWidths(widths);
  };

  const handleClose = (tabId: string) => {
    captureWidths();
    onCloseTab(tabId);
  };

  const hoveredTab = tabs.find((tab) => tab.id === hoveredTabId);
  const contextTab = tabs.find((tab) => tab.id === contextTarget?.tabId);

  const handleMouseEnter = (tab: NotebookTab, element: HTMLDivElement) => {
    if (dragId || menuOpen || contextTarget) return;
    const rect = element.getBoundingClientRect();
    onTabEnter(tab.id, { left: rect.left, bottom: rect.bottom });
    setStats(null);
    if (tab.kernelState === "idle") return;
    void getKernelStats(tab.id).then((nextStats) => {
      setStats(nextStats);
    });
  };

  return (
    <div
      className="flex h-9 items-end border-b border-gray-200 bg-gray-50 pl-16 pr-2 text-gray-900"
      data-testid="tab-strip"
      data-width-lock={lockedWidths !== null ? "true" : undefined}
      onMouseLeave={() => setLockedWidths(null)}
    >
      <div
        aria-label="Notebook tabs"
        className="flex min-w-0 flex-1 items-end overflow-x-auto"
        onDoubleClick={(event) => {
          if (event.target === event.currentTarget) {
            void onNewTab();
          }
        }}
        ref={rowRef}
        role="tablist"
      >
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          const dropTargetBefore =
            dropTarget?.id === tab.id && dropTarget.before;
          const dropTargetAfter =
            dropTarget?.id === tab.id && !dropTarget.before;
          return (
            <div
              aria-label={tab.pinned ? `${tab.title} (pinned)` : undefined}
              className={clsx(
                "group relative flex h-8 items-center rounded-t border border-b-0 text-xs",
                tab.pinned
                  ? "w-[42px] flex-none justify-center px-0"
                  : "min-w-[56px] max-w-[200px] flex-1 basis-[200px] px-2",
                tab.attention && !active && "bg-green-50",
                active
                  ? "z-10 border-gray-200 bg-white"
                  : !tab.attention &&
                      "border-transparent bg-gray-50 text-gray-500 hover:bg-gray-100 hover:text-gray-900",
                dragId === tab.id && "opacity-50",
                dropTargetBefore && "shadow-[-2px_0_0_0_#111827]",
                dropTargetAfter && "shadow-[2px_0_0_0_#111827]",
              )}
              draggable={!tab.pinned}
              key={tab.id}
              onAuxClick={(event) => {
                if (event.button === 1 && !tab.pinned) {
                  handleClose(tab.id);
                }
              }}
              onDragEnd={() => {
                setDragId(null);
                setDropTarget(null);
              }}
              onDragOver={(event) => {
                if (!dragId || dragId === tab.id || tab.pinned) return;
                event.preventDefault();
                const rect = event.currentTarget.getBoundingClientRect();
                setDropTarget({
                  id: tab.id,
                  before: event.clientX < rect.left + rect.width / 2,
                });
              }}
              onDragStart={(event) => {
                if (tab.pinned) {
                  event.preventDefault();
                  return;
                }
                setDragId(tab.id);
                if (event.dataTransfer) {
                  event.dataTransfer.effectAllowed = "move";
                }
              }}
              onDrop={(event) => {
                event.preventDefault();
                if (!dragId || !dropTarget) return;
                const fromIndex = tabs.findIndex(
                  (candidate) => candidate.id === dragId,
                );
                const targetIndex = tabs.findIndex(
                  (candidate) => candidate.id === dropTarget.id,
                );
                if (fromIndex < 0 || targetIndex < 0) return;

                const insertionIndex =
                  targetIndex + (dropTarget.before ? 0 : 1);
                onReorder(
                  dragId,
                  fromIndex < insertionIndex
                    ? insertionIndex - 1
                    : insertionIndex,
                );
                setDragId(null);
                setDropTarget(null);
              }}
              onContextMenu={(event) => {
                event.preventDefault();
                cancel();
                setMenuOpen(false);
                setContextTarget({
                  tabId: tab.id,
                  x: event.clientX,
                  y: event.clientY,
                });
              }}
              onMouseEnter={(event) => {
                handleMouseEnter(tab, event.currentTarget);
              }}
              onMouseLeave={onTabLeave}
              ref={(el) => {
                if (el) tabRefs.current.set(tab.id, el);
                else tabRefs.current.delete(tab.id);
              }}
              style={
                lockedWidths?.[tab.id] !== undefined
                  ? { width: lockedWidths[tab.id], flex: "0 0 auto" }
                  : undefined
              }
            >
              <button
                aria-selected={active}
                className={clsx(
                  "flex min-w-0 items-center gap-1.5 text-left",
                  tab.pinned ? "justify-center" : "flex-1",
                )}
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
                {!hideBadge && (
                  <span className="rounded bg-gray-100 px-1 py-px font-mono text-[10px] uppercase text-gray-500">
                    {languageBadge(tab.language)}
                  </span>
                )}
                {!tab.pinned && (
                  <span className="min-w-0 truncate">{tab.title}</span>
                )}
                {!tab.pinned && tab.attention && (
                  <span
                    aria-hidden="true"
                    className="ml-1 shrink-0 text-[10px] text-green-600"
                  >
                    ✓
                  </span>
                )}
                {active && !tab.pinned && (
                  <span
                    aria-hidden="true"
                    className="ml-0.5 shrink-0 text-[13px] leading-none text-violet-500"
                  >
                    ◎
                  </span>
                )}
              </button>
              {!tab.pinned && !hideCloseSlot && (
                <button
                  aria-label={`Close ${tab.title}`}
                  className="ml-1 flex h-5 w-5 shrink-0 items-center justify-center rounded text-gray-500 transition-colors hover:bg-gray-200 hover:text-gray-900"
                  onClick={(event) => {
                    event.stopPropagation();
                    handleClose(tab.id);
                  }}
                  type="button"
                >
                  {tab.dirty ? (
                    <>
                      <span
                        aria-hidden="true"
                        className="h-1.5 w-1.5 rounded-full bg-violet-500 group-hover:hidden"
                      />
                      <XIcon
                        aria-hidden="true"
                        className="hidden group-hover:block"
                        size={13}
                      />
                    </>
                  ) : (
                    <XIcon aria-hidden="true" size={13} />
                  )}
                </button>
              )}
            </div>
          );
        })}
        <button
          aria-label="New tab"
          className="mb-1 ml-1 flex h-7 w-7 shrink-0 items-center justify-center rounded text-gray-500 transition-all hover:bg-gray-100 hover:text-gray-900 active:scale-110"
          onClick={() => void onNewTab()}
          title="New tab"
          type="button"
        >
          <PlusIcon size={16} />
        </button>
      </div>
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
          <TabListMenu
            activeTabId={activeTabId}
            onDismiss={() => setMenuOpen(false)}
            onNewTab={onNewTab}
            onOpenNotebook={onOpenNotebook}
            onSelect={onSwitchTab}
            tabs={tabs}
          />
        )}
      </div>
      {hoveredTabId && hoveredTab && !contextTarget && !menuOpen && (
        <TabHoverCard anchor={anchor} stats={stats} tab={hoveredTab} />
      )}
      {contextTarget && contextTab && (
        <TabContextMenu
          canReopen={canReopen}
          closeOthersCount={closeOthersTargets(tabs, contextTab.id).length}
          closeRightCount={closeRightTargets(tabs, contextTab.id).length}
          onClose={() => handleClose(contextTab.id)}
          onCloseOthers={() => onCloseOthers(contextTab.id)}
          onCloseRight={() => onCloseRight(contextTab.id)}
          onCopyPath={() => {
            if (contextTab.path) {
              void navigator.clipboard
                .writeText(contextTab.path)
                .catch(() => {});
            }
          }}
          onDismiss={() => setContextTarget(null)}
          onReopenClosed={onReopenClosed}
          onTogglePin={() => onTogglePin(contextTab.id)}
          position={{ x: contextTarget.x, y: contextTarget.y }}
          tab={contextTab}
        />
      )}
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
