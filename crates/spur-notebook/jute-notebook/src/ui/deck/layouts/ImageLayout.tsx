import clsx from "clsx";

import { resolveTheme } from "../themes";
import type { Block } from "../types";

const IMAGE_MIME_TYPES = [
  "image/png",
  "image/jpeg",
  "image/svg+xml",
  "image/bmp",
  "image/gif",
];

export default function ImageLayout({
  blocks,
  themeId,
}: {
  blocks: Block[];
  themeId: string;
  fragmentIndex?: number;
}) {
  const explicit = blocks.find((block) => block.kind === "image");
  const fromOutput = imageFromOutput(blocks);
  const image =
    explicit?.kind === "image"
      ? { src: explicit.src, alt: explicit.alt ?? "" }
      : fromOutput;
  const theme = resolveTheme(themeId);

  if (!image) {
    return <div className={clsx("h-full w-full", theme.muted)} />;
  }

  return (
    <div className="flex h-full items-center justify-center">
      <img
        src={image.src}
        alt={image.alt}
        className="max-h-full max-w-full object-contain"
      />
    </div>
  );
}

function imageFromOutput(blocks: Block[]): { src: string; alt: string } | null {
  const output = blocks.find((block) => block.kind === "output");
  if (output?.kind !== "output") return null;

  for (const item of output.outputs) {
    if (
      item.output_type !== "display_data" &&
      item.output_type !== "execute_result"
    ) {
      continue;
    }
    for (const mimeType of IMAGE_MIME_TYPES) {
      const value = item.data[mimeType];
      if (typeof value === "string") {
        return {
          src: `data:${mimeType};base64,${value}`,
          alt:
            typeof item.data["text/plain"] === "string"
              ? item.data["text/plain"]
              : "",
        };
      }
    }
  }

  return null;
}
