import OutputView from "@/ui/notebook/OutputView";

import type { Block } from "../types";

export default function OutputLayout({
  blocks,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const output = blocks.find((block) => block.kind === "output");
  return (
    <div className="flex h-full items-center justify-center overflow-auto">
      <div className="w-full max-w-full">
        {output?.kind === "output" && (
          <OutputView
            value={{ status: "success", outputs: output.outputs }}
            chromeless
          />
        )}
      </div>
    </div>
  );
}
