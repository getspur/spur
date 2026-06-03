import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import DagNode from "./DagNode";
import type { DagNodeData } from "./useDagGraph";

afterEach(() => {
  cleanup();
});

function data(overrides: Partial<DagNodeData> = {}): DagNodeData {
  return {
    id: "a3",
    label: "summary",
    cellType: "code",
    code: "Summarise sales vs targets.",
    codePreview: "Summarise sales vs targets.",
    produces: [{ port: "summary", repr: "str", version: 5 }],
    consumes: [{ port: "sales", version: 3 }],
    state: "fresh",
    kind: "ai",
    aiLive: true,
    ...overrides,
  };
}

describe("DagNode", () => {
  it("renders the AI tag and LIVE pill for an ai node", () => {
    render(<DagNode data={data()} />);

    expect(screen.getByText("✦ AI")).toBeInTheDocument();
    expect(screen.getByText("● LIVE")).toBeInTheDocument();
    expect(
      screen.getByText("“Summarise sales vs targets."),
    ).toBeInTheDocument();
    expect(screen.getByText("T")).toBeInTheDocument();
  });

  it("renders manual pill when aiLive is false", () => {
    render(<DagNode data={data({ aiLive: false })} />);

    expect(screen.getByText("manual")).toBeInTheDocument();
  });

  it("does not render the AI tag for a code node", () => {
    render(<DagNode data={data({ kind: "code" })} />);

    expect(screen.queryByText(/✦/)).not.toBeInTheDocument();
    expect(screen.queryByText(/LIVE/)).not.toBeInTheDocument();
    expect(screen.queryByText("manual")).not.toBeInTheDocument();
  });
});
