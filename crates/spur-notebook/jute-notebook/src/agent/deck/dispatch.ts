import { invoke } from "@tauri-apps/api/core";

import type { Notebook } from "@/stores/notebook";

import { PROMPTS } from "./prompts";

type Kind = keyof typeof PROMPTS;

export async function dispatchDeckCommand(
  notebook: Notebook,
  kind: Kind,
  userPrompt: string,
): Promise<{ delegation_id: string }> {
  const summary = summarizeNotebook(notebook);
  const task = `${PROMPTS[kind]}\n\nUser's request: ${userPrompt}\n\nCurrent notebook (${summary.cells.length} cells):\n${JSON.stringify(summary, null, 2)}`;

  return await invoke<{ delegation_id: string }>("spur_delegate_to_worker", {
    task,
    workerType: "coder",
    toolAllowlist: ["mcp__notebook__*"],
  });
}

function summarizeNotebook(notebook: Notebook) {
  const state = notebook.store.getState();
  const cellIds = state.cellIds ?? Object.keys(state.cells);
  const cells = cellIds.map((id: string) => {
    const cell = state.cells[id];
    const source = Array.isArray(cell.source)
      ? cell.source.join("")
      : (cell.source ?? "");
    return {
      id,
      type: cell.type,
      layout: cell.juteDeckMetadata?.layout ?? "auto",
      preview: source.slice(0, 80),
    };
  });
  return { path: state.path, cells };
}
