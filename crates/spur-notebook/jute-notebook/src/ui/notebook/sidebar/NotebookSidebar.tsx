import clsx from "clsx";
import { ChevronLeftIcon, ChevronRightIcon } from "lucide-react";
import { useRef } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

import {
  MAX_SIDEBAR_WIDTH,
  MIN_SIDEBAR_WIDTH,
  useSidebar,
} from "@/stores/sidebar";

import { SIDEBAR_PANELS } from "./panels";

export default function NotebookSidebar() {
  const activePanelId = useSidebar((state) => state.activePanelId);
  const collapsed = useSidebar((state) => state.collapsed);
  const sidebarWidth = useSidebar((state) => state.width);
  const activatePanel = useSidebar((state) => state.activatePanel);
  const toggleCollapsed = useSidebar((state) => state.toggleCollapsed);
  const setWidth = useSidebar((state) => state.setWidth);

  const activePanel =
    SIDEBAR_PANELS.find((panel) => panel.id === activePanelId) ??
    SIDEBAR_PANELS[0];

  // Keep-alive: a panel mounts on first activation and stays mounted.
  // Accumulate synchronously during render (not in an effect) so a newly
  // activated panel's body is present on the SAME frame its header updates —
  // otherwise there is a one-frame empty-body flash on panel switch.
  const mountedIdsRef = useRef<Set<string>>(new Set([activePanel.id]));
  mountedIdsRef.current.add(activePanel.id);
  const mountedIds = mountedIdsRef.current;

  const ActiveIcon = activePanel.icon;
  const resizeBy = (delta: number) => setWidth(sidebarWidth + delta);

  const startResize = (event: PointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);

    const onPointerMove = (moveEvent: globalThis.PointerEvent) => {
      setWidth(window.innerWidth - moveEvent.clientX - 48);
    };
    const onPointerUp = () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
    };

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  };

  const resizeWithKeyboard = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      resizeBy(24);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      resizeBy(-24);
    }
  };

  return (
    <aside className="relative flex h-full shrink-0 border-l border-gray-200 bg-gray-50 text-gray-700">
      {!collapsed && (
        <div
          aria-label="Resize sidebar"
          aria-orientation="vertical"
          aria-valuemax={MAX_SIDEBAR_WIDTH}
          aria-valuemin={MIN_SIDEBAR_WIDTH}
          aria-valuenow={sidebarWidth}
          className="absolute left-0 top-0 z-10 h-full w-2 -translate-x-1 cursor-col-resize outline-none transition-colors hover:bg-gray-900/10 focus:bg-gray-900/10"
          onKeyDown={resizeWithKeyboard}
          onPointerDown={startResize}
          role="separator"
          tabIndex={0}
        />
      )}
      <div
        className={clsx(
          "flex h-full min-h-0 flex-col overflow-hidden transition-[width] duration-200",
          collapsed && "w-0",
        )}
        style={collapsed ? undefined : { width: sidebarWidth }}
      >
        {!collapsed && (
          <div className="flex items-center gap-2 px-3 pb-2 pt-14">
            <ActiveIcon className="shrink-0 text-gray-500" size={18} />
            <h2 className="truncate text-sm font-medium text-gray-950">
              {activePanel.title}
            </h2>
          </div>
        )}
        {SIDEBAR_PANELS.map((panel) => {
          if (!mountedIds.has(panel.id)) return null;
          const PanelComponent = panel.Component;
          return (
            <div
              className="min-h-0 flex-1"
              hidden={collapsed || panel.id !== activePanel.id}
              key={panel.id}
            >
              <PanelComponent />
            </div>
          );
        })}
      </div>

      <div className="flex w-12 shrink-0 flex-col items-center gap-1 border-l border-gray-200 bg-white pt-14">
        <button
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          className="rounded p-1.5 text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-950"
          onClick={toggleCollapsed}
          type="button"
        >
          {collapsed ? (
            <ChevronLeftIcon size={18} strokeWidth={1.5} />
          ) : (
            <ChevronRightIcon size={18} strokeWidth={1.5} />
          )}
        </button>
        <div className="my-1 h-px w-5 bg-gray-200" />
        {SIDEBAR_PANELS.map((panel) => {
          const Icon = panel.icon;
          const isActive = !collapsed && panel.id === activePanel.id;
          return (
            <button
              aria-label={panel.ariaLabel}
              aria-pressed={isActive}
              className={clsx(
                "rounded p-1.5 transition-colors",
                isActive
                  ? "bg-gray-900 text-white"
                  : "text-gray-500 hover:bg-gray-100 hover:text-gray-950",
              )}
              key={panel.id}
              onClick={() => activatePanel(panel.id)}
              type="button"
            >
              <Icon size={18} strokeWidth={1.5} />
            </button>
          );
        })}
      </div>
    </aside>
  );
}
