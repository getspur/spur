import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, test } from "vitest";

import OutputView from "@/ui/notebook/OutputView";

import { LayoutFor } from "./layouts";
import BulletsLayout from "./layouts/BulletsLayout";
import OutputLayout from "./layouts/OutputLayout";
import type { Block, ResolvedLayout, SlideSpec } from "./types";

const markdownBlocks: Block[] = [
  { kind: "markdown", md: "## Slide title\n\nBody copy" },
];

function slide(layout: ResolvedLayout, blocks = markdownBlocks): SlideSpec {
  return {
    id: layout,
    layout,
    blocks,
    theme: "minimal-light",
    fragments: false,
  };
}

describe("deck layouts", () => {
  test("dispatcher renders every resolved layout", () => {
    const layouts: ResolvedLayout[] = [
      "title",
      "section",
      "content",
      "bullets",
      "code",
      "output",
      "code-output",
      "two-col",
      "image",
      "blank",
    ];

    for (const layout of layouts) {
      const html = renderToStaticMarkup(
        <LayoutFor slide={slide(layout)} fragmentIndex={0} />,
      );
      expect(html).toBeTruthy();
      expect(html).not.toContain("data-slide");
    }
  });

  test("bullets render all items and dim only items past fragmentIndex", () => {
    const html = renderToStaticMarkup(
      <BulletsLayout
        themeId="minimal-light"
        fragmentIndex={0}
        blocks={[
          {
            kind: "markdown",
            md: "### Three things\n- first\n- second\n- third",
          },
        ]}
      />,
    );

    expect(html).toContain("first");
    expect(html).toContain("second");
    expect(html).toContain("third");
    expect(html.match(/opacity-30/g)).toHaveLength(2);
  });

  test("output layout reuses OutputView without notebook spacing chrome", () => {
    const outputBlock: Block = {
      kind: "output",
      outputs: [{ output_type: "stream", name: "stdout", text: "hello\n" }],
    };

    const direct = renderToStaticMarkup(
      <OutputView
        value={{ status: "success", outputs: outputBlock.outputs }}
      />,
    );
    const chromeless = renderToStaticMarkup(
      <OutputView
        value={{ status: "success", outputs: outputBlock.outputs }}
        chromeless
      />,
    );
    const layout = renderToStaticMarkup(
      <OutputLayout blocks={[outputBlock]} themeId="minimal-light" />,
    );

    expect(direct).toContain("px-8");
    expect(chromeless).not.toContain("px-8");
    expect(layout).toContain("hello");
    expect(layout).not.toContain("px-8");
  });
});
