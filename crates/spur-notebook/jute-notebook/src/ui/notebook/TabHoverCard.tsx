import { useCallback, useEffect, useRef, useState } from "react";

import type { KernelSlotInfo, NotebookTab } from "@/stores/notebook";

export type HoverAnchor = { left: number; bottom: number };

type Props = {
  anchor: HoverAnchor;
  stats: KernelSlotInfo | null;
  tab: NotebookTab;
};

export default function TabHoverCard({ anchor, stats, tab }: Props) {
  const kernelLine =
    tab.kernelState === "idle"
      ? "none"
      : `${tab.kernelState} · gen ${tab.kernelGeneration ?? 0}`;
  const resourceLine = stats
    ? `${Math.round(stats.cpu_pct)}% CPU · ${Math.round(stats.mem_mb)} MB`
    : "·";

  return (
    <div
      className="fixed z-50 w-64 rounded-lg border border-gray-200 bg-white p-3 shadow-lg"
      role="tooltip"
      style={{ left: anchor.left, top: anchor.bottom + 8 }}
    >
      <div className="text-xs font-semibold text-gray-900">{tab.title}</div>
      {tab.path && (
        <div className="break-all font-mono text-[10px] text-gray-400">
          {tab.path}
        </div>
      )}
      <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 border-t border-gray-200 pt-2 text-[11px]">
        <dt className="text-gray-400">Kernel</dt>
        <dd className="font-mono text-gray-900">{kernelLine}</dd>
        <dt className="text-gray-400">Resources</dt>
        <dd className="font-mono text-gray-900">{resourceLine}</dd>
        <dt className="text-gray-400">Mode</dt>
        <dd className="font-mono text-gray-900">{tab.mode}</dd>
        <dt className="text-gray-400">Unsaved</dt>
        <dd className="font-mono text-gray-900">{tab.dirty ? "yes" : "no"}</dd>
      </dl>
    </div>
  );
}

export function useTabHoverDelay(delayMs: number) {
  const [hoveredTabId, setHoveredTabId] = useState<string | undefined>();
  const [anchor, setAnchor] = useState<HoverAnchor>({ left: 0, bottom: 0 });
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancel = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    setHoveredTabId(undefined);
  }, []);

  const onTabEnter = useCallback(
    (tabId: string, rect: HoverAnchor) => {
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        setAnchor(rect);
        setHoveredTabId(tabId);
        timer.current = null;
      }, delayMs);
    },
    [delayMs],
  );

  useEffect(() => cancel, [cancel]);

  return { anchor, cancel, hoveredTabId, onTabEnter, onTabLeave: cancel };
}
