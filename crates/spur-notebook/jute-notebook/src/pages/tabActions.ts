import type { NotebookTab } from "@/stores/notebook";

export function closeOthersTargets(
  tabs: readonly NotebookTab[],
  keepId: string,
): string[] {
  return tabs
    .filter((tab) => tab.id !== keepId && !tab.pinned)
    .map((tab) => tab.id);
}

export function closeRightTargets(
  tabs: readonly NotebookTab[],
  fromId: string,
): string[] {
  const index = tabs.findIndex((tab) => tab.id === fromId);
  if (index < 0) return [];
  return tabs
    .slice(index + 1)
    .filter((tab) => !tab.pinned)
    .map((tab) => tab.id);
}

export function cycleTabId(
  tabs: readonly NotebookTab[],
  activeTabId: string | undefined,
  offset: number,
): string | undefined {
  if (tabs.length === 0) return undefined;
  const index = Math.max(
    0,
    tabs.findIndex((tab) => tab.id === activeTabId),
  );
  return tabs[(index + offset + tabs.length) % tabs.length]?.id;
}

export function jumpTabId(
  tabs: readonly NotebookTab[],
  digit: number,
): string | undefined {
  if (digit === 9) return tabs[tabs.length - 1]?.id;
  return tabs[digit - 1]?.id;
}
