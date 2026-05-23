import { encode } from "html-entities";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { MultilineString, OutputDisplayData } from "@/bindings";
import { CellResult } from "@/stores/notebook";

type Props = {
  value: CellResult | undefined;
};

export default function OutputView({ value }: Props) {
  if (!value) {
    return null;
  }
  const outputs = value.outputs ?? [];
  return (
    <div className="select-text whitespace-pre-wrap break-words px-8 pb-6 pt-4 text-sm after:contents">
      {outputs.map((output, index) => (
        <div key={index}>
          {output.output_type === "stream" ? (
            <pre>{multiline(output.text)}</pre>
          ) : output.output_type === "display_data" ? (
            <OutputViewDisplayData output={output} />
          ) : output.output_type === "execute_result" ? (
            <OutputViewDisplayData output={output} />
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

const IFRAME_MIN_HEIGHT = 40;
const IFRAME_HEIGHT_MESSAGE = "jute-iframe-height";

const OutputViewDisplayData = memo(
  ({ output }: { output: OutputDisplayData }) => {
    const imageHtml = displayImageDataToHtml(output.data, output.metadata);
    if (imageHtml) {
      return <div dangerouslySetInnerHTML={{ __html: imageHtml }}></div>;
    }

    const html = displayStringData(output.data["text/html"]);
    if (html !== null) {
      return <HtmlOutput html={html} />;
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

// sandbox='allow-scripts' lets output scripts run; omitting allow-same-origin blocks parent-origin storage access.
function HtmlOutput({ html }: { html: string }) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(IFRAME_MIN_HEIGHT);
  const srcDoc = useMemo(() => withHeightReporter(html), [html]);

  useEffect(() => {
    const handleMessage = (event: MessageEvent) => {
      if (event.source !== iframeRef.current?.contentWindow) {
        return;
      }
      if (
        typeof event.data !== "object" ||
        event.data === null ||
        event.data.type !== IFRAME_HEIGHT_MESSAGE ||
        typeof event.data.height !== "number" ||
        !Number.isFinite(event.data.height)
      ) {
        return;
      }
      setHeight(Math.max(IFRAME_MIN_HEIGHT, Math.ceil(event.data.height)));
    };

    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, []);

  return (
    <iframe
      ref={iframeRef}
      title="Notebook HTML output"
      srcDoc={srcDoc}
      sandbox="allow-scripts"
      className="block w-full border-0"
      style={{ height, minHeight: IFRAME_MIN_HEIGHT }}
    />
  );
}

function MarkdownOutput({ source }: { source: string }) {
  return (
    <div className="text-sm">
      <Markdown remarkPlugins={[remarkGfm]}>{source}</Markdown>
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
