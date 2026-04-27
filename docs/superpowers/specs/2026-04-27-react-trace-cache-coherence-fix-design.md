# React-Trace Cache Coherence Fix (Approach D')

Date: 2026-04-27
Status: Spec — pending implementation
Owner: brain (Claude Opus 4.7) + worker:codex (adversarial review)

## Problem Statement

Ghost text appears in the brain session view (`crates/spur-tui/src/views/session_detail.rs` →
`crates/spur-tui/src/components/react_trace/`) when the user scrolls (Page Up / Page Down /
mouse wheel) and toggles `Ctrl+O` (collapse/expand of Observe entries) while text is
streaming. After the next render, the viewport snaps to a previously-rendered scroll
position; old rows reappear at unexpected vertical positions and read visually as "ghost
text".

The user-confirmed reproducer is: scroll up/down + collapse/expand toggle while streaming
is in flight. Mermaid rendering is NOT required to reproduce.

## Background — Evidence Trail

Investigation history (delegations preserved as audit surrogates because beads is not
configured in this repo):

- `f2a5f8b1` (codex) — confirmed `TraceKind::Observe { payload: Some(_) }` is never written
  by production code. Duplicate Act + Observe render hypothesis (H1) is OUT.
- `7caff7b5` (gemini) — wrote a headless test that toggles collapse/expand twice with
  `picker: None` and asserts cells outside content are empty. Test passes — confirms
  ratatui Buffer reset clears cell-grid text correctly. Toggle-only does NOT ghost.
- `93f30120` (kimi) — flagged `layout_for_scroll` (`mod.rs:580-602`) reads cache fields
  without checking `c.generation == self.generation`. HIGH severity finding.
- `b64343e8` (codex round 1) — verified the race is triggerable only on same-frame key
  bursts because the event loop drains all crossterm events before a single render
  (`app.rs:2573-2624`). Pushed back on initial Approach A as masking rather than fixing,
  proposed Approach X (drop caches in invalidate_cache).
- User clarification — no mermaid required (rules out M1: image protocol residue);
  reproducer is "scroll + toggle while streaming". This confirms the race is exercised
  by streaming `mark_dirty_from(idx)` calls, not just toggle.
- `b3f993dd` (codex round 2) — accepted Approach D's architecture, demanded three concrete
  corrections (two missed `last_surface` set sites + test-helper update + non-trivial
  test design).

## Root Cause

`ReactTrace` keeps three caches gated by a manual `generation: u64` counter:
- `line_cache` (markdown brain-view virtual rows)
- `compact_cache` (DetailPane compact body)
- `body_cache` (DetailPane Stream tab)

Every mutation that changes the cached projection bumps `generation`:
- `invalidate_cache` (`mod.rs:330-333`) — sets `dirty_from = Some(0)`; called by
  `toggle_observe_collapsed` and other surface-wide invalidators.
- `mark_dirty_from(idx)` (`mod.rs:336-339`) — sets `dirty_from = min(prev, idx)`; called
  by streaming append paths (`append_message` at `mod.rs:400-405`, `mod.rs:428-431`) and
  `push` (`mod.rs:493-521`).

`render_with_ctx` rebuilds the cache before reading from it (`render.rs:454-512`), so
render-path reads are always fresh. But `layout_for_scroll` (`mod.rs:580-602`) — used by
`shift_anchor_by` (`mod.rs:618-648`), called by Page Up / Page Down / mouse wheel —
reads `entry_row_starts.clone()` and `c.rows.len()` without comparing
`c.generation == self.generation`. Between any generation bump and the next render, a
scroll event mutates the anchor against stale layout coordinates. The next render
rebuilds with new layout; `resolve_anchor` clamps the now-misaligned `Row { entry_idx,
row_within_entry }`, snapping the viewport. The visual effect is old rows appearing at
unexpected positions.

The asymmetric staleness predicate between `render_with_ctx` (always rebuilds first) and
`layout_for_scroll` (reads without checking) is the bug class. Comments in the codebase
confirm this class has been patched multiple times (`streaming_tests.rs:1`,
`markdown_stream.rs:326`, `streaming_tests.rs:515`, `streaming_tests.rs:1473`). Each
prior fix patched a symptom; this design encodes the staleness predicate in the type
system so the bug class cannot recur in the same shape.

## Design — Approach D' (Generation-Bearing Surface)

### Type change

`Surface` (today at `mod.rs:36-45`) gains a generation snapshot per painted variant:

```rust
pub(super) enum Surface {
    None,
    Full(u64),     // generation when last painted
    Compact(u64),  // generation when last painted
}
```

### Read path

`layout_for_scroll` (`mod.rs:580-602`) uses match guards to drop stale snapshots:

```rust
fn layout_for_scroll(&self) -> Option<(Vec<usize>, usize)> {
    match self.last_surface {
        Surface::None => None,
        Surface::Compact(g) if g == self.generation => self
            .compact_cache
            .as_ref()
            .map(|c| (c.entry_row_starts.clone(), c.lines.len())),
        Surface::Full(g) if g == self.generation => {
            #[cfg(feature = "markdown")]
            {
                self.line_cache
                    .as_ref()
                    .map(|c| (c.entry_row_starts.clone(), c.rows.len()))
            }
            #[cfg(not(feature = "markdown"))]
            {
                None
            }
        }
        _ => None,
    }
}
```

When the snapshot is stale, the function returns `None`, so `shift_anchor_by` early-returns
without mutating the anchor (`mod.rs:621-623`). The next render rebuilds the cache and
stamps a fresh snapshot; the next scroll keystroke applies correctly.

### Set sites (every painter stamps current generation)

Four sites set `last_surface`. All must stamp `self.generation`:

| File | Line | Variant |
|---|---|---|
| `mod.rs` | 216 (init) | `Surface::None` (no change) |
| `mod.rs` | 1253 (`seed_line_cache_for_tests`) | `Surface::Full(self.generation)` |
| `render.rs` | 406 (non-markdown render) | `Surface::Full(self.generation)` |
| `render.rs` | 608 (markdown render) | `Surface::Full(self.generation)` |
| `compact_render.rs` | 99 (compact render) | `Surface::Compact(self.generation)` |

`invalidate_cache` (`mod.rs:330-333`) is unchanged. Generation comparison in the read
path handles staleness without explicit `last_surface` reset.

### Why D' over alternatives

| Approach | Lines | Type-enforced gen check | Preserves incremental cache | Scope |
|---|---|---|---|---|
| A (gen-filter on read site) | 5 | no — convention | yes | local |
| X (drop caches + Surface::None) | 4 | no — set-site discipline | NO — drops cache contents | local |
| **D' (Surface carries generation)** | **~14** | **yes** | **yes** | **local** |
| B (pending_scroll buffer) | ~100+ | yes | yes | architectural |

D' encodes the staleness predicate in the type. Future cache readers cannot accidentally
skip the gen check because the variants force pattern-deconstruction with the embedded
`u64`. X loses incremental cache value during streaming (rebuilds from cold every
generation bump). B is over-engineered for current evidence.

## Test Plan

New file `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`, gated
`#[cfg(test)]`. Module declared from `mod.rs` next to existing test modules.

### Test 1 — generation-mismatch causes scroll no-op

```text
1. Build ReactTrace, seed >viewport-size entries.
2. INITIAL render via render_with_ctx (stamps last_surface = Full(g0)).
3. Capture anchor.
4. mark_dirty_from(idx) — simulates streaming chunk; bumps generation to g1.
5. shift_anchor_by(-50).
6. Assert anchor is unchanged (no-op because last_surface = Full(g0) ≠ self.generation).
7. Render again (last_surface = Full(g1)).
8. shift_anchor_by(-50).
9. Assert anchor moved this time.
```

The test must include step 2 (initial render) — without it, `last_surface = Surface::None`
and the no-op assertion would pass trivially regardless of D'.

### Test 2 — toggle is also covered

Same shape as Test 1, but step 4 is `toggle_observe_collapsed()` instead of
`mark_dirty_from`. Asserts that the toggle also bumps generation and produces no-op scroll
behavior until next render.

### Test 3 — compact surface symmetry

Same shape, but uses `render_compact` (which stamps `Surface::Compact(g)`). Confirms the
match guard in `layout_for_scroll`'s `Surface::Compact` arm fires on stale generation.

## Verification Commands

After implementation:

```bash
cargo check -p spur-tui --features markdown
cargo clippy -p spur-tui --features markdown -- -D warnings
cargo test -p spur-tui --features markdown
cargo test -p spur-tui --features markdown scroll_race_test
```

All four must exit zero. Existing tests in `streaming_tests.rs` must continue to pass.

## Trade-offs

### Accepted: same-frame keystroke drop

If a user holds Page Up while a streaming chunk arrives in the same crossterm event drain
(`app.rs:2573-2578`), the first scroll keystroke after a generation bump is a no-op.
The next frame paints the new layout and stamps a fresh snapshot; the next keystroke
applies correctly. Held-key scrolling stutters by at most one tick (~30ms at 30Hz
streaming render). Imperceptible in practice. Approach B (pending_scroll buffer) would
preserve the keystroke at ~100 lines + a new "deferred scroll" concept; rejected as
over-engineered for current evidence.

### Accepted: bug class is not eliminated, only this shape is

Future contributors adding a new cache reader that bypasses `layout_for_scroll` could
reintroduce the bug. D' mitigates this by requiring the new reader to deconstruct the
`Surface::*(u64)` variant, forcing consideration of the staleness check. A stronger
mitigation (e.g. `Cached<K, V>` newtype) is deferred until a second instance of the bug
class appears.

## Out of Scope

- M1 (Kitty/Sixel image protocol residue) — ruled out by user clarification ("no, even
  with no mermaid render, ghost image still there"). If image-residue is observed in a
  future report, open a separate spec.
- Approach B (pending_scroll buffer) — rejected as YAGNI per current evidence.
- Cache-coherence audit of `body_cache` (`mod.rs:798-819`) — already keyed correctly on
  `(generation, width)`. No change needed.

## Acceptance Criteria

1. `Surface` enum carries generation snapshot in `Full` and `Compact` variants.
2. All four set sites stamp `self.generation`.
3. `layout_for_scroll` returns `None` when the snapshot generation does not match
   `self.generation`.
4. `invalidate_cache` is unchanged (no `last_surface` reset added).
5. Three new tests pass (generation-mismatch no-op for stream / toggle / compact).
6. All existing tests in `crates/spur-tui` pass.
7. `cargo clippy --features markdown -- -D warnings` is clean.
8. No new comments explaining what the code does — only WHY-comments where the
   invariant is non-obvious (e.g. why `Surface` carries `u64`).
