import clsx from "clsx";
import { encode } from "html-entities";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { MultilineString, OutputDisplayData } from "@/bindings";
import { CellResult } from "@/stores/notebook";
import { useOutputActiveContentEnabled } from "@/stores/settings";

import AfmView, {
  type AfmPortBindingSnapshot,
  WIDGET_VIEW_MIME,
  anywidgetViewFromData,
} from "./JuteAppOutput";
import MarkdownRenderer from "./MarkdownRenderer";
import { installAfmHostTransport } from "./afmHost";
import {
  compilePhasePresentation,
  compileProgressMessage,
  formatCompileElapsed,
} from "./compileProgress";
import { htmlOutputSandbox, withVideoCapture } from "./rendering";

type Props = {
  value: CellResult | undefined;
  cellId?: string;
  chromeless?: boolean;
  afmPortBindings?: AfmPortBindingSnapshot;
};

type CompileProgressState = NonNullable<CellResult["compile"]>;

export default function OutputView({
  value,
  cellId,
  chromeless = false,
  afmPortBindings,
}: Props) {
  const compile = value?.compile;
  const outputs = value?.outputs ?? [];
  const showCompileRail = Boolean(compile && outputs.length === 0);
  const now = useCompileNow(showCompileRail, compile?.startedAt);

  useEffect(() => installAfmHostTransport(), []);

  if (!value) {
    return null;
  }

  return (
    <div
      className={clsx(
        "select-text whitespace-pre-wrap break-words text-sm after:contents",
        !chromeless && "px-8 pb-6 pt-4",
      )}
    >
      {showCompileRail && compile ? (
        <CompileProgressRail compile={compile} now={now} />
      ) : null}
      {outputs.map((output, index) => (
        <div key={index}>
          {output.output_type === "stream" ? (
            <pre>{multiline(output.text)}</pre>
          ) : output.output_type === "display_data" ? (
            <OutputViewDisplayData
              cellId={cellId}
              output={output}
              chromeless={chromeless}
              afmPortBindings={afmPortBindings}
            />
          ) : output.output_type === "execute_result" ? (
            <OutputViewDisplayData
              cellId={cellId}
              output={output}
              chromeless={chromeless}
              afmPortBindings={afmPortBindings}
            />
          ) : output.output_type === "error" ? (
            // TODO: Display error tracebacks.
            <pre className="text-red-500">
              {output.ename}: {output.evalue}
            </pre>
          ) : null}
        </div>
      ))}
    </div>
  );
}

function useCompileNow(active: boolean, startedAt: number | undefined) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!active || startedAt === undefined) {
      return;
    }

    setNow(Date.now());
    const intervalId = window.setInterval(() => {
      setNow(Date.now());
    }, 1000);

    return () => window.clearInterval(intervalId);
  }, [active, startedAt]);

  return now;
}

function CompileProgressRail({
  compile,
  now,
}: {
  compile: CompileProgressState;
  now: number;
}) {
  const presentation = compilePhasePresentation(compile.phase);
  const elapsed = formatCompileElapsed(compile.startedAt, now);
  const message = compileProgressMessage(compile.phase, compile.current);

  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={`${message} ${elapsed}`}
      className={clsx(
        "mb-3 grid grid-cols-[auto_minmax(96px,1fr)_minmax(0,1.6fr)_auto] items-center gap-2 rounded border px-3 py-2",
        presentation.railClassName,
      )}
    >
      <span
        className={clsx(
          "whitespace-nowrap rounded border px-1.5 py-0.5 text-[11px] font-medium",
          presentation.chipClassName,
        )}
      >
        {presentation.label}
      </span>
      <span
        aria-hidden="true"
        className={clsx(
          "relative h-1.5 overflow-hidden rounded",
          presentation.trackClassName,
        )}
      >
        <span
          className={clsx(
            "jute-compile-progress-sweep absolute inset-y-0 left-0 w-1/2 rounded",
            presentation.sweepClassName,
          )}
        />
      </span>
      <span className={clsx("truncate text-xs", presentation.textClassName)}>
        {message}
      </span>
      <span
        className={clsx(
          "whitespace-nowrap font-mono text-[11px]",
          presentation.textClassName,
        )}
      >
        {elapsed}
      </span>
    </div>
  );
}

function multiline(source: MultilineString): string {
  if (typeof source === "string") {
    return source;
  }
  return source.join("");
}

const IMAGE_MIME_TYPES = [
  "image/png",
  "image/jpeg",
  "image/svg+xml",
  "image/bmp",
  "image/gif",
];

const VIDEO_MIME_TYPES = ["video/mp4", "video/webm"];

const IFRAME_MIN_HEIGHT = 40;
const IFRAME_HEIGHT_MESSAGE = "jute-iframe-height";

const OutputViewDisplayData = memo(
  ({
    output,
    cellId,
    chromeless,
    afmPortBindings,
  }: {
    output: OutputDisplayData;
    cellId?: string;
    chromeless?: boolean;
    afmPortBindings?: AfmPortBindingSnapshot;
  }) => {
    const widgetView = anywidgetViewFromData(output.data[WIDGET_VIEW_MIME]);
    if (widgetView) {
      return (
        <AfmView
          key={widgetView.modelId}
          modelId={widgetView.modelId}
          widgetView={widgetView}
          chromeless={chromeless}
          portBindings={afmPortBindings}
        />
      );
    }

    const imageHtml = displayImageDataToHtml(output.data, output.metadata);
    if (imageHtml) {
      return <div dangerouslySetInnerHTML={{ __html: imageHtml }}></div>;
    }

    const videoHtml = displayVideoDataToHtml(output.data, output.metadata);
    if (videoHtml) {
      return <div dangerouslySetInnerHTML={{ __html: videoHtml }}></div>;
    }

    const html = displayStringData(output.data["text/html"]);
    if (html !== null) {
      return <HtmlOutput html={html} cellId={cellId} />;
    }

    const markdown = displayStringData(output.data["text/markdown"]);
    if (markdown !== null) {
      return <MarkdownOutput source={markdown} />;
    }

    const fallbackHtml = displayDataToHtml(output.data, output.metadata);

    if (fallbackHtml) {
      return <div dangerouslySetInnerHTML={{ __html: fallbackHtml }}></div>;
    } else {
      return null;
    }
  },
);

// Active output scripts run in the sandboxed srcdoc iframe; same-origin is
// needed for capture APIs.
function HtmlOutput({ html, cellId }: { html: string; cellId?: string }) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(IFRAME_MIN_HEIGHT);
  const activeContent = useOutputActiveContentEnabled();
  const srcDoc = useMemo(
    () => withHeightReporter(withVideoCapture(html)),
    [html],
  );

  useEffect(() => {
    setHeight(IFRAME_MIN_HEIGHT);
  }, [activeContent, html]);

  useEffect(() => {
    const handleMessage = (event: MessageEvent) => {
      if (event.source !== iframeRef.current?.contentWindow) {
        return;
      }
      if (typeof event.data !== "object" || event.data === null) {
        return;
      }

      if (
        event.data.type === IFRAME_HEIGHT_MESSAGE &&
        typeof event.data.height === "number" &&
        Number.isFinite(event.data.height)
      ) {
        setHeight(Math.max(IFRAME_MIN_HEIGHT, Math.ceil(event.data.height)));
        return;
      }

      if (
        event.data.type === "jute-video-capture" &&
        typeof event.data.cellId === "string" &&
        event.data.cellId.length > 0 &&
        event.data.cellId === cellId &&
        typeof event.data.webm === "string" &&
        typeof event.data.duration_sec === "number" &&
        Number.isFinite(event.data.duration_sec)
      ) {
        void invoke("push_capture_port", {
          port: event.data.cellId,
          webmBase64: event.data.webm,
          durationSec: event.data.duration_sec,
        }).catch((error) => {
          console.error("Failed to push capture port", error);
        });
      }
    };

    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, [cellId]);

  return (
    <iframe
      key={activeContent ? "active" : "static"}
      ref={iframeRef}
      name={cellId ?? ""}
      title="Notebook HTML output"
      srcDoc={srcDoc}
      // TODO: per-notebook trust (Jupyter-style signature) deferred.
      sandbox={htmlOutputSandbox(activeContent)}
      className="block w-full border-0"
      style={{ height, minHeight: IFRAME_MIN_HEIGHT }}
    />
  );
}

function MarkdownOutput({ source }: { source: string }) {
  return (
    <div className="text-sm">
      <MarkdownRenderer source={source} />
    </div>
  );
}

/**
 * Returns the HTML form of a display data message.
 *
 * https://jupyter-client.readthedocs.io/en/stable/messaging.html#display-data
 */
function displayDataToHtml(
  data: Record<string, any>,
  metadata: Record<string, any>,
): string | null {
  const imageHtml = displayImageDataToHtml(data, metadata);
  if (imageHtml) {
    return imageHtml;
  }

  const value = data["text/plain"];
  if (typeof value === "string") {
    return `<pre>${encode(value)}</pre>`;
  } else if (Array.isArray(value)) {
    return `<pre>${encode(value.join(""))}</pre>`;
  }

  return null;
}

function displayImageDataToHtml(
  data: Record<string, any>,
  metadata: Record<string, any>,
): string | null {
  for (const imageType of IMAGE_MIME_TYPES) {
    if (Object.hasOwn(data, imageType)) {
      const value = data[imageType];
      const alt = String(data["text/plain"] ?? "");
      const meta = metadata[imageType];
      if (typeof value === "string") {
        let image = `<img src="data:${imageType};base64,${encode(value)}" alt="${encode(alt)}"`;
        if (meta) {
          if (typeof meta.height === "number" && meta.height > 0) {
            image += ` height="${meta.height}"`;
          }
          if (typeof meta.width === "number" && meta.width > 0) {
            image += ` width="${meta.width}"`;
          }
        }
        image += " />";
        return image;
      }
    }
  }

  return null;
}

function displayVideoDataToHtml(
  data: Record<string, any>,
  _metadata: Record<string, any>,
): string | null {
  for (const videoType of VIDEO_MIME_TYPES) {
    if (Object.hasOwn(data, videoType)) {
      const value = data[videoType];
      if (typeof value === "string") {
        return `<video controls style="max-width:100%"><source src="data:${videoType};base64,${encode(value)}" type="${videoType}"></video>`;
      }
    }
  }

  return null;
}

function displayStringData(value: unknown): string | null {
  if (typeof value === "string") {
    return value;
  }

  if (Array.isArray(value) && value.every((item) => typeof item === "string")) {
    return multiline(value);
  }

  return null;
}

function withHeightReporter(html: string): string {
  return `${html}
<script>
(() => {
  const postHeight = () => {
    window.parent.postMessage({
      type: "${IFRAME_HEIGHT_MESSAGE}",
      height: document.body.scrollHeight,
    }, "*");
  };

  window.addEventListener("load", postHeight);
  new ResizeObserver(postHeight).observe(document.body);
  requestAnimationFrame(postHeight);
  postHeight();
})();
</script>`;
}
