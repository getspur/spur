import type { RecentNotebookEntry } from "@/bindings";
import type { Notebook } from "@/stores/notebook";

import {
  listenForNotebookEvents,
  listenForRecentNotebookChanges,
} from "./events";

type NotebookEventsListener = (notebook: Notebook) => () => void;
type RecentNotebookChangesListener = (
  applyRecents: (entries: RecentNotebookEntry[]) => void | Promise<void>,
) => () => void;

const notebookEventsListener: NotebookEventsListener = listenForNotebookEvents;
const recentNotebookChangesListener: RecentNotebookChangesListener =
  listenForRecentNotebookChanges;

void notebookEventsListener;
void recentNotebookChangesListener;
