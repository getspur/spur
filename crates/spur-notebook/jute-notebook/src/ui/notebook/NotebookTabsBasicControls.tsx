import type { NotebookTab } from "@/stores/notebook";

type Props = {
  activeTabId?: string;
  tabs: NotebookTab[];
  onSwitchTab: (tabId: string) => void;
};

export default function NotebookTabsBasicControls({
  activeTabId: _activeTabId,
  onSwitchTab: _onSwitchTab,
  tabs: _tabs,
}: Props) {
  return null;
}
