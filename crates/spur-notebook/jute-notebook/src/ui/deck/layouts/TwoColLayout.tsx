import clsx from "clsx";

import MarkdownRenderer from "@/ui/notebook/MarkdownRenderer";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

export default function TwoColLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const md = blocks.find((block) => block.kind === "markdown");
  const source = md?.kind === "markdown" ? md.md : "";
  const [heading, body] = splitHeading(source);
  const [left, right] = splitColumns(body);
  const theme = resolveTheme(themeId);

  return (
    <div className="flex h-full flex-col">
      {heading && (
        <h2
          className={clsx("mb-[4%] text-[2.4cqi] font-semibold", theme.heading)}
        >
          {heading}
        </h2>
      )}
      <div className="grid min-h-0 flex-1 grid-cols-2 gap-[6%]">
        <Column source={left} className={theme.body} />
        <Column source={right} className={theme.body} />
      </div>
    </div>
  );
}

function Column({ source, className }: { source: string; className: string }) {
  return (
    <div
      className={clsx(
        "min-w-0 text-[1.35cqi] leading-relaxed [&_h3]:mb-3 [&_h3]:text-[1.4cqi] [&_h3]:font-semibold [&_h3]:uppercase [&_h3]:tracking-[0.12em] [&_ul]:mt-auto",
        className,
      )}
    >
      <MarkdownRenderer source={source.trim()} />
    </div>
  );
}

function splitHeading(source: string): [string, string] {
  const lines = source.split("\n");
  const first = lines[0] ?? "";
  if (/^#{1,3}\s+/.test(first)) {
    return [first.replace(/^#{1,3}\s+/, ""), lines.slice(1).join("\n").trim()];
  }
  return ["", source];
}

function splitColumns(source: string): [string, string] {
  const fenced = source
    .split(/^:::\s*$/m)
    .map((part) => part.trim())
    .filter(Boolean);
  if (fenced.length >= 2) {
    return [fenced[0], fenced.slice(1).join("\n\n")];
  }

  const paragraphBreak = source.indexOf("\n\n");
  if (paragraphBreak >= 0) {
    return [
      source.slice(0, paragraphBreak),
      source.slice(paragraphBreak).trimStart(),
    ];
  }
  return [source, ""];
}
