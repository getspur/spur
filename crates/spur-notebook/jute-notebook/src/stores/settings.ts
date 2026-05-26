import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

export const SETTINGS_STORAGE_KEY = "jute-notebook:settings:v1";

export type SettingsState = {
  markdown: {
    mermaid: boolean;
  };
  output: {
    activeContent: boolean;
  };
};

type SettingsActions = {
  setMarkdownMermaid: (enabled: boolean) => void;
  setOutputActiveContent: (enabled: boolean) => void;
  reset: () => void;
};

type SettingsStore = SettingsState & SettingsActions;

export const DEFAULT_SETTINGS: SettingsState = {
  markdown: { mermaid: false },
  output: { activeContent: false },
};

function defaultSettings(): SettingsState {
  return {
    markdown: { ...DEFAULT_SETTINGS.markdown },
    output: { ...DEFAULT_SETTINGS.output },
  };
}

export const useSettings = create<SettingsStore>()(
  persist(
    (set) => ({
      ...defaultSettings(),

      setMarkdownMermaid: (mermaid) =>
        set((state) => ({
          markdown: { ...state.markdown, mermaid },
        })),

      setOutputActiveContent: (activeContent) =>
        set((state) => ({
          output: { ...state.output, activeContent },
        })),

      reset: () => set(defaultSettings()),
    }),
    {
      name: SETTINGS_STORAGE_KEY,
      storage: createJSONStorage(localStorageForSettings),
      partialize: (state) => ({
        markdown: state.markdown,
        output: state.output,
      }),
      merge: (persisted, current) => {
        const persistedSettings = persisted as Partial<SettingsState> | null;
        return {
          ...current,
          markdown: {
            ...current.markdown,
            ...persistedSettings?.markdown,
          },
          output: {
            ...current.output,
            ...persistedSettings?.output,
          },
        };
      },
    },
  ),
);

export function useMarkdownMermaidEnabled(): boolean {
  return useSettings((state) => state.markdown.mermaid);
}

export function useOutputActiveContentEnabled(): boolean {
  return useSettings((state) => state.output.activeContent);
}

function localStorageForSettings(): Storage {
  const storage = globalThis.localStorage;
  if (
    typeof storage?.getItem !== "function" ||
    typeof storage.setItem !== "function" ||
    typeof storage.removeItem !== "function"
  ) {
    throw new Error("localStorage is unavailable");
  }
  return storage;
}
