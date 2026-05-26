import { XIcon } from "lucide-react";

import {
  useHtmlScriptsNoticeVisible,
  useSettings,
} from "@/stores/settings";

export default function HtmlScriptsNotice() {
  const visible = useHtmlScriptsNoticeVisible();
  const dismiss = useSettings((state) => state.dismissHtmlScriptsNotice);
  const setOutputActiveContent = useSettings(
    (state) => state.setOutputActiveContent,
  );

  if (!visible) return null;

  const enableAndDismiss = () => {
    setOutputActiveContent(true);
    dismiss();
  };

  return (
    <div className="flex items-start gap-3 border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-900">
      <span className="flex-1">
        <span className="font-medium">
          HTML output scripts are off by default.
        </span>{" "}
        Interactive outputs (Plotly, Bokeh, ipywidgets, holoviews, folium) will
        not run scripts until you enable active content.
      </span>
      <button
        className="rounded border border-amber-300 bg-white px-2 py-0.5 font-medium text-amber-900 hover:bg-amber-100"
        onClick={enableAndDismiss}
      >
        Enable now
      </button>
      <button
        className="rounded p-0.5 text-amber-700 hover:bg-amber-100"
        title="Dismiss"
        onClick={dismiss}
      >
        <XIcon size={14} />
      </button>
    </div>
  );
}
