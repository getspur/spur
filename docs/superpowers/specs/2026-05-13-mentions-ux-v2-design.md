# @-Mention UX v2 — Design Spec

**Date:** 2026-05-13
**Status:** Approved for planning
**Reviewers:** Claude (opus), Kimi, Gemini
**Owner:** spur-tui

## 1. Problem

`crates/spur-tui/src/mentions` currently routes four semantically distinct
mention kinds — files/directories, code-graph symbols and files, beads
issues, and worker agents — through a single `@` trigger and a single
blended picker. Users report that typing `@` to insert a file produces a
list where files are buried under workers, issues, and code symbols.

Root cause (verified by reading `registry.rs`):

- The empty-query branch pins workers (cap 6), then appends **all** issues
  with no cap, then files, then a capped code-graph sample. With even a
  modest issue snapshot, files are pushed off-screen.
- The typed-query branch applies an unconditional `+25%` score boost to
  workers, plus a class tie-break that puts `Worker`/`Issue` ahead of
  `File`. Files lose ties they should win.

The bug is a **ranking and grouping problem**, not a namespace problem.

## 2. Rejected alternatives

Three approaches were considered and rejected:

- **Per-kind trigger characters** (`#` for files+code, `!` for issues).
  Rejected: `#` collides with shell comments, markdown headings, and the
  GitHub `#1234` issue idiom; `!` collides with bash history expansion
  (`!!`, `!$`) and bang-commands. The original proposal also inverted
  GitHub convention by giving `#` to files rather than issues.
- **Empty-`@` chooser screen** (four type-rows the user picks first).
  Rejected: produces a jarring layout shift the instant the user types
  the first character (menu → list).
- **Verbose keyword prefixes** (`@file:foo`, `@issue:bd-1`). Rejected:
  ergonomic tax; users will skip them and fall back to bare `@`.

## 3. Design

Two phases. **Phase 1 is the actual fix.** Phase 2 is an optional
power-user layer that ships only if Phase 1 telemetry shows the pain
persists.

### 3.1 Phase 1 — Ranking & grouping

**Empty `@` becomes a sectioned list with hard caps.** Each section gets
a dimmed one-line header (`── Workers ──`, `── Files ──`, `── Issues ──`,
`── Code ──`). The headers teach users that four kinds exist without
adding a chooser step.

| Section | Cap | Sort order |
|---|---:|---|
| Workers | 4 | display length, then alpha |
| Files   | 6 | path depth, then display length, then alpha, then uri |
| Issues  | 3 | most-recent (descending), then id |
| Code    | 3 | `CodeFile` before `CodeSymbol`, then path depth, then alpha |

Total visible rows when all sections are full: 16 content rows + up to
4 header rows = 20. The picker's existing scroll behavior absorbs the
overflow; the first ~10 rows (Workers + start of Files) sit above the
fold so the "I want a file" case lands within the visible region.
Empty sections render no header (no dead space).

Note the section order differs from the typed-query tier order: empty
`@` shows **Workers first** because workers are the social anchor users
expect when addressing the agent, while typed `@foo` prefers **Files
first** because that's the most common intent for a fuzzy query. The
asymmetry is deliberate.

**Typed `@foo` (unified fuzzy, rebalanced):**

- Continue using nucleo with smart-case + smart-normalization.
- **Drop the unconditional `+25%` worker boost** (current
  `WORKER_SCORE_NUM`/`WORKER_SCORE_DEN`).
- Replace the class tie-break with **kind-tier ranking**: when two
  candidates' raw scores are within ~10% of each other, prefer
  `File` > `Worker` > `Issue` > `Code`. Files are the most common
  intent; workers stay easy to reach because their display strings are
  short.
- Outside the ~10% window, raw score wins (a clearly stronger match
  always beats a tier preference).
- Stable tie-key (`stable_tie_key`) unchanged.

### 3.2 Phase 2 — Optional disambiguator prefixes

Ship only if Phase 1 doesn't resolve the reported pain. Adds *optional*
prefix characters that hard-filter the picker to one kind. Bare `@foo`
remains unified fuzzy.

| Prefix syntax | Kind | Rationale |
|---|---|---|
| `@/<path>` | Files / directories | Mirrors path syntax; no Shift modifier |
| `@#<id>`   | Issues | GitHub `#1234` convention is universal |
| `@:<sym>`  | Code symbols | `::` scope resolution / LSP convention |
| `@<name>`  | Workers | Bare — workers own the unprefixed `@` namespace |

**Detection:** in `crates/spur-tui/src/components/completion_trigger.rs`,
extend `maybe_open` so that when `@` opens a `Mention` trigger and the
*next* typed char is `/`, `#`, or `:`, the detector records a
`kind_filter: Option<MentionKind>` on the trigger and strips the filter
char from the reported query. Backspace over the filter char restores
unified mode.

**Picker behavior with filter active:** only the filtered kind's section
renders, expanded to fill the popup. Header text shows the active
filter (e.g. `── Files (filter: @/) ──`) so the user can see and undo
it.

**Boundary rules unchanged:** the `@` trigger still only opens at
start-of-line or after whitespace. Prefix detection inherits the same
boundary; pasting `text @#1` does not open with a filter unless `@` is
at a valid boundary, and the prefix char must be the *next character
typed* (not pre-existing buffer content).

## 4. Implementation surface

- `crates/spur-tui/src/mentions/registry.rs`
  - Replace `WORKER_PIN_CAP=6` with per-kind caps:
    `WORKER_PIN_CAP=4`, `FILE_CAP=6`, `ISSUE_CAP=3`, `CODE_CAP=3`.
  - Empty-query branch: produce four section vectors in the order
    Workers → Files → Issues → Code, each capped, then concatenate
    with section-boundary markers usable by the renderer.
  - Typed-query branch: remove `+25%` worker boost; introduce
    `tier_rank(kind) -> u8` and a within-window comparator
    (`within(a.score, b.score, 10%) && tier_rank(a) != tier_rank(b)`).
- Picker render layer (whichever of
  `crates/spur-tui/src/components/input_completion.rs` or
  `crates/spur-tui/src/components/palette_overlay.rs` currently owns
  the mention-row render path — the implementation plan resolves this
  by reading the two files): emit one dimmed header row per non-empty
  section in empty-`@` mode. Header rows are non-selectable and skipped
  by arrow-key navigation.
- `crates/spur-tui/src/components/completion_trigger.rs` (Phase 2 only):
  add `kind_filter: Option<MentionKind>` to `Trigger`; teach
  `maybe_open` and `advance_composing` about the three prefix chars.

### 4.1 Tests

- `registry.rs` unit tests:
  - Empty-`@` returns at most 4/6/3/3 of each kind in the documented
    order, even when each source has more rows than the cap.
  - A typed query where two candidates score within 10% of each other
    returns the higher-tier kind first; outside the window, raw score
    wins.
  - Removing the worker boost does not regress
    `query_matches_issue_search_text_not_just_display` or
    `query_uses_smart_case_matching`.
- `completion_trigger.rs` unit tests (Phase 2):
  - `@/` opens with `kind_filter = Some(File)` and `query = ""`.
  - `@#bd` opens with `kind_filter = Some(Issue)` and `query = "bd"`.
  - `@:Foo` opens with `kind_filter = Some(CodeSymbol)` and `query = "Foo"`.
  - Backspacing the prefix char reverts to unified mode without closing.
  - Boundary rules: `text@/x` does not open (prev char not whitespace).
- Integration test in `crates/spur-tui/tests/`: empty-`@` renders four
  section headers in the documented order; a typed query routes
  correctly through the new tier ranking.

## 5. Non-goals

- No new top-level trigger characters. `/` (slash command), `@`
  (mention), and the existing slash-arg picker contract are unchanged.
- No empty-`@` chooser screen.
- No verbose keyword prefixes (`@file:`, `@issue:`).
- No change to mention rendering inside the buffer (atoms, protected
  ranges, URI schemes) — this spec only changes the picker.
- No change to the `set_issue_snapshot` / `set_worker_snapshot_in_place`
  cache-clearing contract.

## 6. Risks & rollback

- Phase 1 is pure ranking, behind no flag. Each commit is independently
  revertable. Worst case: a user perceives the new caps as missing data
  — mitigated by always showing the section header so empty rows are
  legible.
- Phase 2 prefix detection is additive: if the prefix character is
  never typed, behavior is identical to Phase 1. The only new failure
  mode is a user typing `@/` expecting a literal slash; the existing
  whitespace boundary on `@` already gates this, so collateral damage
  is bounded to deliberate mention contexts.
- Telemetry gate: ship Phase 1, observe whether the original
  "files buried" report disappears, and only then plan Phase 2.

## 7. Decisions log

- **2026-05-13:** Rejected per-kind triggers (`#`, `!`) on collision and
  convention-inversion grounds (kimi + gemini convergent).
- **2026-05-13:** Rejected empty-`@` chooser in favor of sectioned list
  with headers (gemini critique — layout-shift jank).
- **2026-05-13:** Approved Phase 1 / Phase 2 split — ship the ranking
  fix first, treat prefixes as an optional second layer.
- **2026-05-13:** Approved prefix mapping `@/` (files), `@#` (issues,
  GitHub-aligned), `@:` (symbols, LSP-aligned), bare `@<name>`
  (workers). Original proposal's `@!`/`@#` mapping reshuffled per
  GitHub convention.
