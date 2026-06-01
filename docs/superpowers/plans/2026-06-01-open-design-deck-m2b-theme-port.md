# Open Design Deck — M2b: Theme Port Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb` (cells c5 #3, c8 "Theme bridge", c11 M2b)
**Design epic:** M2 deck-mode (spec merged)

**Goal:** Port the 5 Open Design visual directions into Jute's native deck `THEMES` so native decks aren't limited to the 3 built-in looks (`minimal-light/dark`, `spur-brand`).

**Architecture:** Extend the `Theme` type with an optional `vars` map of CSS custom properties. `SlideFrame` injects `vars` as inline CSS custom properties on the slide `<section>` root. The 5 new themes carry exact OKLch palettes + font stacks from `references/directions.md` as CSS vars, and their Tailwind class fields reference those vars via unambiguous arbitrary properties (`[color:var(--deck-fg)]`, `[font-family:var(--deck-font-display)]`, `[background:var(--deck-bg)]`). The 3 existing class-only themes have no `vars` and are byte-for-byte unchanged; all 8 layout components keep consuming `theme.heading/body/muted/accent/frame` as today — zero blast radius on the layouts.

**Tech Stack:** TypeScript + React, Tailwind (arbitrary-value classes), vitest (`npm test` → `vitest run`) in `crates/spur-notebook/jute-notebook/`.

---

## Source of truth — the 5 directions (from `crates/spur-core/src/skills/open-design/references/directions.md`)

| theme id | display font | body font | bg | surface | fg | muted | border | accent |
|---|---|---|---|---|---|---|---|---|
| `editorial-monocle` | `'Iowan Old Style', 'Charter', Georgia, serif` | `-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif` | `oklch(97% 0.012 80)` | `oklch(99% 0.005 80)` | `oklch(20% 0.02 60)` | `oklch(48% 0.015 60)` | `oklch(89% 0.012 80)` | `oklch(58% 0.16 35)` |
| `modern-minimal` | `-apple-system, BlinkMacSystemFont, 'SF Pro Display', system-ui, sans-serif` | `-apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif` | `oklch(99% 0.002 240)` | `oklch(100% 0 0)` | `oklch(18% 0.012 250)` | `oklch(54% 0.012 250)` | `oklch(92% 0.005 250)` | `oklch(58% 0.18 255)` |
| `warm-soft` | `'Tiempos Headline', 'Newsreader', 'Iowan Old Style', Georgia, serif` | `'Söhne', -apple-system, BlinkMacSystemFont, system-ui, sans-serif` | `oklch(97% 0.018 70)` | `oklch(99% 0.008 70)` | `oklch(22% 0.02 50)` | `oklch(50% 0.018 50)` | `oklch(90% 0.014 70)` | `oklch(64% 0.13 28)` |
| `tech-utility` | `-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif` | (same as display) | `oklch(98% 0.005 250)` | `oklch(100% 0 0)` | `oklch(22% 0.02 240)` | `oklch(50% 0.018 240)` | `oklch(90% 0.008 240)` | `oklch(58% 0.16 145)` |
| `brutalist` | `'Times New Roman', 'Iowan Old Style', Georgia, serif` | `ui-monospace, 'IBM Plex Mono', 'JetBrains Mono', Menlo, monospace` | `oklch(96% 0.004 100)` | `oklch(100% 0 0)` | `oklch(15% 0.02 100)` | `oklch(40% 0.02 100)` | `oklch(15% 0.02 100)` | `oklch(60% 0.22 25)` |

`tech-utility` and `brutalist` also define `--deck-font-mono`: `'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace`.

---

## Task 1: Extend `Theme` + add the 5 token themes

**Task ID:** `t1-themes`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/deck/themes.ts`
- Create: `crates/spur-notebook/jute-notebook/src/ui/deck/themes.test.ts`

**Depends on:** none

**Suggested Worker:** codex (focused, single-module + test)

**Scope Boundary:**
- IN scope: `themes.ts`, `themes.test.ts` only.
- OUT of scope: `SlideFrame.tsx`, any `layouts/*`, bindings, the spur-core skill. Do NOT touch `crates/spur-core/`. Do NOT read `resources/open-design/` (gitignored, absent).
- If you need to touch any out-of-scope file, emit `scope_drift` immediately.

**Acceptance Criteria:**
- [ ] `ThemeId` union has 8 ids; `THEMES` has 8 entries.
- [ ] The 3 original themes are byte-for-byte unchanged.
- [ ] Each new theme carries `vars` with the exact OKLch values + font stacks from the table above.
- [ ] `themes.test.ts` passes; `npm test` (in the jute-notebook dir) is green.

**Implementation:**

- [ ] **Step 1: Write the failing test** — `crates/spur-notebook/jute-notebook/src/ui/deck/themes.test.ts`

```ts
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
    for (const id of ["minimal-light", "minimal-dark", "spur-brand"] as ThemeId[]) {
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
    expect(THEMES["editorial-monocle"].vars!["--deck-bg"]).toBe("oklch(97% 0.012 80)");
    expect(THEMES["modern-minimal"].vars!["--deck-accent"]).toBe("oklch(58% 0.18 255)");
    expect(THEMES["tech-utility"].vars!["--deck-font-mono"]).toContain("JetBrains Mono");
  });

  test("resolveTheme falls back to minimal-light for unknown", () => {
    expect(resolveTheme("nope").id).toBe("minimal-light");
    expect(resolveTheme("warm-soft").id).toBe("warm-soft");
  });
});
```

- [ ] **Step 2: Run it red** — `npm test -- themes` in `crates/spur-notebook/jute-notebook/`. Expected: FAIL (themes not present / `vars` missing).

- [ ] **Step 3: Edit `themes.ts`.** Extend the type and union, add `vars`, append 5 themes. The 3 existing entries stay exactly as-is.

```ts
export type ThemeId =
  | "minimal-light"
  | "minimal-dark"
  | "spur-brand"
  | "editorial-monocle"
  | "modern-minimal"
  | "warm-soft"
  | "tech-utility"
  | "brutalist";

export type Theme = {
  id: ThemeId;
  // Tailwind utility classes applied to the SlideFrame root.
  frame: string;
  // Heading / body / muted text classes for layout components to consume.
  heading: string;
  body: string;
  muted: string;
  accent: string;
  // Optional CSS custom properties; when present, SlideFrame injects them on the
  // slide <section> root so the class fields below can reference them via
  // arbitrary properties. Absent on the 3 class-only built-ins.
  vars?: Record<string, string>;
};
```

Keep the existing `"minimal-light"`, `"minimal-dark"`, `"spur-brand"` entries unchanged, then add (token themes share identical class-field wiring; only `vars` differs):

```ts
  "editorial-monocle": {
    id: "editorial-monocle",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(97% 0.012 80)",
      "--deck-surface": "oklch(99% 0.005 80)",
      "--deck-fg": "oklch(20% 0.02 60)",
      "--deck-muted": "oklch(48% 0.015 60)",
      "--deck-border": "oklch(89% 0.012 80)",
      "--deck-accent": "oklch(58% 0.16 35)",
      "--deck-font-display": "'Iowan Old Style', 'Charter', Georgia, serif",
      "--deck-font-body":
        "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif",
    },
  },
  "modern-minimal": {
    id: "modern-minimal",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(99% 0.002 240)",
      "--deck-surface": "oklch(100% 0 0)",
      "--deck-fg": "oklch(18% 0.012 250)",
      "--deck-muted": "oklch(54% 0.012 250)",
      "--deck-border": "oklch(92% 0.005 250)",
      "--deck-accent": "oklch(58% 0.18 255)",
      "--deck-font-display":
        "-apple-system, BlinkMacSystemFont, 'SF Pro Display', system-ui, sans-serif",
      "--deck-font-body":
        "-apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif",
    },
  },
  "warm-soft": {
    id: "warm-soft",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(97% 0.018 70)",
      "--deck-surface": "oklch(99% 0.008 70)",
      "--deck-fg": "oklch(22% 0.02 50)",
      "--deck-muted": "oklch(50% 0.018 50)",
      "--deck-border": "oklch(90% 0.014 70)",
      "--deck-accent": "oklch(64% 0.13 28)",
      "--deck-font-display":
        "'Tiempos Headline', 'Newsreader', 'Iowan Old Style', Georgia, serif",
      "--deck-font-body":
        "'Söhne', -apple-system, BlinkMacSystemFont, system-ui, sans-serif",
    },
  },
  "tech-utility": {
    id: "tech-utility",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(98% 0.005 250)",
      "--deck-surface": "oklch(100% 0 0)",
      "--deck-fg": "oklch(22% 0.02 240)",
      "--deck-muted": "oklch(50% 0.018 240)",
      "--deck-border": "oklch(90% 0.008 240)",
      "--deck-accent": "oklch(58% 0.16 145)",
      "--deck-font-display":
        "-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif",
      "--deck-font-body":
        "-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif",
      "--deck-font-mono":
        "'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace",
    },
  },
  "brutalist": {
    id: "brutalist",
    frame: "[background:var(--deck-bg)] [color:var(--deck-fg)]",
    heading: "[color:var(--deck-fg)] [font-family:var(--deck-font-display)]",
    body: "[color:var(--deck-fg)] [font-family:var(--deck-font-body)]",
    muted: "[color:var(--deck-muted)] [font-family:var(--deck-font-body)]",
    accent: "[color:var(--deck-accent)]",
    vars: {
      "--deck-bg": "oklch(96% 0.004 100)",
      "--deck-surface": "oklch(100% 0 0)",
      "--deck-fg": "oklch(15% 0.02 100)",
      "--deck-muted": "oklch(40% 0.02 100)",
      "--deck-border": "oklch(15% 0.02 100)",
      "--deck-accent": "oklch(60% 0.22 25)",
      "--deck-font-display": "'Times New Roman', 'Iowan Old Style', Georgia, serif",
      "--deck-font-body":
        "ui-monospace, 'IBM Plex Mono', 'JetBrains Mono', Menlo, monospace",
      "--deck-font-mono":
        "'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace",
    },
  },
```

`resolveTheme` is unchanged (its `id in THEMES` guard already handles the new ids).

- [ ] **Step 4: Run green** — `npm test -- themes`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/deck/themes.ts \
        crates/spur-notebook/jute-notebook/src/ui/deck/themes.test.ts
git commit -m "deck(themes): port 5 Open Design directions as CSS-token themes"
```

---

## Task 2: Inject theme `vars` in `SlideFrame`

**Task ID:** `t2-slideframe`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/deck/SlideFrame.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/deck/SlideFrame.test.tsx` (create if absent)

**Depends on:** `t1-themes`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `SlideFrame.tsx` + its test only.
- OUT of scope: `themes.ts` (done in t1), `layouts/*`, bindings, spur-core. Do NOT touch `crates/spur-core/`.
- Emit `scope_drift` if you need anything else.

**Acceptance Criteria:**
- [ ] For a token theme, the rendered `<section data-slide>` carries the theme's CSS custom properties inline (e.g. `--deck-fg`), merged with any per-slide `background` override.
- [ ] For a class-only theme (no `vars`), behavior is unchanged: `style` is `undefined` when no `background`, and only `{ background }` when set.
- [ ] `npm test` green.

**Implementation:**

- [ ] **Step 1: Write the failing test** — `SlideFrame.test.tsx`

```tsx
import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import SlideFrame from "./SlideFrame";

describe("SlideFrame", () => {
  it("injects theme vars as inline CSS custom properties for token themes", () => {
    const { container } = render(<SlideFrame themeId="modern-minimal">x</SlideFrame>);
    const root = container.querySelector("[data-slide]") as HTMLElement;
    expect(root.style.getPropertyValue("--deck-fg")).toBe("oklch(18% 0.012 250)");
    expect(root.style.getPropertyValue("--deck-accent")).toBe("oklch(58% 0.18 255)");
  });

  it("merges per-slide background over theme vars", () => {
    const { container } = render(
      <SlideFrame themeId="warm-soft" background="#000">x</SlideFrame>,
    );
    const root = container.querySelector("[data-slide]") as HTMLElement;
    expect(root.style.getPropertyValue("--deck-bg")).toBe("oklch(97% 0.018 70)");
    expect(root.style.background).toContain("rgb(0, 0, 0)");
  });

  it("leaves class-only themes without injected vars", () => {
    const { container } = render(<SlideFrame themeId="minimal-light">x</SlideFrame>);
    const root = container.querySelector("[data-slide]") as HTMLElement;
    expect(root.style.getPropertyValue("--deck-fg")).toBe("");
  });
});
```

- [ ] **Step 2: Run red** — `npm test -- SlideFrame`. Expected: FAIL.

- [ ] **Step 3: Edit `SlideFrame.tsx`.** Build the style object from `theme.vars` plus the optional `background`. Current `style` line:

```tsx
      style={background ? { background } : undefined}
```

Replace with:

```tsx
      style={
        theme.vars || background
          ? { ...(theme.vars as React.CSSProperties | undefined), ...(background ? { background } : {}) }
          : undefined
      }
```

(Keep everything else — the `clsx("...", theme.frame)` className and `data-slide` — unchanged. Ensure `React` types are available; the file already imports from React for `ReactNode`.)

- [ ] **Step 4: Run green** — `npm test -- SlideFrame`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/deck/SlideFrame.tsx \
        crates/spur-notebook/jute-notebook/src/ui/deck/SlideFrame.test.tsx
git commit -m "deck(SlideFrame): inject theme CSS-token vars on the slide root"
```

---

## Task 3: Surface the new themes in the skill + provenance + final gate

**Task ID:** `t3-skill-provenance`

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/references/deck-mode.md`
- Modify: `crates/spur-core/src/skills/open-design/CREATION-LOG.md`
- Modify: `crates/spur-core/src/skills/mod.rs`

**Depends on:** `t2-slideframe`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above (all in `crates/spur-core/`).
- OUT of scope: `crates/spur-notebook/` (done in t1/t2). Do NOT read `resources/open-design/`.
- Emit `scope_drift` otherwise.

**Acceptance Criteria:**
- [ ] `deck-mode.md` lists the 5 new theme ids alongside the 3 built-ins.
- [ ] New test `open_design_deck_mode_lists_ported_themes` passes.
- [ ] Final gate `cargo test -p spur-core --lib skills` is green.

**Implementation:**

- [ ] **Step 1: Write the failing test** in `crates/spur-core/src/skills/mod.rs` (in `mod tests`, beside `open_design_deck_mode_native_flow`):

```rust
    #[test]
    fn open_design_deck_mode_lists_ported_themes() {
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references/deck-mode.md");
        let text = std::fs::read_to_string(&refs).expect("deck-mode.md must exist");
        for id in [
            "editorial-monocle",
            "modern-minimal",
            "warm-soft",
            "tech-utility",
            "brutalist",
        ] {
            assert!(text.contains(id), "deck-mode.md must list ported theme `{id}`");
        }
    }
```

- [ ] **Step 2: Run red** — `cargo test -p spur-core --lib skills::tests::open_design_deck_mode_lists_ported_themes`. Expected: FAIL.

- [ ] **Step 3: Edit `deck-mode.md`.** Find the existing themes line:

```
title, author?: "<name>" }`. (More themes arrive in M2b; the 3
built-ins are `minimal-light`, `minimal-dark`, `spur-brand`.)
```

Replace the parenthetical with the full set:

```
title, author?: "<name>" }`. Built-in themes: `minimal-light`, `minimal-dark`,
`spur-brand`, plus the 5 ported Open Design directions — `editorial-monocle`,
`modern-minimal`, `warm-soft`, `tech-utility`, `brutalist` (OKLch palettes + font
stacks from `references/directions.md`).
```

- [ ] **Step 4: Append the M2b CREATION-LOG entry** to `crates/spur-core/src/skills/open-design/CREATION-LOG.md`:

```markdown
- **2026-06-01** — M2b: theme port. Ported the 5 Open Design directions
  (`editorial-monocle`, `modern-minimal`, `warm-soft`, `tech-utility`, `brutalist`)
  into Jute's native deck `THEMES` as CSS-token themes (OKLch palettes + font stacks
  injected by `SlideFrame`). Native decks now offer 8 themes; the 3 class-only built-ins
  are unchanged. Spec: `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb`.
```

- [ ] **Step 5: Final gate** — `cargo test -p spur-core --lib skills`. Expected: PASS (all skills tests, including the new one).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/deck-mode.md \
        crates/spur-core/src/skills/open-design/CREATION-LOG.md \
        crates/spur-core/src/skills/mod.rs
git commit -m "open-design: surface 5 ported deck themes + M2b provenance"
```

---

## Self-Review

- **Spec coverage:** c8 "Theme bridge" (5 directions → Jute THEMES) → t1. Native-track theming (decision #3 / open decision #2 = CSS tokens) → t1/t2. M2b milestone (c11) → all three tasks.
- **Placeholders:** none — exact OKLch/font values inlined; exact code for `themes.ts`, `SlideFrame.tsx`, test files, and the doc edit.
- **Type consistency:** `ThemeId` union (8) ↔ `THEMES` keys (8) ↔ test `NEW` list (5) ↔ skill doc ids (5) ↔ Rust test ids (5) all match.
- **DAG:** t1 → t2 → t3 (serial; t2 needs the `vars` field from t1, t3 documents what t1/t2 shipped). Valid, acyclic.
- **beads:** each task has a unique id, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary.
