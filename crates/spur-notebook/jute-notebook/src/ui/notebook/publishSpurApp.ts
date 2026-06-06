import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

type PublishableNotebook = {
  saveNow: () => Promise<void>;
};

export type PublishSpurAppResponse = {
  path: string;
  manifest: unknown;
  assetCount: number;
  preflight: unknown;
};

export function defaultSpurAppPath(notebookPath: string): string {
  return notebookPath.replace(/(?:\.ipynb)?$/i, ".spurapp");
}

export function defaultSpurAppName(notebookPath: string): string {
  const file = notebookPath.split(/[\\/]/).pop() ?? "Spur App";
  return file.replace(/\.ipynb$/i, "") || "Spur App";
}

export async function publishSpurApp(
  notebook: PublishableNotebook,
  notebookPath: string,
): Promise<PublishSpurAppResponse | null> {
  await notebook.saveNow();

  const outputPath = await save({
    title: "Publish Spur App",
    defaultPath: defaultSpurAppPath(notebookPath),
    filters: [{ name: "Spur App", extensions: ["spurapp"] }],
  });
  if (!outputPath) return null;

  return await invoke<PublishSpurAppResponse>("publish_spur_app", {
    notebookPath,
    outputPath,
    name: defaultSpurAppName(notebookPath),
    includePortSnapshots: false,
  });
}
