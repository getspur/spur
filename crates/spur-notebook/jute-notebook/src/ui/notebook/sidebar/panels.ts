import { DatabaseIcon } from "lucide-react";

import DatasourcePanel from "./DatasourcePanel";
import type { SidebarPanel } from "./types";

export const SIDEBAR_PANELS: SidebarPanel[] = [
  {
    id: "datasources",
    title: "Datasources",
    icon: DatabaseIcon,
    ariaLabel: "Datasources",
    Component: DatasourcePanel,
  },
];
