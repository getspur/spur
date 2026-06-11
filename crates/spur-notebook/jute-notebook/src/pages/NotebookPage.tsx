import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSearch } from "wouter";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { setActiveAgentNotebook } from "@/agent/bridge";
import {
  listenForNotebookEvents,
  listenForRecentNotebookChanges,
} from "@/agent/events";
import { daemonControl } from "@/daemon/control";
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

type NotebookTabSpec = NotebookTab & {
  inline?: string;
};

type NotebookTabEntry = NotebookTabSpec & {
  notebook: Notebook;
};

export default function NotebookPage() {
  const search = useSearch();
  const tabSpecs = useMemo(() => tabsFromSearch(search), [search]);
  const syncedSearchRef = useRef(search);
  const [tabEntries, setTabEntries] = useState<NotebookTabEntry[]>(() =>
    tabSpecs.map(tabEntryFromSpec),
  );
  const [pendingCloseTabId, setPendingCloseTabId] = useState<string | null>(
    null,
  );
  const tabs = useNotebookTabsStore((state) => state.tabs);
  const activeTabId = useNotebookTabsStore((state) => state.activeTabId);
  const setTabs = useNotebookTabsStore((state) => state.setTabs);
  const setActiveTabId = useNotebookTabsStore((state) => state.setActiveTabId);
  const updateTab = useNotebookTabsStore((state) => state.updateTab);
  const activeEntry =
    tabEntries.find((entry) => entry.id === activeTabId) ?? tabEntries[0];
  const activeTabIndex = tabs.findIndex((tab) => tab.id === activeTabId);
  const pendingCloseTab =
    pendingCloseTabId === null
      ? undefined
      : tabs.find((tab) => tab.id === pendingCloseTabId);
  const pendingCloseEntry =
    pendingCloseTabId === null
      ? undefined
      : tabEntries.find((entry) => entry.id === pendingCloseTabId);
  const confirmCloseNeeded =
    pendingCloseTabId !== null &&
    tabRequiresCloseConfirmation(pendingCloseTab, pendingCloseEntry);

  useEffect(() => {
    if (syncedSearchRef.current === search) return;
    syncedSearchRef.current = search;
    setTabEntries(tabSpecs.map(tabEntryFromSpec));
  }, [search, tabSpecs]);

  useEffect(() => {
    setTabs(tabEntries.map(tabFromEntry));
  }, [setTabs, tabEntries]);

  const requestCloseTab = useCallback((tabId: string) => {
    setPendingCloseTabId(tabId);
  }, []);

  const addTab = useCallback(() => {
    const id = `untitled-${Date.now()}`;
    const entry = tabEntryFromSpec({
      id,
      title: "Untitled",
      dirty: false,
      kernelState: "idle",
      language: "python3",
      mode: "cells",
    });
    setTabEntries((entries) => [...entries, entry]);
    setTabs([
      ...tabs,
      {
        id: entry.id,
        path: entry.path,
        title: entry.title,
        dirty: entry.dirty,
        kernelState: entry.kernelState,
        language: entry.language,
        mode: entry.mode,
      },
    ]);
    setActiveTabId(id);
  }, [setActiveTabId, setTabs, tabs]);

  const closeTab = useCallback(
    async (tabId: string) => {
      const tab = tabs.find((candidate) => candidate.id === tabId);
      if (!tab) return;

      await daemonControl({
        command: "set_focus",
        notebook_id: tab.id,
      });
      await daemonControl({ command: "close" });

      setTabEntries((entries) => entries.filter((entry) => entry.id !== tabId));
      const remainingTabs = tabs.filter((candidate) => candidate.id !== tabId);
      setTabs(remainingTabs);

      if (activeTabId === tabId) {
        const removedIndex = tabs.findIndex(
          (candidate) => candidate.id === tabId,
        );
        const nextTab =
          remainingTabs[Math.min(removedIndex, remainingTabs.length - 1)] ??
          remainingTabs[0];
        setActiveTabId(nextTab?.id);
      } else if (activeTabId) {
        await daemonControl({
          command: "set_focus",
          notebook_id: activeTabId,
        });
      }
    },
    [activeTabId, setActiveTabId, setTabs, tabs],
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

      const existingEntry = tabEntries.find(
        (entry) => entry.path === current.path || entry.id === current.path,
      );
      if (existingEntry) {
        setTabs(tabEntries.map(tabFromEntry));
        setActiveTabId(existingEntry.id);
        return;
      }

      const entry = tabEntryFromSpec(tabFromPath(current.path));
      setTabEntries((entries) => {
        if (entries.some((entry) => entry.path === current.path)) {
          return entries;
        }
        return [...entries, entry];
      });
      setTabs([...tabs, tabFromEntry(entry)]);
      setActiveTabId(entry.id);
    });
  }, [setActiveTabId, setTabs, tabEntries, tabs]);

  useEffect(() => {
    setActiveAgentNotebook(undefined);

    void Promise.all(
      tabEntries.map(async (entry) => {
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
    if (!activeEntry) return;
    setActiveAgentNotebook(activeEntry.notebook, activeEntry.id);
    void daemonControl({
      command: "set_focus",
      notebook_id: activeEntry.id,
    });
  }, [activeEntry]);

  useEffect(() => {
    if (pendingCloseTabId === null) return;
    if (confirmCloseNeeded) return;
    setPendingCloseTabId(null);
    void closeTab(pendingCloseTabId);
  }, [closeTab, confirmCloseNeeded, pendingCloseTabId]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.metaKey) return;

      const key = event.key.toLowerCase();
      if (key === "t") {
        event.preventDefault();
        addTab();
        return;
      }

      if (key === "w") {
        event.preventDefault();
        if (activeTabId) requestCloseTab(activeTabId);
        return;
      }

      if (/^[1-9]$/.test(key)) {
        const tab = tabs[Number(key) - 1];
        if (tab) {
          event.preventDefault();
          setActiveTabId(tab.id);
        }
        return;
      }

      if (!event.altKey || tabs.length === 0) return;
      if (key === "arrowleft" || key === "arrowright") {
        event.preventDefault();
        const currentIndex = activeTabIndex >= 0 ? activeTabIndex : 0;
        const offset = key === "arrowleft" ? -1 : 1;
        const nextIndex = (currentIndex + offset + tabs.length) % tabs.length;
        setActiveTabId(tabs[nextIndex]?.id);
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    activeTabId,
    activeTabIndex,
    addTab,
    requestCloseTab,
    setActiveTabId,
    tabs,
  ]);

  return (
    <main className="h-screen bg-white">
      <NotebookTabStrip
        activeTabId={activeTabId}
        onCloseTab={requestCloseTab}
        onNewTab={addTab}
        onSwitchTab={setActiveTabId}
        tabs={tabs}
      />
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
        body={closePromptBody(pendingCloseTab, pendingCloseEntry)}
        confirmLabel="Close tab"
        danger
        onCancel={() => setPendingCloseTabId(null)}
        onConfirm={() => {
          const tabId = pendingCloseTabId;
          setPendingCloseTabId(null);
          if (tabId) void closeTab(tabId);
        }}
        open={confirmCloseNeeded}
        title={`Close ${
          pendingCloseTab?.title ?? pendingCloseEntry?.title ?? "tab"
        }?`}
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
  const inline = params.get("inline") ?? undefined;
  const tabs =
    paths.length > 0
      ? paths.map((path) => tabFromPath(path))
      : inline
        ? [tabFromInline(inline)]
        : [];

  return tabs.length > 0 ? tabs : [tabFromPath(undefined)];
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

function closePromptBody(
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
