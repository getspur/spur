import { useEffect, useMemo } from "react";
import { useSearch } from "wouter";

import { setActiveAgentNotebook } from "@/agent/bridge";
import { listenForNotebookEvents } from "@/agent/events";
import { Notebook, NotebookContext } from "@/stores/notebook";
import HtmlScriptsNotice from "@/ui/notebook/HtmlScriptsNotice";
import NotebookCommandMenu from "@/ui/notebook/NotebookCommandMenu";
import NotebookFooter from "@/ui/notebook/NotebookFooter";
import NotebookHeader from "@/ui/notebook/NotebookHeader";
import NotebookView from "@/ui/notebook/NotebookView";

export default function NotebookPage() {
  const { path, inline } = Object.fromEntries(new URLSearchParams(useSearch()));

  // Singleton notebook object used for the lifetime of this component.
  const notebook = useMemo(() => new Notebook(), []);

  useEffect(() => listenForNotebookEvents(notebook), [notebook]);

  useEffect(() => {
    let cancelled = false;
    setActiveAgentNotebook(undefined);

    async function loadNotebook() {
      if (path) {
        await notebook.loadNotebookFromPath(path);
      } else if (inline) {
        notebook.loadNotebook(JSON.parse(inline));
      } else {
        return;
      }

      if (!cancelled && !notebook.state.loadError) {
        setActiveAgentNotebook(notebook);
      }
    }

    void loadNotebook();

    return () => {
      cancelled = true;
      setActiveAgentNotebook(undefined);
    };
  }, [notebook, path, inline]);

  return (
    <main className="h-screen bg-white">
      <NotebookContext.Provider value={notebook}>
        <NotebookHeader kernelName="Local Kernel (Python 3.11.7)" />
        <HtmlScriptsNotice />
        <NotebookView />
        <NotebookFooter />
        <NotebookCommandMenu />
      </NotebookContext.Provider>
    </main>
  );
}
