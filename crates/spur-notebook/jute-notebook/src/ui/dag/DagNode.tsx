import clsx from "clsx";

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

// Status is the ONLY colour language on a node. The 3px left rail + the header
// dot carry run state; the card itself stays a neutral surface so a wall of
// nodes reads as topology first, status second.
const STATE_RAIL: Record<DagNodeState, string> = {
  fresh: "bg-emerald-500",
  stale: "bg-amber-500",
  running: "bg-blue-500",
  failed: "bg-red-500",
  "upstream-failed": "bg-red-300",
  "never-run": "bg-gray-300",
};

const STATE_DOT: Record<DagNodeState, string> = {
  fresh: "bg-emerald-500",
  stale: "bg-amber-500",
  running: "bg-blue-500 animate-pulse",
  failed: "bg-red-500",
  "upstream-failed": "border-[1.5px] border-red-400 bg-white",
  "never-run": "border-[1.5px] border-gray-300 bg-white",
};

export default function DagNode({
  data,
  onSelect,
  selected = false,
}: DagNodeProps) {
  const hasConsumes = data.consumes.length > 0 || Boolean(data.source);
  const isAi = data.kind === "ai";

  return (
    <article
      aria-label={`Select ${data.id}`}
      className={clsx(
        "flex w-[224px] overflow-hidden rounded border border-gray-200 bg-white text-left shadow-sm transition-shadow hover:shadow-md",
        selected && "ring-2 ring-gray-900/25",
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
      <div className={clsx("w-[3px] shrink-0", STATE_RAIL[data.state])} />
      <div className="min-w-0 flex-1 px-2.5 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <span
            className={clsx(
              "h-2 w-2 shrink-0 rounded-full",
              STATE_DOT[data.state],
            )}
          />
          <h2 className="min-w-0 flex-1 truncate text-[12.5px] font-semibold text-gray-900">
            {data.label}
          </h2>
          <span className="shrink-0 font-mono text-[10px] text-gray-400">
            {data.id}
          </span>
        </div>
        {isAi ? (
          <div className="mt-1.5 flex items-center gap-1.5">
            <span className="inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-1.5 py-px font-mono text-[9.5px] font-semibold text-violet-700">
              ✦ AI
            </span>
            <span
              className={clsx(
                "rounded border px-1.5 py-px font-mono text-[9px]",
                data.aiLive
                  ? "border-violet-600 bg-violet-600 text-white"
                  : "border-gray-300 bg-white text-gray-500",
              )}
            >
              {data.aiLive ? "● LIVE" : "manual"}
            </span>
          </div>
        ) : null}
        {isAi ? (
          <p className="mt-1 line-clamp-2 text-[11px] leading-snug text-gray-700">
            “{data.codePreview}
          </p>
        ) : (
          <p className="mt-1 truncate font-mono text-[10.5px] text-gray-500">
            {data.codePreview}
          </p>
        )}
        <div className="mt-2 flex items-center justify-between gap-2 font-mono text-[10px]">
          <div className="flex min-w-0 items-center gap-1.5 overflow-hidden">
            {hasConsumes ? (
              <>
                <span className="shrink-0 text-gray-400">↓</span>
                {data.consumes.map((port) => (
                  <ConsumedToken key={port.port} port={port} />
                ))}
                {data.source ? <SourceToken source={data.source} /> : null}
              </>
            ) : (
              <span className="text-gray-300">no input</span>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            {data.produces.length > 0 ? (
              <>
                {isAi ? (
                  <span className="rounded-sm border border-gray-300 px-0.5 font-mono text-[8px] text-gray-500">
                    T
                  </span>
                ) : null}
                {data.produces.map((port) => (
                  <ProducedToken key={port.port} port={port} />
                ))}
                <span className="shrink-0 text-gray-400">↑</span>
              </>
            ) : (
              <span className="text-gray-300">sink</span>
            )}
          </div>
        </div>
      </div>
    </article>
  );
}

function ProducedToken({ port }: { port: DagProducedPort }) {
  return (
    <span className="inline-flex items-center gap-1 whitespace-nowrap">
      <span className="text-gray-600">{port.display ?? port.port}</span>
      <span className="text-gray-400">v{port.version}</span>
    </span>
  );
}

function ConsumedToken({ port }: { port: DagConsumedPort }) {
  const version = port.version === undefined ? "v?" : `v${port.version}`;
  return (
    <span className="inline-flex min-w-0 items-center gap-1 whitespace-nowrap">
      {port.stale ? (
        <span className="h-1 w-1 shrink-0 rounded-full bg-amber-500" />
      ) : null}
      <span
        className={clsx(
          "truncate",
          port.stale ? "text-amber-700" : "text-gray-600",
        )}
      >
        {port.port}
      </span>
      <span className={port.stale ? "text-amber-500" : "text-gray-400"}>
        {version}
      </span>
    </span>
  );
}

function SourceToken({ source }: { source: string }) {
  return <span className="truncate text-gray-500">{source}</span>;
}
