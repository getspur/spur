import clsx from "clsx";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

type Props = { blocks: Block[]; themeId: string; fragmentIndex?: number };

export default function BulletsLayout({
  blocks,
  themeId,
  fragmentIndex = 0,
}: Props) {
  const theme = resolveTheme(themeId);
  const md =
    (
      blocks.find((block) => block.kind === "markdown") as
        | { kind: "markdown"; md: string }
        | undefined
    )?.md ?? "";
  const lines = md.split("\n");
  const headingLine = lines.find((line) => /^#{1,3}\s+/.test(line)) ?? "";
  const heading = headingLine.replace(/^#{1,3}\s+/, "");
  const bullets = lines
    .filter((line) => /^\s*[-*]\s+/.test(line))
    .map((line) => line.replace(/^\s*[-*]\s+/, ""));

  return (
    <div className="flex h-full flex-col justify-center">
      {heading && (
        <h2 className={clsx("mb-6 text-[3cqi] font-bold", theme.heading)}>
          {heading}
        </h2>
      )}
      <ul className="list-none space-y-3 p-0">
        {bullets.map((bullet, index) => (
          <li
            key={index}
            className={clsx(
              "flex items-start text-[2cqi] leading-snug transition-opacity [&_code]:rounded [&_code]:bg-black/5 [&_code]:px-1 [&_code]:font-mono",
              theme.body,
              index > fragmentIndex && "opacity-30",
            )}
          >
            <span className={clsx("mr-3 font-bold", theme.accent)}>{"->"}</span>
            <span
              dangerouslySetInnerHTML={{
                __html: renderInlineMarkdown(bullet),
              }}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}

function renderInlineMarkdown(source: string): string {
  const escaped = source
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
  return escaped
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
    .replace(/\*([^*]+)\*/g, "<i>$1</i>");
}
