import { createElement } from "react";

import type { SlideSpec } from "../types";
import BlankLayout from "./BlankLayout";
import BulletsLayout from "./BulletsLayout";
import CodeLayout from "./CodeLayout";
import CodeOutputLayout from "./CodeOutputLayout";
import ContentLayout from "./ContentLayout";
import ImageLayout from "./ImageLayout";
import OutputLayout from "./OutputLayout";
import SectionLayout from "./SectionLayout";
import TitleLayout from "./TitleLayout";
import TwoColLayout from "./TwoColLayout";

export function LayoutFor({
  slide,
  fragmentIndex,
}: {
  slide: SlideSpec;
  fragmentIndex: number;
}) {
  const props = { blocks: slide.blocks, themeId: slide.theme, fragmentIndex };
  switch (slide.layout) {
    case "title":
      return createElement(TitleLayout, props);
    case "section":
      return createElement(SectionLayout, props);
    case "bullets":
      return createElement(BulletsLayout, props);
    case "content":
      return createElement(ContentLayout, props);
    case "code":
      return createElement(CodeLayout, props);
    case "output":
      return createElement(OutputLayout, props);
    case "code-output":
      return createElement(CodeOutputLayout, props);
    case "two-col":
      return createElement(TwoColLayout, props);
    case "image":
      return createElement(ImageLayout, props);
    case "blank":
      return createElement(BlankLayout, props);
  }

  const exhaustive: never = slide.layout;
  return exhaustive;
}
