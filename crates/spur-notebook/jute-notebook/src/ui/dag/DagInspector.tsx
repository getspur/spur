import clsx from "clsx";
import type { ReactNode } from "react";

import type { NodeStatus } from "@/stores/notebook";

import type { DagPortManifest } from "./dagStatus";
import type { DagNodeData } from "./useDagGraph";

type DagInspectorProps = {
  node?: DagNodeData;
  portManifest: DagPortManifest;
  status?: NodeStatus;
};

export default function DagInspector({
  node,
  portManifest,
  status,
}: DagInspectorProps) {
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
        <span className="mt-2 inline-flex rounded bg-gray-100 px-2 py-1 text-xs font-medium text-gray-700">
          {status?.state ?? node.state}
        </span>
      </header>

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
        <div className="mb-2 text-[11px] font-semibold uppercase tracking-normal text-gray-500">
          Code
        </div>
        <textarea
          aria-label="Selected DAG node code"
          className="h-48 w-full resize-none rounded border border-gray-200 bg-gray-50 p-3 font-mono text-xs leading-5 text-gray-800 outline-none"
          readOnly
          value={node.code}
        />
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
