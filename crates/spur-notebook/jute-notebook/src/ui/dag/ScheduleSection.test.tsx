import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { ScheduleSection } from "./ScheduleSection";
import { removeCellSchedule, setCellSchedule } from "./scheduleApi";

vi.mock("./scheduleApi", () => ({
  setCellSchedule: vi.fn().mockResolvedValue(undefined),
  removeCellSchedule: vi.fn().mockResolvedValue(undefined),
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("ScheduleSection", () => {
  test("shows empty state when no schedule", () => {
    render(<ScheduleSection cellId="c1" version={1} schedule={undefined} />);

    expect(screen.getByText("No schedule")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /add schedule trigger/i }),
    ).toBeInTheDocument();
  });

  test("adds a default schedule from the empty state", () => {
    render(<ScheduleSection cellId="c1" version={7} schedule={undefined} />);

    fireEvent.click(
      screen.getByRole("button", { name: /add schedule trigger/i }),
    );

    expect(setCellSchedule).toHaveBeenCalledWith(
      "c1",
      {
        enabled: true,
        cron: "*/15 * * * *",
        timezone: "UTC",
        run_target: "cascade",
        skip_if_running: true,
        catch_up: false,
      },
      7,
    );
  });

  test("renders configured state and arms via preset", () => {
    render(
      <ScheduleSection
        cellId="c1"
        version={2}
        schedule={{
          enabled: true,
          cron: "*/15 * * * *",
          timezone: "UTC",
          run_target: "cascade",
          skip_if_running: true,
          catch_up: false,
        }}
      />,
    );

    expect(screen.getByDisplayValue("*/15 * * * *")).toBeInTheDocument();
    expect(screen.getByText(/Every 15 minutes/i)).toBeInTheDocument();
    expect(screen.getByText("Next runs")).toBeInTheDocument();
    expect(screen.getByLabelText("Skip if running")).toBeChecked();
    expect(screen.getByLabelText("Catch up")).not.toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "1h" }));

    expect(setCellSchedule).toHaveBeenCalledWith(
      "c1",
      expect.objectContaining({
        enabled: true,
        cron: "0 * * * *",
      }),
      2,
    );
  });

  test("removes a configured schedule", () => {
    render(
      <ScheduleSection
        cellId="c1"
        version={3}
        schedule={{
          enabled: true,
          cron: "*/15 * * * *",
          timezone: "UTC",
          run_target: "cascade",
          skip_if_running: true,
          catch_up: false,
        }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Remove schedule" }));

    expect(removeCellSchedule).toHaveBeenCalledWith("c1", 3);
  });

  test("renders no em-dash or en-dash in output", () => {
    const { container } = render(
      <ScheduleSection
        cellId="c1"
        version={2}
        schedule={{
          enabled: true,
          cron: "*/15 * * * *",
          timezone: "UTC",
          run_target: "cascade",
          skip_if_running: true,
          catch_up: false,
        }}
      />,
    );

    expect(container.textContent || "").not.toMatch(/[—–]/);
  });
});
