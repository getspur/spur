import clsx from "clsx";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

export default function TitleLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const md = blocks.find((block) => block.kind === "markdown");
  const source = md?.kind === "markdown" ? md.md : "";
  const [titleLine, ...rest] = source.replace(/^#{1,3}\s+/, "").split("\n");
  const subtitle = rest.join(" ").trim();
  const theme = resolveTheme(themeId);

  return (
    <div className="flex h-full flex-col justify-center">
      <h1
        className={clsx(
          "text-[5cqi] font-bold leading-tight tracking-normal",
          theme.heading,
        )}
      >
        {titleLine}
      </h1>
      {subtitle && (
        <p className={clsx("mt-4 text-[1.6cqi] font-normal", theme.muted)}>
          {subtitle}
        </p>
      )}
    </div>
  );
}
