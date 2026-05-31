import clsx from "clsx";
import {
  ChartLineIcon,
  HomeIcon,
  PlayIcon,
  PlusIcon,
  RefreshCwIcon,
  SettingsIcon,
} from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "wouter";
import { useStore } from "zustand";

import {
  type KernelSlotInfo,
  type NotebookViewMode,
  useNotebook,
} from "@/stores/notebook";

import SettingsPanel from "../settings/SettingsPanel";
import Header from "../shared/Header";

const KERNEL_STATS_POLL_MS = 2000;

type Props = {
  kernelName: string;
};

export default function NotebookHeader({ kernelName }: Props) {
  const notebook = useNotebook();

  const kernelId = useStore(
    notebook.store,
    (state) => state.viewState.kernelId,
  );
  const selectedCellId = useStore(
    notebook.store,
    (state) => state.viewState.selectedCellId,
  );
  const kernelGeneration = useStore(
    notebook.store,
    (state) => state.viewState.kernelGeneration,
  );
  const viewMode = useStore(
    notebook.store,
    (state) => state.viewState.viewMode,
  );
  const setViewMode = useStore(
    notebook.store,
    (state) => state.viewStateActions.setViewMode,
  );
  const [statsOpen, setStatsOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [kernelStats, setKernelStats] = useState<KernelSlotInfo | null>(null);
  const [statsError, setStatsError] = useState<string | null>(null);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        !event.metaKey ||
        !event.shiftKey ||
        event.key.toLowerCase() !== "g"
      ) {
        return;
      }
      event.preventDefault();
      setViewMode(viewMode === "dag" ? "cells" : "dag");
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [setViewMode, viewMode]);

  useEffect(() => {
    if (!statsOpen) return;

    let cancelled = false;

    const updateKernelStats = async () => {
      if (!kernelId) {
        if (!cancelled) {
          setKernelStats(null);
          setStatsError("Kernel not running");
        }
        return;
      }

      try {
        setStatsError(null);
        const info = await notebook.refreshKernelSlotInfo();
        if (!cancelled) {
          setKernelStats(info);
        }
      } catch {
        if (!cancelled) {
          setKernelStats(null);
          setStatsError("Kernel not running");
        }
      }
    };

    void updateKernelStats();
    const timer = window.setInterval(() => {
      void updateKernelStats();
    }, KERNEL_STATS_POLL_MS);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [kernelId, notebook, statsOpen]);

  return (
    <Header>
      {/* Empty placeholder to take up space where the traffic light buttons are. */}
      <div className="w-16" />

      {/* Centered UI components: kernel controls and stats. */}
      <div className="flex items-center">
        <button
          className="rounded p-1 text-gray-500 transition-all hover:bg-gray-100 hover:text-black active:scale-110 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-gray-500 disabled:active:scale-100"
          disabled={selectedCellId === null}
          onClick={() => {
            if (selectedCellId) {
              void notebook.execute(selectedCellId);
            }
          }}
        >
          <PlayIcon size={16} />
        </button>
        <button
          className="rounded p-1 text-gray-500 transition-all hover:bg-gray-100 hover:text-black active:scale-110"
          onClick={() => void notebook.restartKernel()}
        >
          <RefreshCwIcon size={16} />
        </button>

        <div className="mx-2 flex w-60 min-w-0 items-center justify-center rounded border border-gray-200 px-2 py-[3px] text-xs text-gray-900">
          <div
            className={clsx(
              "mr-2 h-2 w-2 shrink-0 rounded-full",
              kernelId ? "bg-green-500" : "bg-orange-500",
            )}
          />
          <span className="truncate">{kernelName}</span>
          {kernelGeneration !== undefined && (
            <span className="ml-2 shrink-0 rounded bg-gray-100 px-1 py-px text-[10px] text-gray-500">
              gen {kernelGeneration}
            </span>
          )}
        </div>

        <div className="relative">
          <button
            className="rounded p-1 text-gray-500 transition-all hover:bg-gray-100 hover:text-black active:scale-110"
            onClick={() => setStatsOpen((open) => !open)}
          >
            <ChartLineIcon size={16} />
          </button>

          {statsOpen && (
            <div className="absolute left-1/2 top-full z-20 mt-2 w-44 -translate-x-1/2 rounded border border-gray-200 bg-white px-3 py-2 text-xs text-gray-600 shadow-lg">
              {statsError ? (
                <span>{statsError}</span>
              ) : kernelStats ? (
                <div className="space-y-1">
                  <div className="flex items-center justify-between gap-3">
                    <span>CPU</span>
                    <span className="font-medium text-gray-900">
                      {kernelStats.cpu_pct.toFixed(1)}%
                    </span>
                  </div>
                  <div className="flex items-center justify-between gap-3">
                    <span>RAM</span>
                    <span className="font-medium text-gray-900">
                      {Math.round(kernelStats.mem_mb)} MB
                    </span>
                  </div>
                </div>
              ) : (
                <span>Loading...</span>
              )}
            </div>
          )}
        </div>

        <div
          className="ml-2 flex rounded border border-gray-200 bg-white p-0.5 text-xs"
          role="group"
          aria-label="Notebook view mode"
        >
          {(["cells", "dag"] as const).map((mode) => (
            <button
              key={mode}
              className={clsx(
                "rounded px-2 py-0.5 transition-colors",
                viewMode === mode
                  ? "bg-gray-900 text-white"
                  : "text-gray-500 hover:bg-gray-100 hover:text-gray-900",
              )}
              type="button"
              aria-pressed={viewMode === mode}
              title={mode === "cells" ? "Notebook cells" : "DAG view"}
              onClick={() => setViewMode(mode)}
            >
              {viewModeLabel(mode)}
            </button>
          ))}
        </div>
      </div>

      {/* Top-right UI components: home and open notebooks. */}
      <div className="flex items-center">
        <div className="relative">
          <button
            className="rounded p-1 text-gray-500 transition-all hover:bg-gray-100 hover:text-black active:scale-110"
            title="Settings"
            onClick={() => setSettingsOpen((open) => !open)}
          >
            <SettingsIcon size={20} strokeWidth={1.5} />
          </button>
          {settingsOpen && <SettingsPanel />}
        </div>
        <Link to="/">
          <button className="rounded p-1 text-gray-500 transition-all hover:bg-gray-100 hover:text-black active:scale-110">
            <HomeIcon size={20} strokeWidth={1.5} />
          </button>
        </Link>
        <Link to="/">
          <button className="rounded p-1 text-gray-500 transition-all hover:bg-gray-100 hover:text-black active:scale-110">
            <PlusIcon size={20} strokeWidth={1.5} />
          </button>
        </Link>
      </div>
    </Header>
  );
}

function viewModeLabel(mode: NotebookViewMode): string {
  return mode === "cells" ? "Notebook" : "DAG";
}
