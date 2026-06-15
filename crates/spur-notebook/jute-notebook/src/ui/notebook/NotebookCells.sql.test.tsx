import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

// SqlCellHeader is presentational, but importing it pulls in NotebookCells'
// module graph, so stub the heavy / store-bound imports the same way
// NotebookCells.test.tsx does.
vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    throw new Error("Notebook store not used by SqlCellHeader");
  },
}));
vi.mock("./CellInput", () => ({
  default: ({ cellId }: { cellId: string }) => (
    <div data-testid={`cell-input-${cellId}`} />
  ),
}));
vi.mock("../dag/scheduleApi", () => ({
  scheduleLabel: (cron: string) => cron,
  setCellSchedule: vi.fn(),
  removeCellSchedule: vi.fn(),
}));

import { SqlCellHeader } from "./NotebookCells";

afterEach(cleanup);

test("shows the kernel-session pill and the current relation name", () => {
  render(<SqlCellHeader relation="top_scorers" onRelationChange={() => {}} />);
  expect(screen.getByText(/kernel session/i)).toBeInTheDocument();
  expect(screen.getByLabelText(/relation/i)).toHaveValue("top_scorers");
});

test("editing the relation name fires onRelationChange", () => {
  const onRelationChange = vi.fn();
  render(<SqlCellHeader relation="" onRelationChange={onRelationChange} />);
  fireEvent.change(screen.getByLabelText(/relation/i), {
    target: { value: "elite" },
  });
  expect(onRelationChange).toHaveBeenCalledWith("elite");
});
