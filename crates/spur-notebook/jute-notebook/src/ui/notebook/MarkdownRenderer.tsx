import {
  Children,
  ReactNode,
  isValidElement,
  useEffect,
  useId,
  useMemo,
  useState,
} from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import { useMarkdownMermaidEnabled } from "@/stores/settings";

import { isMermaidLanguageClassName } from "./rendering";

type Props = {
  source: string;
  className?: string;
};

type MermaidState =
  | { status: "loading" }
  | { status: "rendered"; svg: string }
  | { status: "error"; message: string };

type MermaidModule = typeof import("mermaid").default;

let mermaidModulePromise: Promise<MermaidModule> | null = null;
let mermaidInitialized = false;
let mermaidRenderCounter = 0;

export default function MarkdownRenderer({ source, className }: Props) {
  const mermaidEnabled = useMarkdownMermaidEnabled();

  const components = useMemo<Components>(
    () => ({
      a: ({ ...props }) => <a {...props} target="_blank" rel="noreferrer" />,
      code: ({ className, children, ...props }) => {
        if (mermaidEnabled && isMermaidLanguageClassName(className)) {
          return <MermaidBlock source={childrenToString(children)} />;
        }

        return (
          <code className={className} {...props}>
            {children}
          </code>
        );
      },
      pre: ({ children, ...props }) => {
        const child = onlyChild(children);
        if (isValidElement(child) && child.type === MermaidBlock) {
          return child;
        }

        return <pre {...props}>{children}</pre>;
      },
    }),
    [mermaidEnabled],
  );

  return (
    <Markdown
      className={className}
      remarkPlugins={[remarkGfm]}
      components={components}
    >
      {source}
    </Markdown>
  );
}

function MermaidBlock({ source }: { source: string }) {
  const idPrefix = useStableMermaidIdPrefix();
  const [state, setState] = useState<MermaidState>({ status: "loading" });

  useEffect(() => {
    let cancelled = false;
    const renderId = `${idPrefix}-${mermaidRenderCounter++}`;
    setState({ status: "loading" });

    void renderMermaid(renderId, source)
      .then((svg) => {
        if (!cancelled) {
          setState({ status: "rendered", svg });
        }
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setState({
            status: "error",
            message: error instanceof Error ? error.message : String(error),
          });
        }
      });

    return () => {
      cancelled = true;
    };
  }, [idPrefix, source]);

  if (state.status === "rendered") {
    return (
      <div
        className="my-4 overflow-x-auto whitespace-normal"
        dangerouslySetInnerHTML={{ __html: state.svg }}
      />
    );
  }

  return (
    <div className="my-4 whitespace-normal">
      <code className="block whitespace-pre-wrap rounded border border-gray-200 bg-gray-50 p-3 text-xs text-gray-800">
        {source}
      </code>
      {state.status === "error" && (
        <p className="mt-2 text-xs text-red-600">
          Mermaid diagram failed to render: {state.message}
        </p>
      )}
    </div>
  );
}

function useStableMermaidIdPrefix() {
  const reactId = useId();
  return useMemo(
    () => `jute-mermaid-${reactId.replace(/[^A-Za-z0-9_-]/g, "")}`,
    [reactId],
  );
}

async function renderMermaid(id: string, source: string): Promise<string> {
  const mermaid = await loadMermaid();
  const { svg } = await mermaid.render(id, source);
  return svg;
}

async function loadMermaid(): Promise<MermaidModule> {
  if (!mermaidModulePromise) {
    mermaidModulePromise = import("mermaid").then((module) => {
      const mermaid = module.default;
      if (!mermaidInitialized) {
        mermaid.initialize({ startOnLoad: false, securityLevel: "strict" });
        mermaidInitialized = true;
      }
      return mermaid;
    });
  }

  return mermaidModulePromise;
}

function childrenToString(children: ReactNode): string {
  return Children.toArray(children).join("").replace(/\n$/, "");
}

function onlyChild(children: ReactNode): ReactNode {
  const childArray = Children.toArray(children);
  return childArray.length === 1 ? childArray[0] : null;
}
