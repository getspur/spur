import {
  useMarkdownMermaidEnabled,
  useOutputActiveContentEnabled,
  useSettings,
} from "@/stores/settings";

export default function SettingsPanel() {
  const mermaidEnabled = useMarkdownMermaidEnabled();
  const activeContentEnabled = useOutputActiveContentEnabled();
  const setMarkdownMermaid = useSettings((state) => state.setMarkdownMermaid);
  const setOutputActiveContent = useSettings(
    (state) => state.setOutputActiveContent,
  );
  const appGrants = useSettings((state) => state.appGrants);
  const revokeAppGrant = useSettings((state) => state.revokeAppGrant);

  const grantEntries = Object.entries(appGrants);

  return (
    <div className="absolute right-0 top-full z-20 mt-2 w-80 rounded border border-gray-200 bg-white p-3 text-xs text-gray-700 shadow-lg">
      <div className="space-y-3">
        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            className="mt-0.5"
            checked={mermaidEnabled}
            onChange={(event) =>
              setMarkdownMermaid(event.currentTarget.checked)
            }
          />
          <span>
            <span className="block font-medium text-gray-900">
              Render Mermaid diagrams in markdown
            </span>
            <span className="block text-gray-500">
              Lazy-loads ~1MB on first use.
            </span>
          </span>
        </label>

        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            className="mt-0.5"
            checked={activeContentEnabled}
            onChange={(event) =>
              setOutputActiveContent(event.currentTarget.checked)
            }
          />
          <span>
            <span className="block font-medium text-gray-900">
              Allow active content in HTML outputs
            </span>
            <span className="block text-gray-500">
              Lets scripts run inside the sandboxed iframe. Enable only for
              notebooks you trust.
            </span>
          </span>
        </label>

        {grantEntries.length > 0 && (
          <div>
            <span className="block font-medium text-gray-900">
              App script grants
            </span>
            <span className="block text-gray-500">
              Per-app trust decisions. Revoking re-shows the prompt on next
              open.
            </span>
            <ul className="mt-1.5 space-y-1">
              {grantEntries.map(([appRoot, grant]) => (
                <li
                  key={appRoot}
                  className="flex items-center justify-between gap-2 rounded border border-gray-100 bg-gray-50 px-2 py-1"
                >
                  <span
                    className="min-w-0 flex-1 truncate text-gray-700"
                    title={appRoot}
                  >
                    {appRoot.split("/").pop() ?? appRoot}
                    {" "}
                    <span
                      className={
                        grant.activeOutputScripts
                          ? "text-green-700"
                          : "text-red-700"
                      }
                    >
                      ({grant.activeOutputScripts ? "allowed" : "denied"})
                    </span>
                  </span>
                  <button
                    className="shrink-0 text-gray-500 hover:text-red-600"
                    onClick={() => revokeAppGrant(appRoot)}
                    title={`Revoke grant for ${appRoot}`}
                    type="button"
                    aria-label={`Revoke grant for ${appRoot}`}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
