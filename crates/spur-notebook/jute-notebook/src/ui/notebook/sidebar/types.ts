import type { LucideIcon } from "lucide-react";
import type { ComponentType } from "react";

export type SidebarPanel = {
  id: string;
  title: string;
  icon: LucideIcon;
  ariaLabel: string;
  Component: ComponentType;
};
