import clsx from "clsx";
import type { CSSProperties, ReactNode } from "react";

import { resolveTheme } from "./themes";

type Props = {
  themeId: string;
  background?: string;
  children: ReactNode;
};

export default function SlideFrame({ themeId, background, children }: Props) {
  const theme = resolveTheme(themeId);
  const style: CSSProperties | undefined =
    theme.vars || background
      ? {
          ...(theme.vars as CSSProperties | undefined),
          ...(background ? { background } : {}),
        }
      : undefined;

  return (
    <section
      className={clsx(
        "relative flex h-full w-full flex-col p-[5%] [container-type:inline-size]",
        theme.frame,
      )}
      style={style}
      data-slide
    >
      {children}
    </section>
  );
}
