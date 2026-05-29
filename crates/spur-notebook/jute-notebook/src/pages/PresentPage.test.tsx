import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import PresentPage from "./PresentPage";

const mocks = vi.hoisted(() => {
  const source = "### Three things\n- first\n- second\n- third";
  const state = {
    serverState: {
      lastAppliedVersion: 0,
      cellIds: ["slide-1"],
      cells: {
        "slide-1": {
          type: "markdown",
          initialText: source,
          source,
          version: 1,
        },
      },
    },
    viewState: {
      selectedCellId: null,
      isLoading: false,
    },
    editBuffer: {
      cellSources: {},
    },
  };

  return {
    loadNotebookFromPath: vi.fn(),
    setLocation: vi.fn(),
    store: {
      getInitialState: () => state,
      getState: () => state,
      subscribe: () => () => {},
    },
  };
});

vi.mock("@/stores/notebook", async () => {
  const React = await vi.importActual<typeof import("react")>("react");

  return {
    Notebook: vi.fn().mockImplementation(() => ({
      loadNotebookFromPath: mocks.loadNotebookFromPath,
      store: mocks.store,
    })),
    NotebookContext: React.createContext(undefined),
  };
});

vi.mock("wouter", () => ({
  useLocation: () => ["/present", mocks.setLocation],
  useSearch: () => "",
}));

describe("PresentPage", () => {
  it("renders non-fragment bullet slides without dimmed bullets", () => {
    const { container } = render(<PresentPage />);

    expect(container).toHaveTextContent("first");
    expect(container).toHaveTextContent("second");
    expect(container).toHaveTextContent("third");
    expect(container.querySelectorAll(".opacity-30")).toHaveLength(0);
  });
});
