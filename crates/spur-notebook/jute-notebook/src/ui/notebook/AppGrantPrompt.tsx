/**
 * Modal that appears when a notebook opens in app mode AND its manifest
 * declares `active_output_scripts` AND no stored grant exists for the app
 * root.  The user can Allow or Deny.  Deny shows a dismissible banner instead
 * of silently defaulting.
 */

import { useEffect } from "react";
import { useStore } from "zustand";

import { type NotebookOpenInfo, useNotebook } from "@/stores/notebook";
import { useSettings } from "@/stores/settings";

type AppGrantPromptProps = {
  openInfo: NotebookOpenInfo;
  onResolved: () => void;
};

/** Listed capability labels shown in the grant prompt body. */
function capabilityLabels(
  capabilities: NotebookOpenInfo["capabilities"],
): string[] {
  const labels: string[] = [];
  if (capabilities.active_output_scripts)
    labels.push("Run scripts in output iframes");
  if (capabilities.canvas_capture) labels.push("Record canvas captures");
  if (capabilities.artifacts_dir)
    labels.push("Write to a host-managed artifacts directory");
  if (capabilities.ports) labels.push("Read/write notebook port store");
  return labels;
}

export default function AppGrantPrompt({
  openInfo,
  onResolved,
}: AppGrantPromptProps) {
  const setAppGrant = useSettings((state) => state.setAppGrant);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        handleDeny();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function handleAllow() {
    setAppGrant(openInfo.app_root, true);
    onResolved();
  }

  function handleDeny() {
    setAppGrant(openInfo.app_root, false);
    onResolved();
  }

  const labels = capabilityLabels(openInfo.capabilities);

  return (
    <div
      aria-modal="true"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/30"
      role="dialog"
      aria-label={`Trust grant for ${openInfo.app_name}`}
    >
      <div
        className="w-full max-w-md rounded border border-gray-300 bg-white p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-base font-semibold text-gray-950">
          Allow active scripts for{" "}
          <span className="font-bold">{openInfo.app_name}</span>?
        </h2>
        <p className="mt-2 text-sm text-gray-600">
          This app requests the following capabilities:
        </p>
        {labels.length > 0 && (
          <ul className="mt-2 list-inside list-disc text-sm text-gray-700">
            {labels.map((label) => (
              <li key={label}>{label}</li>
            ))}
          </ul>
        )}
        <p className="mt-3 text-xs text-gray-500">
          You can change this later in Settings. Denying keeps scripts disabled
          and shows a banner.
        </p>
        <div className="mt-5 flex items-center justify-end gap-2">
          <button
            className="rounded border border-gray-300 px-3 py-2 text-sm text-gray-600 transition-colors hover:border-black hover:text-gray-950"
            onClick={handleDeny}
            type="button"
          >
            Deny
          </button>
          <button
            autoFocus
            className="rounded bg-gray-950 px-3 py-2 text-sm text-white transition-colors hover:bg-black"
            onClick={handleAllow}
            type="button"
          >
            Allow
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Container that reads the notebook's appOpenInfo from the store and decides
 * whether to show the grant prompt.  Renders nothing when:
 * - the notebook is not in app mode,
 * - the manifest does not request `active_output_scripts`,
 * - a grant already exists (allow or deny) for the app root.
 */
export function AppGrantPromptContainer() {
  const notebook = useNotebook();
  const appOpenInfo = useStore(
    notebook.store,
    (state) => state.viewState.appOpenInfo,
  );
  const appGrants = useSettings((state) => state.appGrants);

  if (!appOpenInfo) return null;
  if (!appOpenInfo.capabilities.active_output_scripts) return null;

  const existingGrant = appGrants[appOpenInfo.app_root];
  if (existingGrant !== undefined) return null;

  // Grant is not yet set — show the prompt.  `onResolved` closes the modal
  // (the store update from setAppGrant triggers a re-render).
  return (
    <AppGrantPrompt openInfo={appOpenInfo} onResolved={() => undefined} />
  );
}

/**
 * A dismissible banner shown when the user denied active scripts for an app
 * (grant exists with `activeOutputScripts = false`).
 */
export function ScriptsDisabledBanner() {
  const notebook = useNotebook();
  const appOpenInfo = useStore(
    notebook.store,
    (state) => state.viewState.appOpenInfo,
  );
  const { appGrants, revokeAppGrant } = useSettings((state) => ({
    appGrants: state.appGrants,
    revokeAppGrant: state.revokeAppGrant,
  }));

  if (!appOpenInfo) return null;

  const grant = appGrants[appOpenInfo.app_root];
  if (!grant || grant.activeOutputScripts) return null;

  return (
    <div
      role="status"
      className="flex items-center justify-between gap-3 border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-800"
    >
      <span>
        Output scripts disabled for{" "}
        <strong>{appOpenInfo.app_name}</strong> — enable in Settings or{" "}
        <button
          className="underline hover:no-underline"
          onClick={() => revokeAppGrant(appOpenInfo.app_root)}
          type="button"
        >
          re-prompt
        </button>
        .
      </span>
    </div>
  );
}
