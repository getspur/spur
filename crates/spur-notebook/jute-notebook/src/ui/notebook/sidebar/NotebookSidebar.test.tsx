import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "@/stores/sidebar";

vi.mock("./panels", () => {
  const Stub = (label: string) => () => <div>{label}</div>;
  const Icon = () => <span data-testid="icon" />;
  return {
    SIDEBAR_PANELS: [
      {
        id: DEFAULT_SIDEBAR_PANEL_ID,
        title: "Datasources",
        ariaLabel: "Datasources",
        icon: Icon,
        Component: Stub("ALPHA BODY"),
      },
      {
        id: "chat",
        title: "AI chat",
        ariaLabel: "AI chat",
        icon: Icon,
        Component: Stub("BETA BODY"),
      },
    ],
  };
});

import NotebookSidebar from "./NotebookSidebar";

beforeEach(() => {
  useSidebar.setState({
    activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
    collapsed: false,
  });
});
afterEach(cleanup);

describe("NotebookSidebar", () => {
  test("renders one rail button per panel plus a collapse toggle", () => {
    render(<NotebookSidebar />);
    expect(
      screen.getByRole("button", { name: "Datasources" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "AI chat" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /collapse sidebar/i }),
    ).toBeInTheDocument();
  });

  test("lazy-mounts: inactive panel is absent until activated, then stays mounted", () => {
    render(<NotebookSidebar />);
    expect(screen.getByText("ALPHA BODY")).toBeVisible();
    expect(screen.queryByText("BETA BODY")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "AI chat" }));
    expect(screen.getByText("BETA BODY")).toBeVisible();
    // alpha kept mounted but hidden
    expect(screen.getByText("ALPHA BODY")).not.toBeVisible();
  });

  test("collapse toggle hides panels and flips its label", () => {
    render(<NotebookSidebar />);
    fireEvent.click(
      screen.getByRole("button", { name: /collapse sidebar/i }),
    );
    expect(screen.getByText("ALPHA BODY")).not.toBeVisible();
    expect(
      screen.getByRole("button", { name: /expand sidebar/i }),
    ).toBeInTheDocument();
  });

  test("activating a panel while collapsed expands", () => {
    useSidebar.setState({ collapsed: true });
    render(<NotebookSidebar />);
    fireEvent.click(screen.getByRole("button", { name: "AI chat" }));
    expect(screen.getByText("BETA BODY")).toBeVisible();
    expect(useSidebar.getState().collapsed).toBe(false);
  });
});
