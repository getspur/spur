import clsx from "clsx";
import { PlayIcon } from "lucide-react";
import { type ReactNode, Suspense, lazy, useState } from "react";
import { useStore } from "zustand";

import { daemonControl } from "@/daemon/control";
import { type NodeStatus, useNotebook } from "@/stores/notebook";

import CellInputFallback from "../notebook/CellInputFallback";
import {
  type DagPortManifest,
  runNotebookCascade,
  runNotebookCell,
} from "./dagStatus";
import type { DagNodeData } from "./useDagGraph";

const CellInput = lazy(() => import("../notebook/CellInput"));

type DagInspectorProps = {
  node?: DagNodeData;
  onRunError?: (error: unknown) => void;
  portManifest: DagPortManifest;
  status?: NodeStatus;
};

export default function DagInspector({
  node,
  onRunError,
  portManifest,
  status,
}: DagInspectorProps) {
  const notebook = useNotebook();
  const [lastRunSourceByCell, setLastRunSourceByCell] = useState<
    Record<string, string>
  >({});
  const [runningCellId, setRunningCellId] = useState<string | null>(null);
  const [cascadingCellId, setCascadingCellId] = useState<string | null>(null);
  const currentSource = useStore(notebook.store, (state) => {
    if (!node) return undefined;
    return (
      state.editBuffer.cellSources[node.id]?.source ??
      state.serverState.cells[node.id]?.source ??
      node.code
    );
  });
  const serverSource = useStore(notebook.store, (state) => {
    if (!node) return undefined;
    return state.serverState.cells[node.id]?.source;
  });
  const lastRunSource = node && (lastRunSourceByCell[node.id] ?? node.code);
  const isEdited =
    node && currentSource !== undefined && currentSource !== lastRunSource;
  const isRunning =
    node && (runningCellId === node.id || status?.state === "running");
  const isCascading = node && cascadingCellId === node.id;

  const runNode = async () => {
    if (!node) return;
    setRunningCellId(node.id);
    try {
      const sourceAtRun = currentSource ?? node.code;
      if (serverSource !== undefined && sourceAtRun !== serverSource) {
        const response = await daemonControl({
          command: "apply_edit",
          id: node.id,
          source: sourceAtRun,
        });
        if (!response.ok) {
          throw new Error(
            response.error?.message ?? "Failed to apply pending cell edit",
          );
        }
      }
      await runNotebookCell(node.id);
      setLastRunSourceByCell((previous) => ({
        ...previous,
        [node.id]: sourceAtRun,
      }));
    } catch (error) {
      onRunError?.(error);
    } finally {
      setRunningCellId(null);
    }
  };

  const runDownstream = async () => {
    if (!node) return;
    setCascadingCellId(node.id);
    try {
      await runNotebookCascade(node.id);
    } catch (error) {
      onRunError?.(error);
    } finally {
      setCascadingCellId(null);
    }
  };

  if (!node) {
    return (
      <aside
        aria-label="DAG inspector"
        className="w-80 shrink-0 border-l border-gray-200 bg-white px-4 py-4 text-sm text-gray-500"
      >
        Select a node to inspect it.
      </aside>
    );
  }

  const isAi = node.kind === "ai";
  const aiLive = Boolean(node.aiLive);

  return (
    <aside
      aria-label="DAG inspector"
      className="flex w-80 shrink-0 flex-col gap-4 border-l border-gray-200 bg-white px-4 py-4"
    >
      <header>
        <div className="text-[11px] font-semibold uppercase tracking-normal text-gray-500">
          Selected node
        </div>
        <h2 className="mt-1 truncate text-base font-semibold text-gray-950">
          {node.label}
        </h2>
        <div className="mt-1 truncate font-mono text-xs text-gray-500">
          {node.id}
        </div>
        <div className="mt-2 flex items-center gap-2">
          <span className="inline-flex rounded bg-gray-100 px-2 py-1 text-xs font-medium text-gray-700">
            {status?.state ?? node.state}
          </span>
          {isAi ? (
            <span className="inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-2 py-1 font-mono text-xs font-semibold text-violet-700">
              ✦ AI
            </span>
          ) : null}
        </div>
      </header>

      {isAi ? (
        <section>
          <div className="mb-2 text-[11px] font-semibold uppercase tracking-normal text-gray-500">
            Mode
          </div>
          <div
            aria-disabled="true"
            className="inline-flex rounded border border-gray-200 bg-gray-50 p-0.5 opacity-60"
            title="Live auto-run requires backend wiring (bd-1bpb)"
          >
            <button
              type="button"
              aria-pressed={!aiLive}
              className={clsx(
                "rounded px-2.5 py-1 text-xs font-medium disabled:cursor-not-allowed",
                !aiLive ? "bg-white text-gray-900 shadow-sm" : "text-gray-500",
              )}
              disabled
            >
              manual
            </button>
            <button
              type="button"
              aria-pressed={aiLive}
              className={clsx(
                "rounded px-2.5 py-1 text-xs font-medium disabled:cursor-not-allowed",
                aiLive ? "bg-gray-900 text-white shadow-sm" : "text-gray-500",
              )}
              disabled
            >
              live
            </button>
          </div>
        </section>
      ) : null}

      <PortList title="Consumes">
        {node.consumes.length > 0 ? (
          node.consumes.map((port) => (
            <li
              key={port.port}
              className="flex items-center justify-between gap-3"
            >
              <span className="min-w-0 truncate font-medium text-gray-800">
                {port.port}
              </span>
              <VersionBadge
                currentVersion={portManifest[port.port] ?? port.version}
                ranVersion={
                  status?.ranPortVersions[port.port] ?? port.ranVersion
                }
              />
            </li>
          ))
        ) : (
          <EmptyPortRow />
        )}
      </PortList>

      <PortList title="Produces">
        {node.produces.length > 0 ? (
          node.produces.map((port) => (
            <li
              key={port.port}
              className="flex items-center justify-between gap-3"
            >
              <span className="min-w-0 truncate font-medium text-gray-800">
                {port.display ?? port.port}
              </span>
              <span className="shrink-0 rounded bg-emerald-50 px-2 py-1 text-xs font-medium text-emerald-700">
                v{portManifest[port.port] ?? port.version}
              </span>
            </li>
          ))
        ) : (
          <EmptyPortRow />
        )}
      </PortList>

      <section className="min-h-0">
        <div className="mb-2 flex items-center justify-between gap-3">
          <div className="text-[11px] font-semibold uppercase tracking-normal text-gray-500">
            {isAi ? "Prompt" : "Code"}
          </div>
          <div className="flex items-center gap-2">
            {isEdited && (
              <span className="rounded bg-amber-100 px-2 py-1 text-xs font-medium text-amber-800">
                Edited
              </span>
            )}
            <button
              type="button"
              className="inline-flex items-center gap-1.5 rounded border border-gray-200 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-800 transition-colors hover:border-gray-300 hover:bg-gray-50 disabled:cursor-not-allowed disabled:opacity-60"
              disabled={Boolean(isRunning)}
              onClick={() => {
                void runNode();
              }}
            >
              <PlayIcon size={14} />
              <span>{isRunning ? "Running" : "Run node"}</span>
            </button>
            <button
              type="button"
              className="inline-flex items-center gap-1.5 rounded border border-gray-200 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-800 transition-colors hover:border-gray-300 hover:bg-gray-50 disabled:cursor-not-allowed disabled:opacity-60"
              disabled={Boolean(isCascading)}
              onClick={() => {
                void runDownstream();
              }}
            >
              <PlayIcon size={14} />
              <span>{isCascading ? "Running" : "Run downstream"}</span>
            </button>
          </div>
        </div>
        <div className="h-48 overflow-auto rounded border border-gray-200 bg-white text-xs">
          <Suspense fallback={<CellInputFallback cellId={node.id} />}>
            <CellInput cellId={node.id} />
          </Suspense>
        </div>
      </section>
    </aside>
  );
}

function PortList({ children, title }: { children: ReactNode; title: string }) {
  return (
    <section>
      <div className="mb-2 text-[11px] font-semibold uppercase tracking-normal text-gray-500">
        {title}
      </div>
      <ul className="grid gap-2 text-xs">{children}</ul>
    </section>
  );
}

function VersionBadge({
  currentVersion,
  ranVersion,
}: {
  currentVersion?: number;
  ranVersion?: number;
}) {
  const bumped =
    currentVersion !== undefined &&
    ranVersion !== undefined &&
    currentVersion > ranVersion;

  return (
    <span
      className={clsx(
        "shrink-0 rounded px-2 py-1 text-xs font-medium",
        bumped ? "bg-amber-100 text-amber-800" : "bg-sky-50 text-sky-700",
      )}
    >
      {ranVersion === undefined
        ? currentVersion === undefined
          ? "v?"
          : `v${currentVersion}`
        : bumped
          ? `v${ranVersion} -> v${currentVersion}`
          : `v${ranVersion}`}
    </span>
  );
}

function EmptyPortRow() {
  return <li className="text-xs text-gray-400">None</li>;
}
