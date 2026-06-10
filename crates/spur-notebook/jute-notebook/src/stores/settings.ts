import { create, useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

export const SETTINGS_STORAGE_KEY = "jute-notebook:settings:v1";

/** A persisted trust grant for one app root directory. */
export type AppGrant = {
  /** Whether the user allowed active output scripts for this app. */
  activeOutputScripts: boolean;
  /** ISO-8601 timestamp of when the grant was recorded. */
  grantedAt: string;
};

export type SettingsState = {
  markdown: {
    mermaid: boolean;
  };
  output: {
    activeContent: boolean;
  };
  notices: {
    htmlScriptsDismissed: boolean;
  };
  /** Per-app trust grants keyed by app root directory path. */
  appGrants: Record<string, AppGrant>;
};

type SettingsActions = {
  setMarkdownMermaid: (enabled: boolean) => void;
  setOutputActiveContent: (enabled: boolean) => void;
  dismissHtmlScriptsNotice: () => void;
  /**
   * Record a trust grant for the given app root.
   * `granted = true` → allow active output scripts.
   * `granted = false` → deny (open with scripts off, show banner).
   */
  setAppGrant: (appRoot: string, granted: boolean) => void;
  /** Remove the stored grant for an app root so the prompt appears again. */
  revokeAppGrant: (appRoot: string) => void;
  reset: () => void;
};

type SettingsStore = SettingsState & SettingsActions;

export const DEFAULT_SETTINGS: SettingsState = {
  markdown: { mermaid: false },
  output: { activeContent: false },
  notices: { htmlScriptsDismissed: false },
  appGrants: {},
};

function defaultSettings(): SettingsState {
  return {
    markdown: { ...DEFAULT_SETTINGS.markdown },
    output: { ...DEFAULT_SETTINGS.output },
    notices: { ...DEFAULT_SETTINGS.notices },
    appGrants: {},
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

      dismissHtmlScriptsNotice: () =>
        set((state) => ({
          notices: { ...state.notices, htmlScriptsDismissed: true },
        })),

      setAppGrant: (appRoot, granted) =>
        set((state) => ({
          appGrants: {
            ...state.appGrants,
            [appRoot]: {
              activeOutputScripts: granted,
              grantedAt: new Date().toISOString(),
            },
          },
        })),

      revokeAppGrant: (appRoot) =>
        set((state) => {
          const next = { ...state.appGrants };
          delete next[appRoot];
          return { appGrants: next };
        }),

      reset: () => set(defaultSettings()),
    }),
    {
      name: SETTINGS_STORAGE_KEY,
      storage: createJSONStorage(localStorageForSettings),
      partialize: (state) => ({
        markdown: state.markdown,
        output: state.output,
        notices: state.notices,
        appGrants: state.appGrants,
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
          notices: {
            ...current.notices,
            ...persistedSettings?.notices,
          },
          appGrants: {
            ...current.appGrants,
            ...persistedSettings?.appGrants,
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

export function useHtmlScriptsNoticeVisible(): boolean {
  return useSettings(
    (state) =>
      !state.notices.htmlScriptsDismissed && !state.output.activeContent,
  );
}

/**
 * Resolve the effective active-content flag for a specific app root.
 *
 * If a per-app grant exists, it takes precedence over the global toggle.
 * Falls back to `output.activeContent` when:
 * - `appRoot` is undefined (not in app mode or app root unknown), or
 * - no grant has been recorded for this root yet.
 *
 * Can be called outside of a React render context (e.g. from tests).
 */
export function useEffectiveActiveContent(
  appRoot: string | undefined,
): boolean {
  const state = useSettings.getState();
  if (appRoot !== undefined) {
    const grant = state.appGrants[appRoot];
    if (grant !== undefined) {
      return grant.activeOutputScripts;
    }
  }
  return state.output.activeContent;
}

/**
 * React hook variant of `useEffectiveActiveContent` — subscribes to store
 * updates so the component re-renders when the grant or global toggle changes.
 */
export function useEffectiveActiveContentReactive(
  appRoot: string | undefined,
): boolean {
  return useStore(useSettings, (state) => {
    if (appRoot !== undefined) {
      const grant = state.appGrants[appRoot];
      if (grant !== undefined) {
        return grant.activeOutputScripts;
      }
    }
    return state.output.activeContent;
  });
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
