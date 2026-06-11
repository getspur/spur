import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useSearch } from "wouter";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { setActiveAgentNotebook } from "@/agent/bridge";
import {
  listenForNotebookEvents,
  listenForRecentNotebookChanges,
} from "@/agent/events";
import { daemonControl, pathFromDaemonControlResponse } from "@/daemon/control";
import {
  Notebook,
  NotebookContext,
  type NotebookTab,
  type NotebookTabKernelState,
  useNotebookTabsStore,
} from "@/stores/notebook";
import {
  AppGrantPromptContainer,
  ScriptsDisabledBanner,
} from "@/ui/notebook/AppGrantPrompt";
import HtmlScriptsNotice from "@/ui/notebook/HtmlScriptsNotice";
import NotebookCommandMenu from "@/ui/notebook/NotebookCommandMenu";
import NotebookFooter from "@/ui/notebook/NotebookFooter";
import NotebookHeader from "@/ui/notebook/NotebookHeader";
import NotebookTabStrip from "@/ui/notebook/NotebookTabStrip";
import NotebookView from "@/ui/notebook/NotebookView";
import ConfirmModal from "@/ui/shared/ConfirmModal";

import {
  activeTabIdFromSearch,
  notebookRouteForPaths,
  notebookRouteWithPath,
  pinnedPathsFromSearch,
} from "./notebookRoute";
import {
  closeOthersTargets,
  closeRightTargets,
  cycleTabId,
  jumpTabId,
} from "./tabActions";

type NotebookTabSpec = NotebookTab & {
  inline?: string;
};

type NotebookTabEntry = NotebookTabSpec & {
  notebook: Notebook;
};

type PendingCloseTarget = {
  id: string;
  tab: NotebookTab | undefined;
  entry: NotebookTabEntry | undefined;
};

export default function NotebookPage() {
  const search = useSearch();
  const [, setLocation] = useLocation();
  const tabSpecs = useMemo(() => tabsFromSearch(search), [search]);
  const routeActiveTabId = useMemo(
    () => activeTabIdFromSearch(search),
    [search],
  );
  const syncedSearchRef = useRef(search);
  const initialScratchRequestedRef = useRef(false);
  const loadedSourceByNotebookRef = useRef(new WeakMap<Notebook, string>());
  const [tabEntries, setTabEntries] = useState<NotebookTabEntry[]>(() =>
    tabSpecs.map(tabEntryFromSpec),
  );
  const [pendingCloseIds, setPendingCloseIds] = useState<string[] | null>(null);
  const [tabError, setTabError] = useState<string | null>(null);
  const tabs = useNotebookTabsStore((state) => state.tabs);
  const activeTabId = useNotebookTabsStore((state) => state.activeTabId);
  const setTabs = useNotebookTabsStore((state) => state.setTabs);
  const setActiveTabId = useNotebookTabsStore((state) => state.setActiveTabId);
  const updateTab = useNotebookTabsStore((state) => state.updateTab);
  const moveTab = useNotebookTabsStore((state) => state.moveTab);
  const setPinned = useNotebookTabsStore((state) => state.setPinned);
  const pushClosedTab = useNotebookTabsStore((state) => state.pushClosedTab);
  const popClosedTab = useNotebookTabsStore((state) => state.popClosedTab);
  const canReopen = useNotebookTabsStore(
    (state) => state.closedTabs.length > 0,
  );
  const activeEntry =
    tabEntries.find((entry) => entry.id === activeTabId) ?? tabEntries[0];
  const pendingCloseTargets =
    pendingCloseIds?.map((id) => ({
      id,
      tab: tabs.find((tab) => tab.id === id),
      entry: tabEntries.find((entry) => entry.id === id),
    })) ?? [];
  const riskyPendingCloseCount = pendingCloseTargets.filter(({ tab, entry }) =>
    tabRequiresCloseConfirmation(tab, entry),
  ).length;
  const confirmCloseNeeded =
    pendingCloseIds !== null && riskyPendingCloseCount > 0;

  useEffect(() => {
    if (syncedSearchRef.current === search) return;
    syncedSearchRef.current = search;
    setTabEntries((entries) => reconcileTabEntriesFromSpecs(entries, tabSpecs));
  }, [search, tabSpecs]);

  useEffect(() => {
    setTabs(tabEntries.map(tabFromEntry));
    if (
      routeActiveTabId &&
      tabEntries.some((entry) => entry.id === routeActiveTabId)
    ) {
      setActiveTabId(routeActiveTabId);
    }
  }, [routeActiveTabId, setActiveTabId, setTabs, tabEntries]);

  const closeMany = useCallback((ids: readonly string[]) => {
    const uniqueIds = [...new Set(ids)];
    if (uniqueIds.length === 0) return;
    setPendingCloseIds(uniqueIds);
  }, []);

  const requestCloseTab = useCallback(
    (tabId: string) => {
      closeMany([tabId]);
    },
    [closeMany],
  );

  const closeOthers = useCallback(
    (tabId: string) => closeMany(closeOthersTargets(tabs, tabId)),
    [closeMany, tabs],
  );

  const closeRight = useCallback(
    (tabId: string) => closeMany(closeRightTargets(tabs, tabId)),
    [closeMany, tabs],
  );

  const addOrFocusNotebookPath = useCallback(
    (path: string) => {
      setTabError(null);

      const existing = tabEntries.find((entry) => entry.path === path);
      const nextEntries =
        existing === undefined
          ? [
              ...tabEntries.filter((entry) => !isBlankPlaceholderTab(entry)),
              tabEntryFromSpec(tabFromPath(path)),
            ]
          : tabEntries;

      if (existing === undefined) {
        setTabEntries(nextEntries);
      }
      const nextTabs = nextEntries.map(tabFromEntry);
      setTabs(nextTabs);
      setActiveTabId(path);
      setLocation(
        notebookRouteWithPath(nextEntries, path, pinnedPathsFromTabs(nextTabs)),
      );
    },
    [setActiveTabId, setLocation, setTabs, tabEntries],
  );

  const addTab = useCallback(async () => {
    try {
      const response = await daemonControl({ command: "new", activate: false });
      addOrFocusNotebookPath(pathFromDaemonControlResponse(response, "new"));
    } catch (caught) {
      setTabError(errorMessage(caught));
    }
  }, [addOrFocusNotebookPath]);

  const openNotebookFromPicker = useCallback(async () => {
    try {
      const file = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
      });
      if (typeof file !== "string") return;

      const response = await daemonControl({
        command: "open",
        path: file,
        activate: false,
      });
      addOrFocusNotebookPath(pathFromDaemonControlResponse(response, "open"));
    } catch (caught) {
      setTabError(errorMessage(caught));
    }
  }, [addOrFocusNotebookPath]);

  const closeTab = useCallback(
    async (tabId: string) => {
      const currentTabs = useNotebookTabsStore.getState().tabs;
      const currentActiveTabId = useNotebookTabsStore.getState().activeTabId;
      const tab = currentTabs.find((candidate) => candidate.id === tabId);
      if (!tab) return;
      const removedIndex = currentTabs.findIndex(
        (candidate) => candidate.id === tabId,
      );

      await daemonControl({
        command: "close_notebook",
        notebook_id: tab.id,
      });

      if (tab.path) pushClosedTab({ tab, index: removedIndex });

      setTabEntries((entries) => entries.filter((entry) => entry.id !== tabId));
      const remainingTabs = currentTabs.filter(
        (candidate) => candidate.id !== tabId,
      );
      setTabs(remainingTabs);

      if (currentActiveTabId === tabId) {
        const nextTab =
          remainingTabs[Math.min(removedIndex, remainingTabs.length - 1)] ??
          remainingTabs[0];
        setActiveTabId(nextTab?.id);
      }
    },
    [pushClosedTab, setActiveTabId, setTabs],
  );

  const reopenClosedTab = useCallback(async () => {
    const record = popClosedTab();
    if (!record?.tab.path) return;
    try {
      const response = await daemonControl({
        command: "open",
        path: record.tab.path,
        activate: false,
      });
      addOrFocusNotebookPath(pathFromDaemonControlResponse(response, "open"));
      moveTab(record.tab.path, record.index);
    } catch (caught) {
      setTabError(errorMessage(caught));
    }
  }, [addOrFocusNotebookPath, moveTab, popClosedTab]);

  const togglePin = useCallback(
    (tabId: string) => {
      const tab = useNotebookTabsStore
        .getState()
        .tabs.find((candidate) => candidate.id === tabId);
      setPinned(tabId, !tab?.pinned);
      const orderedTabs = useNotebookTabsStore.getState().tabs;
      const orderedPaths = orderedTabs.flatMap((tab) =>
        tab.path ? [tab.path] : [],
      );
      const activePath = orderedTabs.find(
        (tab) => tab.id === useNotebookTabsStore.getState().activeTabId,
      )?.path;
      if (orderedPaths.length > 0 && activePath) {
        setLocation(
          notebookRouteForPaths(
            orderedPaths,
            activePath,
            pinnedPathsFromTabs(orderedTabs),
          ),
        );
      }
    },
    [setLocation, setPinned],
  );

  const getKernelStats = useCallback(
    async (tabId: string) => {
      const entry = tabEntries.find((candidate) => candidate.id === tabId);
      if (!entry) return null;
      try {
        return await entry.notebook.refreshKernelSlotInfo();
      } catch {
        return null;
      }
    },
    [tabEntries],
  );

  const reorderTab = useCallback(
    (tabId: string, toIndex: number) => {
      moveTab(tabId, toIndex);
      const orderedTabs = useNotebookTabsStore.getState().tabs;
      const orderedPaths = orderedTabs.flatMap((tab) =>
        tab.path ? [tab.path] : [],
      );
      const activePath = orderedTabs.find(
        (tab) => tab.id === activeTabId,
      )?.path;
      if (orderedPaths.length > 0 && activePath) {
        setLocation(
          notebookRouteForPaths(
            orderedPaths,
            activePath,
            pinnedPathsFromTabs(orderedTabs),
          ),
        );
      }
    },
    [activeTabId, moveTab, setLocation],
  );

  useEffect(() => {
    const unlisten = tabEntries.map((entry) =>
      listenForNotebookEvents(entry.notebook),
    );
    return () => {
      unlisten.forEach((cleanup) => cleanup());
    };
  }, [tabEntries]);

  useEffect(() => {
    return listenForRecentNotebookChanges((entries) => {
      const current = entries.find((entry) => entry.isCurrent);
      if (!current?.path) return;
      addOrFocusNotebookPath(current.path);
    });
  }, [addOrFocusNotebookPath]);

  useEffect(() => {
    setActiveAgentNotebook(undefined);

    void Promise.all(
      tabEntries.map(async (entry) => {
        const sourceKey = tabEntryLoadSourceKey(entry);
        if (!sourceKey) return;
        if (
          loadedSourceByNotebookRef.current.get(entry.notebook) === sourceKey
        ) {
          return;
        }
        loadedSourceByNotebookRef.current.set(entry.notebook, sourceKey);

        if (entry.path) {
          await entry.notebook.loadNotebookFromPath(entry.path);
        } else if (entry.inline) {
          entry.notebook.loadNotebook(JSON.parse(entry.inline));
        }
      }),
    );

    return () => {
      setActiveAgentNotebook(undefined);
    };
  }, [tabEntries]);

  useEffect(() => {
    if (tabSpecs.some((spec) => spec.path || spec.inline)) return;
    if (initialScratchRequestedRef.current) return;

    initialScratchRequestedRef.current = true;
    void addTab();
  }, [addTab, tabSpecs]);

  useEffect(() => {
    if (!activeEntry) return;
    setActiveAgentNotebook(activeEntry.notebook, activeEntry.id);
    void daemonControl({
      command: "set_focus",
      notebook_id: activeEntry.id,
    });
  }, [activeEntry]);

  useEffect(() => {
    if (pendingCloseIds === null) return;
    if (confirmCloseNeeded) return;
    const ids = pendingCloseIds;
    setPendingCloseIds(null);
    void closeTabsInOrder(ids, closeTab);
  }, [closeTab, confirmCloseNeeded, pendingCloseIds]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Tab" && event.ctrlKey) {
        event.preventDefault();
        const next = cycleTabId(tabs, activeTabId, event.shiftKey ? -1 : 1);
        if (next) setActiveTabId(next);
        return;
      }

      if (!event.metaKey) return;
      const key = event.key.toLowerCase();
      if (key === "t" && event.shiftKey) {
        event.preventDefault();
        void reopenClosedTab();
        return;
      }

      if (key === "t") {
        event.preventDefault();
        void addTab();
        return;
      }

      if (key === "w") {
        event.preventDefault();
        const tab = tabs.find((candidate) => candidate.id === activeTabId);
        if (tab && !tab.pinned) requestCloseTab(tab.id);
        return;
      }

      if (/^[1-9]$/.test(key)) {
        event.preventDefault();
        const target = jumpTabId(tabs, Number(key));
        if (target) setActiveTabId(target);
        return;
      }

      if (event.altKey && (key === "arrowleft" || key === "arrowright")) {
        event.preventDefault();
        const next = cycleTabId(
          tabs,
          activeTabId,
          key === "arrowleft" ? -1 : 1,
        );
        if (next) setActiveTabId(next);
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    activeTabId,
    addTab,
    requestCloseTab,
    reopenClosedTab,
    setActiveTabId,
    tabs,
  ]);

  return (
    <main className="h-screen bg-white">
      <NotebookTabStrip
        activeTabId={activeTabId}
        canReopen={canReopen}
        getKernelStats={getKernelStats}
        onCloseTab={requestCloseTab}
        onCloseOthers={closeOthers}
        onCloseRight={closeRight}
        onNewTab={addTab}
        onOpenNotebook={openNotebookFromPicker}
        onReopenClosed={reopenClosedTab}
        onReorder={reorderTab}
        onSwitchTab={setActiveTabId}
        onTogglePin={togglePin}
        tabs={tabs}
      />
      {tabError && (
        <div
          className="border-b border-red-200 bg-red-50 px-16 py-2 text-sm text-red-700"
          role="alert"
        >
          {tabError}
        </div>
      )}
      {tabEntries.map((entry) => (
        <NotebookTabPanel
          active={entry.id === activeEntry?.id}
          key={entry.id}
          notebook={entry.notebook}
          tabId={entry.id}
          updateTab={updateTab}
        />
      ))}
      <ConfirmModal
        body={closePromptBody(pendingCloseTargets, riskyPendingCloseCount)}
        confirmLabel={
          pendingCloseTargets.length > 1 ? "Close tabs" : "Close tab"
        }
        danger
        onCancel={() => setPendingCloseIds(null)}
        onConfirm={() => {
          const ids = pendingCloseIds;
          setPendingCloseIds(null);
          if (ids) void closeTabsInOrder(ids, closeTab);
        }}
        open={confirmCloseNeeded}
        title={closePromptTitle(pendingCloseTargets)}
      />
    </main>
  );
}

function NotebookTabPanel({
  active,
  notebook,
  tabId,
  updateTab,
}: {
  active: boolean;
  notebook: Notebook;
  tabId: string;
  updateTab: (tabId: string, patch: Partial<NotebookTab>) => void;
}) {
  const [path, viewMode, kernelId, kernelSpecName, editBuffer, dagStatus] =
    useStore(
      notebook.store,
      useShallow((state) => [
        state.viewState.path,
        state.viewState.viewMode,
        state.viewState.kernelId,
        state.viewState.kernelSpecName,
        state.editBuffer.cellSources,
        state.dagStatus,
      ]),
    );
  const appMode = viewMode === "app";

  useEffect(() => {
    updateTab(tabId, {
      path,
      title: titleFromPath(path),
      dirty: Object.keys(editBuffer).length > 0,
      kernelState: kernelStateFromNotebook(kernelId, dagStatus),
      language: kernelSpecName ?? "python3",
      mode: viewMode,
    });
  }, [
    dagStatus,
    editBuffer,
    kernelId,
    kernelSpecName,
    path,
    tabId,
    updateTab,
    viewMode,
  ]);

  return (
    <section
      aria-hidden={!active}
      className={active ? "contents" : "hidden"}
      data-notebook-tab-id={tabId}
    >
      <NotebookContext.Provider value={notebook}>
        <NotebookHeader kernelName="Local Kernel (Python 3.11.7)" />
        {!appMode && <HtmlScriptsNotice />}
        {appMode && <ScriptsDisabledBanner />}
        <NotebookView />
        {!appMode && <NotebookFooter />}
        {!appMode && <NotebookCommandMenu />}
        {/* Grant prompt is rendered last so it appears above everything. */}
        <AppGrantPromptContainer />
      </NotebookContext.Provider>
    </section>
  );
}

function tabsFromSearch(search: string): NotebookTabSpec[] {
  const params = new URLSearchParams(search);
  const paths = params.getAll("path");
  const pinnedPaths = new Set(pinnedPathsFromSearch(search));
  const inline = params.get("inline") ?? undefined;
  const tabs =
    paths.length > 0
      ? paths.map((path) => ({
          ...tabFromPath(path),
          pinned: pinnedPaths.has(path),
        }))
      : inline
        ? [tabFromInline(inline)]
        : [];

  return tabs.length > 0 ? pinnedFirst(tabs) : [tabFromPath(undefined)];
}

function tabFromPath(path: string | undefined): NotebookTabSpec {
  return {
    id: path ?? "untitled",
    path,
    title: titleFromPath(path),
    dirty: false,
    kernelState: "idle",
    language: "python3",
    mode: "cells",
  };
}

function tabFromInline(inline: string): NotebookTabSpec {
  return {
    ...tabFromPath(undefined),
    id: "inline",
    inline,
  };
}

function titleFromPath(path: string | undefined): string {
  if (!path) return "Untitled";
  const idx = path.lastIndexOf("/");
  return path.slice(idx + 1) || path;
}

function tabEntryFromSpec(tab: NotebookTabSpec): NotebookTabEntry {
  return {
    ...tab,
    notebook: new Notebook(),
  };
}

function reconcileTabEntriesFromSpecs(
  entries: readonly NotebookTabEntry[],
  specs: readonly NotebookTabSpec[],
): NotebookTabEntry[] {
  const claimed = new Set<NotebookTabEntry>();

  return specs.map((spec) => {
    const existing = entries.find(
      (entry) => !claimed.has(entry) && tabEntryMatchesSpec(entry, spec),
    );
    if (!existing) return tabEntryFromSpec(spec);

    claimed.add(existing);
    return {
      ...spec,
      ...existing,
      id: spec.id,
      path: spec.path,
      inline: spec.inline,
      pinned: spec.pinned,
      notebook: existing.notebook,
    };
  });
}

function pinnedFirst(tabs: readonly NotebookTabSpec[]): NotebookTabSpec[] {
  return [...tabs].sort(
    (a, b) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)),
  );
}

function pinnedPathsFromTabs(tabs: readonly NotebookTab[]): string[] {
  return tabs.flatMap((tab) => (tab.pinned && tab.path ? [tab.path] : []));
}

function tabEntryMatchesSpec(
  entry: NotebookTabEntry,
  spec: NotebookTabSpec,
): boolean {
  if (entry.id === spec.id) return true;
  if (entry.path && spec.path && entry.path === spec.path) return true;
  if (entry.inline && spec.inline && entry.inline === spec.inline) return true;
  return false;
}

function tabEntryLoadSourceKey(entry: NotebookTabEntry): string | undefined {
  if (entry.path) return `path:${entry.path}`;
  if (entry.inline) return `inline:${entry.inline}`;
  return undefined;
}

function isBlankPlaceholderTab(tab: NotebookTabEntry): boolean {
  return (
    tab.id === "untitled" && tab.path === undefined && tab.inline === undefined
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function tabFromEntry(entry: NotebookTabEntry): NotebookTab {
  const { inline: _inline, notebook: _notebook, ...tab } = entry;
  return tab;
}

function tabRequiresCloseConfirmation(
  tab: NotebookTab | undefined,
  entry: NotebookTabEntry | undefined,
): boolean {
  if (tab?.dirty || tab?.kernelState === "running") return true;
  if (!entry) return false;

  const state = entry.notebook.store.getState();
  return (
    Object.keys(state.editBuffer.cellSources).length > 0 ||
    Object.values(state.dagStatus).some((node) => node.state === "running")
  );
}

async function closeTabsInOrder(
  ids: readonly string[],
  closeTab: (tabId: string) => Promise<void>,
): Promise<void> {
  for (const id of ids) {
    await closeTab(id);
  }
}

function closePromptTitle(targets: readonly PendingCloseTarget[]): string {
  if (targets.length > 1) return `Close ${targets.length} tabs?`;
  const target = targets[0];
  return `Close ${target?.tab?.title ?? target?.entry?.title ?? "tab"}?`;
}

function closePromptBody(
  targets: readonly PendingCloseTarget[],
  riskyCount: number,
): string {
  if (targets.length > 1) {
    return `${riskyCount} of these tabs have unsaved changes or running kernels. Closing tears down their kernel slots.`;
  }

  const target = targets[0];
  return closeSinglePromptBody(target?.tab, target?.entry);
}

function closeSinglePromptBody(
  tab: NotebookTab | undefined,
  entry: NotebookTabEntry | undefined,
): string {
  if (!tab && !entry) return "";
  const dirty =
    Boolean(tab?.dirty) ||
    (entry
      ? Object.keys(entry.notebook.store.getState().editBuffer.cellSources)
          .length > 0
      : false);
  const running =
    tab?.kernelState === "running" ||
    (entry
      ? Object.values(entry.notebook.store.getState().dagStatus).some(
          (node) => node.state === "running",
        )
      : false);

  if (dirty && running) {
    return "This tab has unsaved changes and a running kernel. Closing it will tear down only this tab's kernel slot.";
  }
  if (dirty) {
    return "This tab has unsaved changes. Closing it will tear down only this tab's kernel slot.";
  }
  return "This tab has a running kernel. Closing it will tear down only this tab's kernel slot.";
}

function kernelStateFromNotebook(
  kernelId: string | undefined,
  dagStatus: Record<string, { state: string }>,
): NotebookTabKernelState {
  if (Object.values(dagStatus).some((node) => node.state === "running")) {
    return "running";
  }
  return kernelId ? "live" : "idle";
}
