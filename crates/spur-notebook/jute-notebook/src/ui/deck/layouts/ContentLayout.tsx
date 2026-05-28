import clsx from "clsx";

import MarkdownRenderer from "@/ui/notebook/MarkdownRenderer";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

export default function ContentLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const md = blocks.find((block) => block.kind === "markdown");
  const source = md?.kind === "markdown" ? md.md : "";
  const theme = resolveTheme(themeId);

  return (
    <div
      className={clsx(
        "prose prose-slate prose-headings:text-[2.6cqi] flex h-full max-w-none flex-col justify-center text-[1.6cqi] leading-relaxed",
        theme.body,
      )}
    >
      <MarkdownRenderer source={source} />
    </div>
  );
}
