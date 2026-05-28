import type { Block } from "../types";
import CodeLayout from "./CodeLayout";
import OutputLayout from "./OutputLayout";

export default function CodeOutputLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  return (
    <div className="grid h-full grid-cols-2 gap-[4%]">
      <div className="min-h-0">
        <CodeLayout blocks={blocks} themeId={themeId} />
      </div>
      <div className="min-h-0 rounded-md border border-slate-200/70 bg-white/70 p-[3%] text-slate-900">
        <OutputLayout blocks={blocks} themeId={themeId} />
      </div>
    </div>
  );
}
