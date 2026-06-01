# Open Design on Jute — M1 Vertical Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Task 2 additionally requires the superpowers:writing-skills sub-skill for authoring the SKILL.md body.

**Goal:** Ship the `open-design` SPUR skill so the brain agent can run the Open Design loop (brief → discovery → direction → todo → artifact → critique) entirely by driving a Jute notebook through the existing `notebook_*` MCP tools, producing a final design artifact that renders as a `text/html` cell output — with no Node daemon and no jute schema change.

**Architecture:** Open Design's "brain" is prompt text + design assets, not runtime machinery. M1 re-homes that brain as a bundled SPUR skill (`crates/spur-core/src/skills/open-design/`). The skill teaches the agent to emit notebook cells via `notebook_insert_cell`/`notebook_write_cell`/`notebook_read_cell`, and to render the artifact as a cell whose output carries `text/html` — which Jute's `OutputView.tsx` already displays in a sandboxed iframe (`sandbox='allow-scripts'`, auto-height via the injected `jute-iframe-height` reporter). Skill registration is a one-line `include_str!` in `skills/mod.rs::bundled_raw()`; `list_active_skills` then exposes it automatically. Distribution to the 8 adapter dirs (and the `SPUR-MANAGED sha256` marker stamping) is done by `spur skills init` (`installer::run`).

**Tech Stack:** Rust (`spur-core` skills module, `spur-cli`), Markdown SKILL.md packages, the SPUR notebook MCP (`notebook_*` tools), Jute (Tauri + React) as the renderer. No new dependencies.

**Reference spec:** `docs/superpowers/specs/2026-05-31-open-design-jute-host-shell-design.ipynb` (the approved design, authored as a notebook).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/spur-core/src/skills/open-design/SKILL.md` | **New.** The skill: frontmatter (`name`/`description`/`role: brain`) + `SPUR-MANAGED` marker + body teaching the notebook-driven design loop. |
| `crates/spur-core/src/skills/open-design/references/directions.md` | **New.** The 5 visual directions (palette + font stack), ported verbatim from OD's `apps/daemon/src/prompts/directions.ts`. |
| `crates/spur-core/src/skills/open-design/references/critique.md` | **New.** The 5-dimensional self-critique + anti-AI-slop checklist, ported from OD's discovery prompt. |
| `crates/spur-core/src/skills/open-design/CREATION-LOG.md` | **New.** Provenance note (matches the `systematic-debugging` skill convention). |
| `crates/spur-core/src/skills/mod.rs` | **Modify.** Register `open-design` in `bundled_raw()` (`mod.rs:21-83`); add tests in the `tests` module (`mod.rs:301+`). |

Each reference file is loaded by the agent on demand (the SKILL.md body points to them by relative path), keeping the SKILL.md itself focused on the loop protocol.

**Out of scope for M1 (do NOT build):** deck mode routing (M2), reactive live-artifact DAG (M3), interactive form cell types / export / design-system browser (M4), any change to `jute-notebook` Rust or React, any new `notebook_*` MCP tool, any per-cell artifact-manifest metadata schema.

---

## Task 1: Register the `open-design` skill and scaffold its SKILL.md

**Files:**
- Create: `crates/spur-core/src/skills/open-design/SKILL.md`
- Modify: `crates/spur-core/src/skills/mod.rs:21-83` (the `bundled_raw()` map), `crates/spur-core/src/skills/mod.rs:301+` (the `tests` module)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the end of `crates/spur-core/src/skills/mod.rs` (before the final closing `}`):

```rust
    #[test]
    fn open_design_skill_is_bundled_and_loadable() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake)
            .expect("open-design skill must be bundled");
        // Drives the notebook through the MCP tool surface, not a Node daemon.
        assert!(
            body.contains("notebook_insert_cell"),
            "skill must instruct driving the notebook via notebook_* tools"
        );
        assert!(
            body.contains("text/html"),
            "skill must instruct emitting the artifact as a text/html cell output"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib skills::tests::open_design_skill_is_bundled_and_loadable`
Expected: FAIL — `open-design skill must be bundled` (the key is absent from `bundled_raw()`), or a compile error from the missing `include_str!` target if you registered before creating the file. Create the file in Step 3 first if you hit a compile error.

- [ ] **Step 3: Create the SKILL.md scaffold and register it**

Create `crates/spur-core/src/skills/open-design/SKILL.md` with this exact content (the `sha256` in the marker is a placeholder zero-hash; `spur skills init` in Task 4 regenerates the distributed copies' markers — the bundled source marker value does not affect `load_skill`):

```markdown
---
name: open-design
description: "Use when the user asks to design something visual — a landing page, pitch deck, poster, dashboard, mobile screen, or any UI artifact. Establishes the Open Design loop (discovery → direction → plan → artifact → critique) driven entirely by emitting Jute notebook cells through the notebook_* MCP tools, with the final artifact rendered as a text/html cell output."
role: brain
---
<!-- SPUR-MANAGED v=1 skill=open-design sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# Open Design — Notebook-Driven Visual Design

You are a senior product designer with a working notebook. You do not write prose
about a design; you **build the design as notebook cells**. The notebook IS the
project: brief, plan, and rendered artifact in one document.

<HARD-GATE>
You operate the notebook ONLY through the `notebook_*` MCP tools
(`notebook_insert_cell`, `notebook_write_cell`, `notebook_read_cell`,
`notebook_get_notebook`, `notebook_set_cell_metadata`). Never ask the user to
paste code or open files. The final artifact MUST be a cell whose output carries
`text/html`, so Jute renders it in its sandboxed iframe.
</HARD-GATE>

## The loop

1. **Discovery.** `notebook_insert_cell(kind=markdown)` a brief-lock form: surface,
   audience, tone, brand context, scale. Wait for the user to fill it, then
   `notebook_read_cell` to read their answers back.
2. **Direction.** If the user has no brand, insert a markdown cell offering the 5
   directions from `references/directions.md`. Apply the chosen palette + font
   stack deterministically — no freestyle colors.
3. **Plan.** Insert a markdown cell with a short TodoWrite-style plan.
4. **Artifact.** `notebook_insert_cell(kind=code)` then `notebook_write_cell` so the
   cell emits a single self-contained HTML document as a `text/html` output. Keep
   it one file (inline CSS + optional `<script>`). This is the rendered design.
5. **Critique.** Run the 5-dimensional self-critique in `references/critique.md`
   against your own output, then `notebook_write_cell` a revised artifact.

See `references/directions.md` and `references/critique.md`.
```

Then register it in `bundled_raw()` in `crates/spur-core/src/skills/mod.rs` — add this line immediately after the `writing-skills` insert (around `mod.rs:81`):

```rust
        m.insert("open-design", include_str!("open-design/SKILL.md"));
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_skill_is_bundled_and_loadable`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/open-design/SKILL.md crates/spur-core/src/skills/mod.rs
git commit -m "feat(skills): register open-design notebook-driven design skill"
```

---

## Task 2: Author the full design-loop body (writing-skills sub-skill)

**REQUIRED SUB-SKILL:** Invoke `superpowers:writing-skills` before editing the body. It governs SKILL.md structure, the `SPUR-MANAGED` marker, and tone.

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/SKILL.md` (expand the body)
- Modify: `crates/spur-core/src/skills/mod.rs:301+` (add a section-coverage test)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/spur-core/src/skills/mod.rs`:

```rust
    #[test]
    fn open_design_skill_covers_full_loop() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        for marker in [
            "Discovery",      // brief-lock step
            "Direction",      // direction picker step
            "Plan",           // todo plan step
            "Artifact",       // artifact emission step
            "Critique",       // self-critique step
            "references/directions.md",
            "references/critique.md",
            "notebook_read_cell",
            "notebook_write_cell",
        ] {
            assert!(
                body.contains(marker),
                "open-design body must cover `{marker}`"
            );
        }
    }
```

- [ ] **Step 2: Run test to verify it fails (or passes against the scaffold)**

Run: `cargo test -p spur-core --lib skills::tests::open_design_skill_covers_full_loop`
Expected: the scaffold from Task 1 already contains every marker, so this may PASS immediately. That is fine — the test is a regression guard. If any marker is missing, the test names it; add that section.

- [ ] **Step 3: Expand the body via writing-skills**

Using `superpowers:writing-skills`, flesh out each numbered step in the body with the concrete agent instructions below. Keep the HARD-GATE and the five step headers (`Discovery`/`Direction`/`Plan`/`Artifact`/`Critique`) so the test stays green. Add, under each step, the exact tool call shape, e.g.:

```markdown
### 4. Artifact

- `notebook_insert_cell(kind="code", source="# <skill> artifact")` to create the cell.
- `notebook_write_cell(id, source, expected_version)` where the cell, when rendered,
  yields one `text/html` output: a single self-contained HTML document (inline CSS,
  optional inline `<script>` for interactivity). No external assets, no build step.
- Do NOT split the artifact across files — M1 is single-entry HTML only.
- Re-read with `notebook_read_cell(id)` to confirm the output mime is `text/html`.
```

Weave in the Open Design senior-designer framing below (inlined here because
`resources/open-design/` is gitignored and absent from your tree — do not invent new
behavior beyond it):

- **Persona first:** "You are an expert designer working with the user as your
  manager. You produce design artifacts in HTML — prototypes, decks, dashboards,
  marketing pages. **HTML is your tool, not your medium**: when making slides be a
  slide designer; when making an app prototype be an interaction designer. Don't
  write a web page when the brief is a deck."
- **Embody the specialist:** slide deck → slide designer (fixed canvas, one idea per
  slide, headlines ≥ 36px, body ≥ 22px); mobile prototype → interaction designer
  (real device frame, 44px hit targets, real screens not placeholders); landing →
  brand designer (one hero, 3–6 sections, real copy, one decisive flourish);
  dashboard → systems designer (information density is the feature; mono numerics,
  tabular data, no decoration).
- **Speed of feedback is the point:** discovery is time-to-first-byte — "30 seconds
  of radios beats 30 minutes of redirects." Lock the brief before building.
- **Critique + anti-slop are non-negotiable:** Step 5 runs the 5-dimensional critique
  and the anti-AI-slop checklist in `references/critique.md` before finalizing.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_skill_covers_full_loop`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/open-design/SKILL.md crates/spur-core/src/skills/mod.rs
git commit -m "feat(skills): author open-design full loop body"
```

---

## Task 3: Port the directions and critique reference files

**Files:**
- Create: `crates/spur-core/src/skills/open-design/references/directions.md`
- Create: `crates/spur-core/src/skills/open-design/references/critique.md`
- Modify: `crates/spur-core/src/skills/mod.rs:301+` (add a reference-content test)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/spur-core/src/skills/mod.rs`:

```rust
    #[test]
    fn open_design_directions_reference_lists_all_five() {
        // The reference files live beside the bundled skill source.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references");
        let directions = std::fs::read_to_string(dir.join("directions.md"))
            .expect("directions.md must exist");
        for school in [
            "Editorial Monocle",
            "Modern Minimal",
            "Warm Soft",
            "Tech Utility",
            "Brutalist Experimental",
        ] {
            assert!(directions.contains(school), "directions.md must list `{school}`");
        }
        assert!(
            directions.contains("oklch"),
            "directions must carry deterministic OKLch palettes"
        );
        let critique = std::fs::read_to_string(dir.join("critique.md"))
            .expect("critique.md must exist");
        assert!(
            critique.to_lowercase().contains("anti-ai-slop")
                || critique.to_lowercase().contains("anti ai slop"),
            "critique.md must include the anti-AI-slop checklist"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib skills::tests::open_design_directions_reference_lists_all_five`
Expected: FAIL — `directions.md must exist` (file not created yet).

- [ ] **Step 3: Create the reference files**

> The Open Design source (`resources/open-design/`) is **gitignored** and absent from
> the worker's base tree, so the exact content is inlined here. Write each file
> **verbatim** as given — do not paraphrase or re-derive the OKLch tokens.

Create `crates/spur-core/src/skills/open-design/references/directions.md` with exactly:

```markdown
# Open Design — Visual Directions

When the user has no brand, offer these 5 directions, then bind the chosen one's
OKLch palette + font stack to the artifact's CSS `:root`. Deterministic — never
improvise colors. Keep the directions visually distinct.

## Editorial Monocle
- **Label:** Editorial — Monocle / FT magazine
- **Mood:** Print-magazine feel. Generous whitespace, large serif headlines, restrained palette of off-white paper + ink + a single warm accent. Confident, quietly intelligent.
- **References:** Monocle · The Financial Times Weekend · NYT Magazine · It's Nice That
- **Display font:** `'Iowan Old Style', 'Charter', Georgia, serif`
- **Body font:** `-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif`
- **Palette (OKLch):** bg `oklch(97% 0.012 80)` · surface `oklch(99% 0.005 80)` · fg `oklch(20% 0.02 60)` · muted `oklch(48% 0.015 60)` · border `oklch(89% 0.012 80)` · accent `oklch(58% 0.16 35)`
- **Posture:** serif display, sans body, mono for metadata only; no shadows, no rounded cards — borders + whitespace do the work; one decisive image cropped at the bottom; kicker in mono uppercase; one accent color used at most twice.

## Modern Minimal
- **Label:** Modern minimal — Linear / Vercel
- **Mood:** Quiet, precise, software-native. System fonts, near-greyscale palette, a single saturated accent. The chrome disappears so content is the only thing that registers.
- **References:** Linear · Vercel · Notion 2024 · Stripe docs
- **Display font:** `-apple-system, BlinkMacSystemFont, 'SF Pro Display', system-ui, sans-serif`
- **Body font:** `-apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif`
- **Palette (OKLch):** bg `oklch(99% 0.002 240)` · surface `oklch(100% 0 0)` · fg `oklch(18% 0.012 250)` · muted `oklch(54% 0.012 250)` · border `oklch(92% 0.005 250)` · accent `oklch(58% 0.18 255)`
- **Posture:** tight letter-spacing on display (-0.02em); hairline borders only, no shadows except dropdowns/modals; tabular-nums; sticky frosted nav, content-led layouts; one accent for links + primary CTA only.

## Warm Soft
- **Label:** Warm & soft — Stripe pre-2020 / Headspace
- **Mood:** Cream backgrounds, soft accent, gentle radii. Reads like a thoughtful product magazine — friendly without being cute. Good for fintech, wellness, indie SaaS.
- **References:** Stripe pre-2020 · Headspace · Substack · Mercury
- **Display font:** `'Tiempos Headline', 'Newsreader', 'Iowan Old Style', Georgia, serif`
- **Body font:** `'Söhne', -apple-system, BlinkMacSystemFont, system-ui, sans-serif`
- **Palette (OKLch):** bg `oklch(97% 0.018 70)` · surface `oklch(99% 0.008 70)` · fg `oklch(22% 0.02 50)` · muted `oklch(50% 0.018 50)` · border `oklch(90% 0.014 70)` · accent `oklch(64% 0.13 28)`
- **Posture:** serif display, soft sans body; gentle radii (12–16px), no hard 0px corners; single accent for CTA + one editorial flourish; soft inner glow rather than drop shadows; real screenshots/photos over icons.

## Tech Utility
- **Label:** Tech / utility — Datadog / GitHub
- **Mood:** Data-dense, monospace-friendly, light + grid. Made for engineers and operators who want information per square inch, not vibes.
- **References:** Datadog · GitHub · Cloudflare dashboard · Sentry
- **Display font:** `-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif`
- **Body font:** `-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif`
- **Mono font:** `'JetBrains Mono', 'IBM Plex Mono', ui-monospace, Menlo, monospace`
- **Palette (OKLch):** bg `oklch(98% 0.005 250)` · surface `oklch(100% 0 0)` · fg `oklch(22% 0.02 240)` · muted `oklch(50% 0.018 240)` · border `oklch(90% 0.008 240)` · accent `oklch(58% 0.16 145)`
- **Posture:** one sans family OK — utility trumps editorial; tabular numerics everywhere, mono for code/IDs/hashes; dense tables with hairline borders, no striping; inline status pills with restrained tinted backgrounds; show the product, not hero images.

## Brutalist Experimental
- **Label:** Brutalist / experimental — Are.na / Yale
- **Mood:** Loud type. Visible grid. System sans + a single oversized serif. Deliberate ugliness as confidence. Great for art, indie, agency, manifesto pages.
- **References:** Are.na · Yale Center for British Art · mschf · Read.cv
- **Display font:** `'Times New Roman', 'Iowan Old Style', Georgia, serif`
- **Body font:** `ui-monospace, 'IBM Plex Mono', 'JetBrains Mono', Menlo, monospace`
- **Palette (OKLch):** bg `oklch(96% 0.004 100)` · surface `oklch(100% 0 0)` · fg `oklch(15% 0.02 100)` · muted `oklch(40% 0.02 100)` · border `oklch(15% 0.02 100)` · accent `oklch(60% 0.22 25)`
- **Posture:** serif display at extreme sizes (clamp(80px, 12vw, 200px)); monospace body, deliberately; full-strength fg borders (1.5–2px); asymmetric 70/30 columns; near-zero radius, no shadows, no gradients; underline links, no hover decoration.
```

Create `crates/spur-core/src/skills/open-design/references/critique.md` with exactly:

```markdown
# Open Design — Self-Critique & Anti-AI-Slop

## 5-dimensional critique (run before finalizing the artifact)

Score yourself silently 1–5 on each. Any dimension under 3/5 is a regression — go
back, fix the weakest, re-score. Two passes is normal.

1. **Philosophy** — does the visual posture match what was asked (editorial vs minimal vs brutalist)? Or did you drift back to your favourite default?
2. **Hierarchy** — does the eye land in one obvious place per screen? Or is everything competing?
3. **Execution** — typography, spacing, alignment, contrast — right, or just close?
4. **Specificity** — is every word, number, image specific to *this* brief? Or did generic stat-slop creep in?
5. **Restraint** — one accent used at most twice, one decisive flourish — or three competing flourishes?

## Anti-AI-slop checklist (audit before shipping)

- ❌ Aggressive purple/violet gradient backgrounds
- ❌ Generic emoji feature icons (✨ 🚀 🎯 …)
- ❌ Rounded card with a left coloured border accent
- ❌ Hand-drawn SVG humans / faces / scenery
- ❌ Inter / Roboto / Arial as a *display* face (body is fine)
- ❌ Invented metrics ("10× faster", "99.9% uptime") without a source
- ❌ Filler copy — "Feature One / Feature Two", lorem ipsum
- ❌ An icon next to every heading
- ❌ A gradient on every background

When you don't have a real value, leave an honest placeholder (`—`, a grey block, a
labelled stub) instead of inventing one. An honest placeholder beats a fake stat.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_directions_reference_lists_all_five`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/
git commit -m "feat(skills): port open-design directions + critique references"
```

---

## Task 4: Description trigger guard, provenance log, and adapter distribution

**Files:**
- Create: `crates/spur-core/src/skills/open-design/CREATION-LOG.md`
- Modify: `crates/spur-core/src/skills/mod.rs:301+` (add a description-trigger test)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/spur-core/src/skills/mod.rs` (mirrors the existing `*_description_contains_trigger_phrases` tests at `mod.rs` tail):

```rust
    #[test]
    fn open_design_description_contains_trigger_phrases() {
        let raw = all_bundled_raw().get("open-design").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("").to_lowercase();
        assert!(
            desc.contains("design") || desc.contains("landing") || desc.contains("deck"),
            "description should contain visual-design trigger phrases, got: {desc}"
        );
        assert_eq!(
            parsed.role,
            Some(crate::skills::SkillRole::Brain),
            "open-design is a brain-role skill"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails or passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_description_contains_trigger_phrases`
Expected: PASS if the Task 1 frontmatter (`role: brain`, design-trigger description) is intact; FAIL naming the missing phrase or wrong role otherwise. If `SkillRole` is not re-exported at `crate::skills::SkillRole`, use the path the other tests use (check the existing `*_description_*` tests' imports at `mod.rs` tail and match them).

- [ ] **Step 3: Create the provenance log**

Create `crates/spur-core/src/skills/open-design/CREATION-LOG.md`:

```markdown
# open-design — creation log

- **2026-05-31** — Created for the "Open Design on Jute" host-shell M1 vertical slice.
  Re-homes Open Design's prompt stack (discovery / directions / critique) as a
  notebook-driven SPUR skill. Source spec:
  `docs/superpowers/specs/2026-05-31-open-design-jute-host-shell-design.ipynb`.
  Replaces OD's Node daemon agent loop with `notebook_*` MCP tool driving.
```

- [ ] **Step 4: Run the full skills test suite, then distribute to adapter dirs**

Run: `cargo test -p spur-core --lib skills`
Expected: PASS (all open-design tests plus the pre-existing skills tests).

Then stamp the `SPUR-MANAGED` markers and render the skill into the per-adapter dirs:

Run: `cargo run -p spur-cli -- skills init`
Expected: command succeeds; because `open-design` is `role: brain`, the installer materializes it **only** under `.spur/skills/open-design/SKILL.md` with a valid `SPUR-MANAGED v=1 skill=open-design sha256=<64-hex>` marker. Worker adapter dirs (`.claude/skills/spurpower-*`, etc.) are intentionally skipped for brain-role skills (see the `run_skips_worker_adapters_for_brain_only_skill` test). Verify:

Run: `git status --short .spur/skills | head`
Expected: new `open-design` skill files listed under `.spur/skills/`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/open-design/CREATION-LOG.md crates/spur-core/src/skills/mod.rs .spur/skills .claude/skills .codex .gemini .kiro .opencode .kimi 2>/dev/null
git commit -m "feat(skills): distribute open-design skill to adapter dirs + provenance"
```

---

## Task 5: End-to-end verification (manual, via the live notebook)

This task has no unit test — it exercises the agent loop against a real Jute notebook
through the MCP. Treat the expected outcomes as the acceptance criteria for M1.

**Files:** none (verification only).

- [ ] **Step 1: Confirm the skill is active**

Run: `cargo run -p spur-cli -- skills init` (idempotent) and confirm a brain session
lists `open-design` among active skills (it is returned by `list_active_skills`
because it is bundled).

- [ ] **Step 2: Create a fresh notebook and drive the loop**

In a brain session with the notebook MCP connected:
1. `notebook_new` — create a scratch notebook.
2. Give the brief: *"Design a SaaS landing page hero for a developer tool."*
3. The agent (following the `open-design` skill) should:
   - `notebook_insert_cell(markdown)` a discovery form, then `notebook_read_cell` your answers.
   - `notebook_insert_cell(markdown)` a direction picker; apply the chosen direction's tokens.
   - `notebook_insert_cell(markdown)` a short plan.
   - `notebook_insert_cell(code)` + `notebook_write_cell` an artifact cell whose output is one `text/html` document.

- [ ] **Step 3: Verify the artifact renders**

Read it back: `notebook_read_cell(<artifact cell id>)`.
Expected: `outputs[0].output_type == "display_data"` and `outputs[0].data["text/html"]`
is present and non-empty. In the Jute window the cell shows the rendered hero in a
sandboxed iframe (enable *active content* if the artifact uses `<script>`).

- [ ] **Step 4: Verify the notebook is the project**

Run: `notebook_save` (or save in the UI), then `notebook_get_notebook(path)`.
Expected: the saved `.ipynb` contains the discovery, direction, plan, and artifact
cells — i.e. brief + transcript + rendered artifact persisted in one file, with no
`.od` SQLite store and no Node daemon involved anywhere in the flow.

- [ ] **Step 5: Record the result**

Append a short "M1 verified" note (date + the notebook path used) to
`crates/spur-core/src/skills/open-design/CREATION-LOG.md` and commit:

```bash
git add crates/spur-core/src/skills/open-design/CREATION-LOG.md
git commit -m "docs(skills): record open-design M1 end-to-end verification"
```

---

## Self-Review Notes

- **Spec coverage:** Subsystem table rows — *prompt stack* (Tasks 1–3), *agent runtime / notebook driving* (SKILL.md HARD-GATE + Task 5), *visualization / artifact-as-text/html* (Task 2 step 3 + Task 5 step 3), *persistence = the .ipynb* (Task 5 step 4). Deck (M2), live-DAG (M3), forms/export (M4) are explicitly deferred per the spec's milestones.
- **No jute change:** confirmed in the spec — `OutputView.tsx` already renders `text/html` in a sandboxed iframe and `set_cell_metadata` only accepts the typed `jute_deck` facet, so M1 deliberately avoids any artifact-manifest schema or React/Rust change in `jute-notebook`.
- **Type consistency:** test fn names, the `load_skill`/`all_bundled_raw`/`frontmatter::parse_source` calls, and the `bundled_raw()` insert site all match `crates/spur-core/src/skills/mod.rs` as read on 2026-05-31. `SkillRole::Brain` path in Task 4 step 2 has a fallback instruction in case the re-export path differs.
- **Open question deferred:** per-cell artifact manifest (spec open-question #1) is intentionally not in M1 — the `text/html` output is self-describing for the single-entry case.
