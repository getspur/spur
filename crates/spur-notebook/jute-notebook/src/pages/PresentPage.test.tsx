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
          juteDeckMetadata: undefined as { layout: string } | undefined,
        },
      },
    },
    viewState: {
      selectedCellId: null,
      isLoading: false,
      viewMode: "cells",
    },
    editBuffer: {
      cellSources: {},
    },
    dagStatus: {},
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

  it("labels code slides with the notebook language", () => {
    mocks.state.serverState.notebookMetadata = {
      language_info: { name: "rust" },
    };
    mocks.state.serverState.cells["slide-1"] = {
      type: "code",
      initialText: "fn main() {}",
      source: "fn main() {}",
      version: 1,
      juteDeckMetadata: { layout: "code" },
    };

    const { container } = render(<PresentPage />);

    expect(container).toHaveTextContent("rust");
  });

  it("falls back to python when notebook language is absent", () => {
    mocks.state.serverState.cells["slide-1"] = {
      type: "code",
      initialText: "puts 'hello'",
      source: "puts 'hello'",
      version: 1,
      juteDeckMetadata: { layout: "code" },
    };

    const { container } = render(<PresentPage />);

    expect(container).toHaveTextContent("python");
  });
});
