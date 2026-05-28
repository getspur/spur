import clsx from "clsx";
import type { ReactNode } from "react";

import { resolveTheme } from "./themes";

type Props = {
  themeId: string;
  background?: string;
  children: ReactNode;
};

export default function SlideFrame({ themeId, background, children }: Props) {
  const theme = resolveTheme(themeId);
  return (
    <section
      className={clsx(
        "relative flex h-full w-full flex-col p-[5%] [container-type:inline-size]",
        theme.frame,
      )}
      style={background ? { background } : undefined}
      data-slide
    >
      {children}
    </section>
  );
}
