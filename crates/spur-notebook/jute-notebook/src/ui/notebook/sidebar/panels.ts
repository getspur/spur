import { BotIcon, DatabaseIcon } from "lucide-react";

import { DEFAULT_SIDEBAR_PANEL_ID } from "@/stores/sidebar";

import ChatPanel from "./ChatPanel";
import DatasourcePanel from "./DatasourcePanel";
import type { SidebarPanel } from "./types";

export const SIDEBAR_PANELS: SidebarPanel[] = [
  {
    id: DEFAULT_SIDEBAR_PANEL_ID,
    title: "Datasources",
    icon: DatabaseIcon,
    ariaLabel: "Datasources",
    Component: DatasourcePanel,
  },
  {
    id: "agent",
    title: "AI Agent",
    icon: BotIcon,
    ariaLabel: "AI Agent",
    Component: ChatPanel,
  },
];
