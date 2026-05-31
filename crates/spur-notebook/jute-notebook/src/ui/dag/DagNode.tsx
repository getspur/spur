import clsx from "clsx";
import type { ReactNode } from "react";

import type {
  DagConsumedPort,
  DagNodeData,
  DagNodeState,
  DagProducedPort,
} from "./useDagGraph";

type DagNodeProps = {
  data: DagNodeData;
  onSelect?: (id: string) => void;
  selected?: boolean;
};

const STATE_BORDER_COLORS: Record<DagNodeState, string> = {
  fresh: "border-emerald-500 bg-emerald-50/30",
  stale: "border-amber-500 bg-amber-50/40",
  running: "animate-pulse border-blue-500 bg-blue-50/40",
  failed: "border-red-500 bg-red-50/40",
  "upstream-failed": "border-red-400 border-dashed bg-red-50/20",
  "never-run": "border-gray-300 bg-white",
};

const STATE_BADGE_COLORS: Record<DagNodeState, string> = {
  fresh: "bg-emerald-100 text-emerald-800",
  stale: "bg-amber-100 text-amber-800",
  running: "bg-blue-100 text-blue-800",
  failed: "bg-red-100 text-red-800",
  "upstream-failed": "bg-red-100 text-red-700",
  "never-run": "bg-gray-100 text-gray-600",
};

export default function DagNode({
  data,
  onSelect,
  selected = false,
}: DagNodeProps) {
  return (
    <article
      aria-label={`Select ${data.label}`}
      className={clsx(
        "h-44 w-[280px] overflow-hidden rounded border text-left shadow-sm transition-colors",
        STATE_BORDER_COLORS[data.state],
        selected && "ring-2 ring-gray-900/20",
      )}
      onClick={() => onSelect?.(data.id)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect?.(data.id);
        }
      }}
      role="button"
      tabIndex={0}
    >
      <div className="border-b border-gray-100 px-3 py-2">
        <div className="flex min-w-0 items-center justify-between gap-2">
          <h2 className="truncate text-sm font-semibold text-gray-950">
            {data.label}
          </h2>
          <span className="shrink-0 rounded bg-gray-100 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-normal text-gray-600">
            {data.cellType}
          </span>
        </div>
        <div className="mt-1 flex min-w-0 items-center justify-between gap-2">
          <p className="truncate font-mono text-xs text-gray-500">
            {data.codePreview}
          </p>
          <span
            className={clsx(
              "shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-normal",
              STATE_BADGE_COLORS[data.state],
            )}
          >
            {data.state}
          </span>
        </div>
      </div>
      <div className="grid gap-2 px-3 py-2 text-xs">
        <PortSection label="Consumes">
          {data.consumes.length > 0 || data.source ? (
            <>
              {data.consumes.map((port) => (
                <ConsumedPortChip key={port.port} port={port} />
              ))}
              {data.source ? <SourceChip source={data.source} /> : null}
            </>
          ) : (
            <EmptyPorts />
          )}
        </PortSection>
        <PortSection label="Produces">
          {data.produces.length > 0 ? (
            data.produces.map((port) => (
              <ProducedPortChip key={port.port} port={port} />
            ))
          ) : (
            <EmptyPorts />
          )}
        </PortSection>
      </div>
    </article>
  );
}

function PortSection({
  children,
  label,
}: {
  children: ReactNode;
  label: string;
}) {
  return (
    <section>
      <div className="mb-1 text-[10px] font-semibold uppercase tracking-normal text-gray-500">
        {label}
      </div>
      <div className="flex min-h-6 flex-wrap gap-1">{children}</div>
    </section>
  );
}

function ProducedPortChip({ port }: { port: DagProducedPort }) {
  return (
    <span className="max-w-full truncate rounded bg-emerald-50 px-2 py-1 text-[11px] font-medium text-emerald-800">
      {port.display ?? port.port}
      <span className="ml-1 text-emerald-600">v{port.version}</span>
    </span>
  );
}

function ConsumedPortChip({ port }: { port: DagConsumedPort }) {
  return (
    <span
      className={clsx(
        "max-w-full truncate rounded px-2 py-1 text-[11px] font-medium",
        port.stale ? "bg-amber-100 text-amber-800" : "bg-sky-50 text-sky-800",
      )}
    >
      {port.port}
      <span
        className={clsx("ml-1", port.stale ? "text-amber-700" : "text-sky-600")}
      >
        {port.version === undefined ? "v?" : `v${port.version}`}
      </span>
    </span>
  );
}

function SourceChip({ source }: { source: string }) {
  return (
    <span className="max-w-full truncate rounded bg-indigo-50 px-2 py-1 text-[11px] font-medium text-indigo-800">
      {source}
    </span>
  );
}

function EmptyPorts() {
  return <span className="text-[11px] text-gray-400">None</span>;
}
