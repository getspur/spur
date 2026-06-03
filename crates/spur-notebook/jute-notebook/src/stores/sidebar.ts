import { create } from "zustand";

export type SidebarState = {
  activePanelId: string;
  collapsed: boolean;
};

export type SidebarActions = {
  activatePanel: (id: string) => void;
  toggleCollapsed: () => void;
  setCollapsed: (collapsed: boolean) => void;
};

export type SidebarStore = SidebarState & SidebarActions;

// Must match SIDEBAR_PANELS[0].id in src/ui/notebook/sidebar/panels.ts.
export const DEFAULT_SIDEBAR_PANEL_ID = "datasources";

export const useSidebar = create<SidebarStore>()((set) => ({
  activePanelId: DEFAULT_SIDEBAR_PANEL_ID,
  collapsed: false,

  activatePanel: (id) => set({ activePanelId: id, collapsed: false }),
  toggleCollapsed: () => set((state) => ({ collapsed: !state.collapsed })),
  setCollapsed: (collapsed) => set({ collapsed }),
}));
