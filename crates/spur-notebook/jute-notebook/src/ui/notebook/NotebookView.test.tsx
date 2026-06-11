import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { type StoreApi, createStore } from "zustand/vanilla";

import NotebookView from "./NotebookView";

const mocks = vi.hoisted(() => ({
  notebook: undefined as { store: StoreApi<any> } | undefined,
}));

vi.mock("@/stores/notebook", () => ({
  useNotebook: () => {
    if (!mocks.notebook) throw new Error("Notebook mock not configured");
    return mocks.notebook;
  },
}));

vi.mock("@/ui/dag/DagView", () => ({
  default: () => <div data-testid="dag-view" />,
}));

vi.mock("./AppMode", () => ({
  default: () => <div data-testid="app-mode" />,
}));

vi.mock("./NotebookCells", () => ({
  default: () => <div data-testid="notebook-cells" />,
}));

vi.mock("./NotebookLocation", () => ({
  default: () => <div data-testid="notebook-location" />,
}));

vi.mock("./sidebar/ChatPanel", () => ({
  default: () => <div data-testid="chat-panel" />,
}));

vi.mock("./sidebar/DatasourcePanel", () => ({
  default: () => <div data-testid="datasource-panel" />,
}));

describe("NotebookView", () => {
  afterEach(() => {
    cleanup();
    mocks.notebook = undefined;
  });

  test("renders app mode as an app canvas without notebook document chrome", () => {
    mocks.notebook = {
      store: createStore<any>()(() => ({
        serverState: {
          cellIds: [],
          cells: {},
        },
        viewState: {
          path: "/Users/kevintruong/.spur/scratch/Untitled101.ipynb",
          loadError: null,
          viewMode: "app",
        },
      })),
    };

    const { container } = render(<NotebookView />);

    expect(screen.getByTestId("app-mode")).toBeInTheDocument();
    expect(screen.queryByTestId("notebook-location")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "AI Agent" }),
    ).toBeInTheDocument();
    expect(container.firstElementChild).toHaveClass(
      "grid-cols-[minmax(0,1fr),auto]",
    );
    expect(container.querySelector(".py-16")).not.toBeInTheDocument();
  });
});
