import "@testing-library/jest-dom/vitest";

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SchedulesOverview } from "./SchedulesOverview";

vi.mock("../dag/scheduleApi", () => ({
  listSchedules: vi.fn().mockResolvedValue([
    {
      cell_id: "c1",
      trigger: {
        enabled: true,
        cron: "*/15 * * * *",
        timezone: "UTC",
        run_target: "cascade",
        skip_if_running: true,
        catch_up: false,
      },
      next_fire: "2026-06-14T14:30:00Z",
      last_run: {
        fired_at: "2026-06-14T14:15:00Z",
        status: "success",
        duration_ms: 1200,
        error: null,
      },
      consecutive_failures: 0,
      recent: [],
    },
    {
      cell_id: "nightly_export",
      trigger: {
        enabled: true,
        cron: "0 2 * * *",
        timezone: "UTC",
        run_target: "cascade",
        skip_if_running: true,
        catch_up: false,
      },
      next_fire: "2026-06-15T02:00:00Z",
      last_run: {
        fired_at: "2026-06-14T02:00:00Z",
        status: "failed",
        duration_ms: null,
        error: "boom",
      },
      consecutive_failures: 2,
      recent: [],
    },
  ]),
}));

afterEach(() => cleanup());

describe("SchedulesOverview", () => {
  it("lists armed cells and surfaces failures", async () => {
    render(<SchedulesOverview onClose={() => {}} />);

    expect(await screen.findByText("c1")).toBeInTheDocument();
    expect(await screen.findByText(/failed, 2 in a row/i)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /pause all/i }),
    ).toBeInTheDocument();
  });
});
