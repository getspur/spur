import {
  AlertTriangleIcon,
  ClockIcon,
  PauseIcon,
  XIcon,
} from "lucide-react";
import { useEffect, useId, useState } from "react";

import {
  type ScheduleSnapshotEntry,
  listSchedules,
} from "../dag/scheduleApi";

type Props = {
  onClose: () => void;
};

export function SchedulesOverview({ onClose }: Props) {
  const titleId = useId();
  const [entries, setEntries] = useState<ScheduleSnapshotEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      try {
        setError(null);
        const schedules = await listSchedules();
        if (!cancelled) {
          setEntries(schedules.filter((entry) => entry.trigger.enabled));
        }
      } catch {
        if (!cancelled) {
          setEntries([]);
          setError("Schedules unavailable");
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void load();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  const pauseAll = () => {
    setEntries((current) =>
      current.map((entry) => ({
        ...entry,
        trigger: { ...entry.trigger, enabled: false },
      })),
    );
  };

  return (
    <div
      aria-labelledby={titleId}
      aria-modal="true"
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/30"
      onClick={onClose}
      role="dialog"
    >
      <div
        className="w-full max-w-3xl rounded border border-gray-300 bg-white p-5 shadow-xl"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4">
          <div>
            <div className="flex items-center gap-2 text-gray-950">
              <ClockIcon size={18} />
              <h2 className="text-lg" id={titleId}>
                Schedules
              </h2>
            </div>
            <p className="mt-1 text-sm text-gray-600">
              Armed cells in this notebook.
            </p>
          </div>
          <button
            className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-950"
            onClick={onClose}
            type="button"
            aria-label="Close schedules"
          >
            <XIcon size={18} />
          </button>
        </div>

        <div className="mt-5 overflow-hidden rounded border border-gray-200">
          <table className="w-full border-collapse text-left text-sm">
            <thead className="bg-gray-50 text-xs uppercase text-gray-500">
              <tr>
                <th className="px-3 py-2 font-medium">Cell</th>
                <th className="px-3 py-2 font-medium">Schedule</th>
                <th className="px-3 py-2 font-medium">Next run</th>
                <th className="px-3 py-2 font-medium">Last run</th>
                <th className="px-3 py-2 font-medium">Enabled</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-200">
              {loading ? (
                <tr>
                  <td className="px-3 py-5 text-gray-500" colSpan={5}>
                    Loading schedules...
                  </td>
                </tr>
              ) : error ? (
                <tr>
                  <td className="px-3 py-5 text-red-600" colSpan={5}>
                    {error}
                  </td>
                </tr>
              ) : entries.length === 0 ? (
                <tr>
                  <td className="px-3 py-5 text-gray-500" colSpan={5}>
                    No armed schedules.
                  </td>
                </tr>
              ) : (
                entries.map((entry) => (
                  <tr key={entry.cell_id} className="align-top">
                    <td className="px-3 py-3 font-medium text-gray-950">
                      {entry.cell_id}
                    </td>
                    <td className="px-3 py-3 text-gray-700">
                      <div>{entry.trigger.cron}</div>
                      <div className="text-xs text-gray-500">
                        {entry.trigger.timezone}
                      </div>
                    </td>
                    <td className="px-3 py-3 text-gray-700">
                      {formatDate(entry.next_fire)}
                    </td>
                    <td className="px-3 py-3">
                      <LastRunStatus entry={entry} />
                    </td>
                    <td className="px-3 py-3">
                      <label className="inline-flex items-center gap-2 text-xs text-gray-600">
                        <input
                          checked={entry.trigger.enabled}
                          className="h-4 w-4 rounded border-gray-300 text-violet-600"
                          onChange={() => {
                            setEntries((current) =>
                              current.map((item) =>
                                item.cell_id === entry.cell_id
                                  ? {
                                      ...item,
                                      trigger: {
                                        ...item.trigger,
                                        enabled: !item.trigger.enabled,
                                      },
                                    }
                                  : item,
                              ),
                            );
                          }}
                          type="checkbox"
                        />
                        Armed
                      </label>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>

        <div className="mt-5 flex items-center justify-between gap-4">
          <p className="text-xs text-gray-500">
            Schedules run on the local kernel while SPUR is open. Closed windows
            are skipped unless catch-up is on.
          </p>
          <button
            className="inline-flex shrink-0 items-center gap-2 rounded bg-gray-950 px-3 py-2 text-sm text-white transition-colors hover:bg-black"
            onClick={pauseAll}
            type="button"
          >
            <PauseIcon size={15} />
            Pause all
          </button>
        </div>
      </div>
    </div>
  );
}

function LastRunStatus({ entry }: { entry: ScheduleSnapshotEntry }) {
  if (!entry.last_run) {
    return <span className="text-gray-500">not run yet</span>;
  }

  if (entry.last_run.status === "failed") {
    const failures = entry.consecutive_failures ?? 1;
    return (
      <span className="inline-flex items-center gap-1 text-red-600">
        <AlertTriangleIcon size={14} />
        failed, {failures} in a row
      </span>
    );
  }

  return (
    <span className="inline-flex items-center gap-1 text-green-600">
      success
    </span>
  );
}

function formatDate(value?: string | null): string {
  if (!value) return "not scheduled";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    month: "short",
    year: "numeric",
  });
}

export default SchedulesOverview;
