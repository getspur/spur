import clsx from "clsx";
import {
  AlertTriangle,
  ChevronDown,
  Clock,
  Globe,
  Plus,
  X,
} from "lucide-react";

import type { CellCronTrigger } from "@/bindings/CellCronTrigger";

import { removeCellSchedule, setCellSchedule } from "./scheduleApi";

type ScheduleSectionProps = {
  cellId: string;
  heading?: string;
  variant?: "section" | "compact";
  version: number;
  schedule?: CellCronTrigger;
};

type Preset = {
  label: string;
  cron: string;
};

const DEFAULT_TRIGGER: CellCronTrigger = {
  enabled: true,
  cron: "*/15 * * * *",
  timezone: "UTC",
  run_target: "cascade",
  skip_if_running: true,
  catch_up: false,
};

const PRESETS: Preset[] = [
  { label: "5m", cron: "*/5 * * * *" },
  { label: "15m", cron: "*/15 * * * *" },
  { label: "1h", cron: "0 * * * *" },
  { label: "Daily", cron: "0 6 * * *" },
  { label: "Weekly", cron: "0 6 * * 1" },
  { label: "Custom", cron: "" },
];

export function ScheduleSection({
  cellId,
  heading = "Schedule",
  schedule,
  variant = "section",
  version,
}: ScheduleSectionProps) {
  const current = schedule ?? DEFAULT_TRIGGER;
  const compact = variant === "compact";
  const selectedPreset =
    PRESETS.find((preset) => preset.cron && preset.cron === current.cron)
      ?.label ?? "Custom";

  const save = (trigger: CellCronTrigger) => {
    void setCellSchedule(cellId, trigger, version);
  };

  return (
    <section>
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="text-[11px] font-semibold uppercase tracking-normal text-gray-500">
          {heading}
        </div>
        {schedule ? (
          <button
            type="button"
            aria-label="Remove schedule"
            className="inline-flex h-7 w-7 items-center justify-center rounded border border-gray-200 bg-white text-gray-500 transition-colors hover:border-gray-300 hover:bg-gray-50 hover:text-gray-700"
            onClick={() => {
              void removeCellSchedule(cellId, version);
            }}
          >
            <X size={14} />
          </button>
        ) : null}
      </div>

      {!schedule ? (
        <div
          className={clsx(
            "rounded border border-dashed border-gray-200 bg-gray-50",
            compact ? "px-2.5 py-2.5" : "px-3 py-3",
          )}
        >
          <div className="flex items-start gap-2">
            <Clock className="mt-0.5 text-gray-400" size={16} />
            <div className="min-w-0">
              <div className="text-sm font-semibold text-gray-900">
                No schedule
              </div>
              <p className="mt-1 text-xs leading-5 text-gray-500">
                Run this cell on a recurring local kernel schedule.
              </p>
              <button
                type="button"
                className="mt-3 inline-flex items-center gap-1.5 rounded bg-gray-900 px-2.5 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-gray-800"
                onClick={() => save(DEFAULT_TRIGGER)}
              >
                <Plus size={14} />
                Add schedule trigger
              </button>
            </div>
          </div>
        </div>
      ) : (
        <div
          className={clsx(
            "grid gap-3 rounded border border-gray-200 bg-white text-xs",
            compact ? "p-2.5" : "p-3",
          )}
        >
          <div className="flex items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-2">
              <span
                className={clsx(
                  "inline-flex h-7 w-7 items-center justify-center rounded",
                  current.enabled
                    ? "bg-violet-100 text-violet-700"
                    : "bg-gray-100 text-gray-500",
                )}
              >
                <Clock size={15} />
              </span>
              <div className="min-w-0">
                <div className="font-semibold text-gray-900">
                  {current.enabled ? "Armed" : "Paused"}
                </div>
                <div className="truncate text-gray-500">{current.cron}</div>
              </div>
            </div>
            <button
              type="button"
              aria-pressed={current.enabled}
              className={clsx(
                "inline-flex h-6 w-11 items-center rounded-full p-0.5 transition-colors",
                current.enabled ? "bg-violet-600" : "bg-gray-200",
              )}
              onClick={() => save({ ...current, enabled: !current.enabled })}
            >
              <span
                className={clsx(
                  "h-5 w-5 rounded-full bg-white shadow-sm transition-transform",
                  current.enabled ? "translate-x-5" : "translate-x-0",
                )}
              />
            </button>
          </div>

          <div
            aria-label="Schedule preset"
            className="grid grid-cols-3 rounded border border-gray-200 bg-gray-50 p-0.5"
          >
            {PRESETS.map((preset) => (
              <button
                type="button"
                key={preset.label}
                className={clsx(
                  "rounded px-2 py-1 text-xs font-medium transition-colors",
                  selectedPreset === preset.label
                    ? "bg-gray-900 text-white shadow-sm"
                    : "text-gray-600 hover:bg-white hover:text-gray-900",
                )}
                onClick={() => {
                  if (preset.cron) {
                    save({ ...current, cron: preset.cron, enabled: true });
                  }
                }}
              >
                {preset.label}
              </button>
            ))}
          </div>

          <label className="grid gap-1">
            <span className="font-medium text-gray-700">Cron</span>
            <input
              className="rounded border border-gray-200 bg-white px-2 py-1.5 font-mono text-xs text-gray-900 outline-none transition-colors focus:border-violet-400 focus:ring-2 focus:ring-violet-100"
              value={current.cron}
              onChange={(event) =>
                save({ ...current, cron: event.currentTarget.value })
              }
            />
          </label>

          <div className="rounded bg-gray-50 px-2 py-1.5 text-gray-600">
            <span className="font-medium text-gray-700">Reads as:</span>{" "}
            {describeCron(current.cron)}
          </div>

          <div>
            <div className="mb-1 font-medium text-gray-700">Next runs</div>
            <ol className="grid gap-1 text-gray-500">
              {nextRuns(current.cron, current.timezone).map((run) => (
                <li key={run}>{run}</li>
              ))}
            </ol>
          </div>

          <label className="grid gap-1">
            <span className="font-medium text-gray-700">Runs target</span>
            <div className="relative">
              <select
                className="w-full appearance-none rounded border border-gray-200 bg-white px-2 py-1.5 pr-7 text-xs text-gray-900 outline-none transition-colors focus:border-violet-400 focus:ring-2 focus:ring-violet-100"
                value={current.run_target}
                onChange={(event) =>
                  save({
                    ...current,
                    run_target: event.currentTarget
                      .value as CellCronTrigger["run_target"],
                  })
                }
              >
                <option value="cascade">Cell and downstream</option>
                <option value="cell_only">Cell only</option>
              </select>
              <ChevronDown
                className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-gray-400"
                size={14}
              />
            </div>
          </label>

          <label className="grid gap-1">
            <span className="inline-flex items-center gap-1.5 font-medium text-gray-700">
              <Globe size={14} />
              Timezone
            </span>
            <input
              className="rounded border border-gray-200 bg-white px-2 py-1.5 text-xs text-gray-900 outline-none transition-colors focus:border-violet-400 focus:ring-2 focus:ring-violet-100"
              value={current.timezone}
              onChange={(event) =>
                save({ ...current, timezone: event.currentTarget.value })
              }
            />
          </label>

          <div className="grid gap-2">
            <PolicyToggle
              checked={current.skip_if_running}
              label="Skip if running"
              onChange={(checked) =>
                save({ ...current, skip_if_running: checked })
              }
            />
            <PolicyToggle
              checked={current.catch_up}
              label="Catch up"
              onChange={(checked) => save({ ...current, catch_up: checked })}
            />
          </div>

          <div className="flex items-start gap-2 rounded bg-amber-50 px-2 py-2 text-amber-800">
            <AlertTriangle className="mt-0.5 shrink-0" size={14} />
            <span>
              Runs only while the local kernel and notebook daemon are awake.
            </span>
          </div>
        </div>
      )}
    </section>
  );
}

function PolicyToggle({
  checked,
  label,
  onChange,
}: {
  checked: boolean;
  label: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex items-center justify-between gap-3 rounded border border-gray-200 px-2 py-1.5">
      <span className="font-medium text-gray-700">{label}</span>
      <input
        type="checkbox"
        aria-label={label}
        className="h-4 w-4 rounded border-gray-300 text-violet-600 focus:ring-violet-500"
        checked={checked}
        onChange={(event) => onChange(event.currentTarget.checked)}
      />
    </label>
  );
}

function describeCron(expr: string): string {
  switch (expr.trim()) {
    case "*/5 * * * *":
      return "Every 5 minutes";
    case "*/15 * * * *":
      return "Every 15 minutes";
    case "0 * * * *":
      return "Every hour";
    case "0 6 * * *":
      return "Daily at 6:00 AM";
    case "0 6 * * 1":
      return "Weekly on Monday at 6:00 AM";
    default:
      return "Custom schedule";
  }
}

function nextRuns(expr: string, timezone: string): string[] {
  const intervalMinutes = intervalMinutesFor(expr);
  if (!intervalMinutes) {
    return [`Calculated by scheduler in ${timezone}`];
  }

  const formatter = new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
    timeZone: timezone || "UTC",
  });
  const start = Date.now();

  return [1, 2, 3].map((step) =>
    formatter.format(new Date(start + intervalMinutes * step * 60 * 1000)),
  );
}

function intervalMinutesFor(expr: string): number | undefined {
  switch (expr.trim()) {
    case "*/5 * * * *":
      return 5;
    case "*/15 * * * *":
      return 15;
    case "0 * * * *":
      return 60;
    case "0 6 * * *":
      return 24 * 60;
    case "0 6 * * 1":
      return 7 * 24 * 60;
    default:
      return undefined;
  }
}
