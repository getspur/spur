import clsx from "clsx";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

export default function SectionLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const md = blocks.find((block) => block.kind === "markdown");
  const source = md?.kind === "markdown" ? md.md : "";
  const heading = source.replace(/^#{1,3}\s+/, "").trim();
  const theme = resolveTheme(themeId);

  return (
    <div className="flex h-full flex-col justify-center">
      <div
        className={clsx(
          "mb-4 text-[1.1cqi] font-semibold uppercase tracking-[0.2em]",
          theme.muted,
        )}
      >
        Section
      </div>
      <h2
        className={clsx("text-[4cqi] font-bold leading-tight", theme.heading)}
      >
        {heading}
      </h2>
    </div>
  );
}
