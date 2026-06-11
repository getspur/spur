import { useEffect, useMemo } from "react";
import { useSearch } from "wouter";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { setActiveAgentNotebook } from "@/agent/bridge";
import { listenForNotebookEvents } from "@/agent/events";
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
import NotebookTabsBasicControls from "@/ui/notebook/NotebookTabsBasicControls";
import NotebookView from "@/ui/notebook/NotebookView";

type NotebookTabSpec = NotebookTab & {
  inline?: string;
};

type NotebookTabEntry = NotebookTabSpec & {
  notebook: Notebook;
};

export default function NotebookPage() {
  const search = useSearch();
  const tabSpecs = useMemo(() => tabsFromSearch(search), [search]);
  const tabEntries = useMemo<NotebookTabEntry[]>(
    () =>
      tabSpecs.map((tab) => ({
        ...tab,
        notebook: new Notebook(),
      })),
    [tabSpecs],
  );
  const tabs = useNotebookTabsStore((state) => state.tabs);
  const activeTabId = useNotebookTabsStore((state) => state.activeTabId);
  const setTabs = useNotebookTabsStore((state) => state.setTabs);
  const setActiveTabId = useNotebookTabsStore((state) => state.setActiveTabId);
  const updateTab = useNotebookTabsStore((state) => state.updateTab);
  const activeEntry =
    tabEntries.find((entry) => entry.id === activeTabId) ?? tabEntries[0];

  useEffect(() => {
    setTabs(
      tabEntries.map(({ inline: _inline, notebook: _notebook, ...tab }) => tab),
    );
  }, [setTabs, tabEntries]);

  useEffect(() => {
    const unlisten = tabEntries.map((entry) =>
      listenForNotebookEvents(entry.notebook),
    );
    return () => {
      unlisten.forEach((cleanup) => cleanup());
    };
  }, [tabEntries]);

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
    setActiveAgentNotebook(activeEntry.notebook);
    void daemonControl({
      command: "set_focus",
      notebook_id: activeEntry.id,
    });
  }, [activeEntry]);

  return (
    <main className="h-screen bg-white">
      <NotebookTabsBasicControls
        activeTabId={activeTabId}
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
  const [path, viewMode, kernelId, editBuffer, dagStatus] = useStore(
    notebook.store,
    useShallow((state) => [
      state.viewState.path,
      state.viewState.viewMode,
      state.viewState.kernelId,
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
      mode: viewMode,
    });
  }, [dagStatus, editBuffer, kernelId, path, tabId, updateTab, viewMode]);

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

function kernelStateFromNotebook(
  kernelId: string | undefined,
  dagStatus: Record<string, { state: string }>,
): NotebookTabKernelState {
  if (Object.values(dagStatus).some((node) => node.state === "running")) {
    return "running";
  }
  return kernelId ? "live" : "idle";
}
