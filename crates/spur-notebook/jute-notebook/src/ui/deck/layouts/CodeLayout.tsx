import clsx from "clsx";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

export default function CodeLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const code = blocks.find((block) => block.kind === "code");
  const fallback = blocks.find((block) => block.kind === "markdown");
  const source =
    code?.kind === "code"
      ? code.source
      : fallback?.kind === "markdown"
        ? fallback.md
        : "";
  const lang = code?.kind === "code" ? code.lang : "text";
  const theme = resolveTheme(themeId);

  return (
    <div className="flex h-full flex-col justify-center">
      <div
        className={clsx(
          "mb-3 text-[1cqi] font-semibold uppercase tracking-[0.14em]",
          theme.muted,
        )}
      >
        {lang}
      </div>
      <pre className="m-0 max-h-full overflow-auto rounded-md bg-slate-950 p-[3%] font-mono text-[1.25cqi] leading-relaxed text-slate-100">
        <code>{source}</code>
      </pre>
    </div>
  );
}
