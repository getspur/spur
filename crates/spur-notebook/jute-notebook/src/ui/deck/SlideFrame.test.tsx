import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import SlideFrame from "./SlideFrame";

describe("SlideFrame", () => {
  it("injects theme vars as inline CSS custom properties for token themes", () => {
    const { container } = render(
      <SlideFrame themeId="modern-minimal">x</SlideFrame>,
    );
    const root = container.querySelector("[data-slide]") as HTMLElement;

    expect(root.style.getPropertyValue("--deck-fg")).toBe(
      "oklch(18% 0.012 250)",
    );
    expect(root.style.getPropertyValue("--deck-accent")).toBe(
      "oklch(58% 0.18 255)",
    );
  });

  it("merges per-slide background over theme vars", () => {
    const { container } = render(
      <SlideFrame themeId="warm-soft" background="#000">
        x
      </SlideFrame>,
    );
    const root = container.querySelector("[data-slide]") as HTMLElement;

    expect(root.style.getPropertyValue("--deck-bg")).toBe(
      "oklch(97% 0.018 70)",
    );
    expect(root.style.background).toContain("rgb(0, 0, 0)");
  });

  it("leaves class-only themes without injected vars or style attributes", () => {
    const { container } = render(
      <SlideFrame themeId="minimal-light">x</SlideFrame>,
    );
    const root = container.querySelector("[data-slide]") as HTMLElement;

    expect(root).not.toHaveAttribute("style");
    expect(root.style.getPropertyValue("--deck-fg")).toBe("");
  });

  it("keeps class-only background overrides as the only inline style", () => {
    const { container } = render(
      <SlideFrame themeId="minimal-light" background="#000">
        x
      </SlideFrame>,
    );
    const root = container.querySelector("[data-slide]") as HTMLElement;

    expect(root.style.getPropertyValue("--deck-fg")).toBe("");
    expect(root.style.background).toContain("rgb(0, 0, 0)");
    expect(root.getAttribute("style")).not.toContain("--deck-");
  });
});
