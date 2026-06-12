import { create } from "zustand";

export type SidebarState = {
  activePanelId: string;
  collapsed: boolean;
  width: number;
};

export type SidebarActions = {
  activatePanel: (id: string) => void;
  toggleCollapsed: () => void;
  setCollapsed: (collapsed: boolean) => void;
  setWidth: (width: number) => void;
};

export type SidebarStore = SidebarState & SidebarActions;

// Must match SIDEBAR_PANELS[0].id in src/ui/notebook/sidebar/panels.ts.
export const DEFAULT_SIDEBAR_PANEL_ID = "datasources";
export const DEFAULT_SIDEBAR_WIDTH = 420;
export const MIN_SIDEBAR_WIDTH = 320;
export const MAX_SIDEBAR_WIDTH = 720;

function clampSidebarWidth(width: number) {
  return Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, width));
}

export const useSidebar = create<SidebarStore>()((set) => ({
  activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
  collapsed: false,
  width: DEFAULT_SIDEBAR_WIDTH,

  activatePanel: (id) => set({ activePanelId: id, collapsed: false }),
  toggleCollapsed: () => set((state) => ({ collapsed: !state.collapsed })),
  setCollapsed: (collapsed) => set({ collapsed }),
  setWidth: (width) => set({ width: clampSidebarWidth(width) }),
}));
