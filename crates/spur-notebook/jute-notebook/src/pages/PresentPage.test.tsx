import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import PresentPage from "./PresentPage";

const mocks = vi.hoisted(() => {
  const source = "### Three things\n- first\n- second\n- third";
  const makeState = () => ({
    serverState: {
      lastAppliedVersion: 0,
      notebookMetadata: {},
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
  });
  const state = makeState();

  return {
    loadNotebookFromPath: vi.fn(),
    makeState,
    setLocation: vi.fn(),
    state,
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
  beforeEach(() => {
    Object.assign(mocks.state, mocks.makeState());
    mocks.loadNotebookFromPath.mockReset();
    mocks.setLocation.mockReset();
  });

  it("renders non-fragment bullet slides without dimmed bullets", () => {
    const { container } = render(<PresentPage />);

    expect(container).toHaveTextContent("first");
    expect(container).toHaveTextContent("second");
    expect(container).toHaveTextContent("third");
    expect(container.querySelectorAll(".opacity-30")).toHaveLength(0);
  });

  it("applies the notebook-level deck theme in present mode", () => {
    mocks.state.serverState.notebookMetadata = {
      jute_deck: { theme: "spur-brand" },
    };

    const { container } = render(<PresentPage />);

    expect(container.querySelector("[data-slide]")).toHaveClass(
      "from-indigo-900",
    );
  });
});
