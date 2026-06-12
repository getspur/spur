import { beforeEach, describe, expect, test } from "vitest";

import { DEFAULT_SIDEBAR_PANEL_ID, useSidebar } from "../sidebar";

describe("useSidebar", () => {
  beforeEach(() => {
    useSidebar.setState({
      activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
      collapsed: false,
      width: 420,
    });
  });

  test("starts on the default panel, expanded", () => {
    const state = useSidebar.getState();
    expect(state.activePanelId).toBe(DEFAULT_SIDEBAR_PANEL_ID);
    expect(state.collapsed).toBe(false);
  });

  test("activatePanel sets the id and clears collapsed", () => {
    useSidebar.setState({ collapsed: true });
    useSidebar.getState().activatePanel("chat");
    const state = useSidebar.getState();
    expect(state.activePanelId).toBe("chat");
    expect(state.collapsed).toBe(false);
  });

  test("toggleCollapsed flips collapsed", () => {
    useSidebar.getState().toggleCollapsed();
    expect(useSidebar.getState().collapsed).toBe(true);
    useSidebar.getState().toggleCollapsed();
    expect(useSidebar.getState().collapsed).toBe(false);
  });

  test("setCollapsed sets collapsed explicitly", () => {
    useSidebar.getState().setCollapsed(true);
    expect(useSidebar.getState().collapsed).toBe(true);
    useSidebar.getState().setCollapsed(false);
    expect(useSidebar.getState().collapsed).toBe(false);
  });

  test("setWidth clamps sidebar width to readable bounds", () => {
    useSidebar.getState().setWidth(760);
    expect(useSidebar.getState().width).toBe(720);

    useSidebar.getState().setWidth(260);
    expect(useSidebar.getState().width).toBe(320);
  });
});
