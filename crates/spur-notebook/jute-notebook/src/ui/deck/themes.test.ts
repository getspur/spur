import { describe, expect, test } from "vitest";

import { THEMES, resolveTheme, type ThemeId } from "./themes";

const NEW: ThemeId[] = [
  "editorial-monocle",
  "modern-minimal",
  "warm-soft",
  "tech-utility",
  "brutalist",
];

describe("deck themes", () => {
  test("8 themes total, 3 originals preserved", () => {
    expect(Object.keys(THEMES)).toHaveLength(8);
    for (const id of [
      "minimal-light",
      "minimal-dark",
      "spur-brand",
    ] as ThemeId[]) {
      expect(THEMES[id].vars).toBeUndefined();
    }
  });

  test("each ported theme carries OKLch palette + font vars", () => {
    for (const id of NEW) {
      const v = THEMES[id].vars!;
      expect(v).toBeDefined();
      expect(v["--deck-bg"]).toMatch(/^oklch\(/);
      expect(v["--deck-fg"]).toMatch(/^oklch\(/);
      expect(v["--deck-accent"]).toMatch(/^oklch\(/);
      expect(v["--deck-font-display"]).toBeTruthy();
      expect(v["--deck-font-body"]).toBeTruthy();
      // class fields reference the vars (unambiguous arbitrary props)
      expect(THEMES[id].frame).toContain("var(--deck-bg)");
      expect(THEMES[id].heading).toContain("var(--deck-font-display)");
    }
  });

  test("exact anchor values (editorial-monocle, modern-minimal accent)", () => {
    expect(THEMES["editorial-monocle"].vars!["--deck-bg"]).toBe(
      "oklch(97% 0.012 80)",
    );
    expect(THEMES["modern-minimal"].vars!["--deck-accent"]).toBe(
      "oklch(58% 0.18 255)",
    );
    expect(THEMES["tech-utility"].vars!["--deck-font-mono"]).toContain(
      "JetBrains Mono",
    );
  });

  test("resolveTheme falls back to minimal-light for unknown", () => {
    expect(resolveTheme("nope").id).toBe("minimal-light");
    expect(resolveTheme("warm-soft").id).toBe("warm-soft");
  });
});
