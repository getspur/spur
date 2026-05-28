import type { Block } from "../types";

export default function BlankLayout({
  blocks,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const html = blocks.find((block) => block.kind === "html");
  if (html?.kind === "html") {
    return (
      <div
        className="h-full w-full"
        dangerouslySetInnerHTML={{ __html: html.html }}
      />
    );
  }

  return <div className="h-full w-full" />;
}
