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
      </div>
    </div>
  );
}
