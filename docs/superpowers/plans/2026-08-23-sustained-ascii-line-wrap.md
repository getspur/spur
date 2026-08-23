# Sustained ASCII Line Wrap Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `beads://bd-3djz` (approved Z3Opt decision and measured profile evidence)
**Formal @spec cells (if notebook):** none
**Design epic:** `bd-3djz` (closed)

**Goal:** Reduce sustained transcript render CPU by bypassing the generic `(Style, char)` flattening path for supported printable-ASCII span streams.

**Architecture:** Extend the existing ReactTrace simulation to model the eight-event paint cap and use test-only path counters on the real render journey. Dispatch to a direct ASCII span walker only when the measured eligibility gate is met; every unsupported width, empty input, control byte, non-ASCII byte, and semantic mismatch falls back to the unchanged generic walker.

**Tech Stack:** Rust 2021, ratatui `Line`/`Span`, `scripts/spur-cargo`, Z3 Optimize through SPUR solve, macOS `/usr/bin/time -l`, and the existing profiling build.

---

### Task 1: Gate and implement printable-ASCII span streaming

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-tui/src/components/line_wrap.rs`
- Modify: `crates/spur-tui/examples/react_trace_bench_sim.rs`
- Create: `docs/superpowers/plans/2026-08-23-sustained-ascii-line-wrap.md`

**Depends on:** none (the approved source decision is beads dependency `bd-3djz`)

**Acceptance Criteria:**
- [ ] SOLVE PRE persists the 818-permille minimum and proves unsupported inputs cannot select the fast path.
- [ ] A RED commit contains the sustained eight-appends-per-draw journey and fails because eligible calls still use the generic path.
- [ ] The pre-change journey measures at least 818 eligible calls per 1000 observed wrapping calls; otherwise no production fast path is shipped.
- [ ] Printable-ASCII multi-span output, styles, whitespace handling, and character starts match the generic reference.
- [ ] Width zero, empty input, tabs/control bytes, non-ASCII, and any future unsupported shape retain generic behavior.
- [ ] The same local profiling binary/workload records before/after wall, CPU, and peak RSS measurements.
- [ ] SOLVE POST re-evaluates the implemented guard and measured eligibility.
- [ ] Targeted tests, `spur-tui` checks, formatting, and scoped diff verification pass.

**Suggested Worker:** self (the user explicitly requested inline execution with no delegation)

**Scope Boundary:**
- IN scope: the line-wrap dispatcher/ASCII helper, test-only path counters and equivalence tests, the sustained simulation batch size/reporting, and this plan.
- OUT of scope: event coalescing, `DRAIN_CAP_PER_FRAME`, Markdown scanner state, virtual-row caching, rendering layout, and other crates.
- If the eligibility gate fails or another file is required, stop and record the decision on `bd-2sy6` before expanding scope.

**Implementation:**

- [ ] **Step 1: Persist SOLVE PRE before RED**

Derive the discrete gate by minimizing `eligibility_permille` subject to:

```text
0 <= eligibility_permille <= 1000
93 * eligibility_permille >= 76 * 1000
```

Prove the fail-closed selection invariant by asking for a counterexample to:

```text
fast_path <-> positive_width && nonempty_spans && printable_ascii
fast_path -> positive_width && nonempty_spans && printable_ascii
```

Expected: threshold SAT/complete at `818`; unsupported counterexample UNSAT.

- [ ] **Step 2: Write the failing sustained-journey test and benchmark batch**

Use the real ReactTrace renderer and eight appends per draw. Test-only counters distinguish observed, eligible, and fast-path calls:

```rust
for frame in 0..STREAM_FRAMES {
    for chunk in 0..8 {
        trace.append_message(" +token", streaming_agent, timestamp(frame * 8 + chunk));
    }
    harness.draw(&mut trace);
}

assert!(stats.ascii_eligible * 1000 >= stats.observed * 818);
assert_eq!(stats.ascii_fast_path_hits, stats.ascii_eligible);
```

- [ ] **Step 3: Run RED and commit it separately**

Run:

```bash
SPUR_REMOTE=0 scripts/spur-cargo test -p spur-tui sustained_eight_append_journey_uses_ascii_fast_path -- --nocapture
```

Expected: FAIL because eligibility is measured but `ascii_fast_path_hits == 0`.

Commit:

```bash
git commit -m "test(spur-tui): bd-2sy6 cover sustained ASCII wrapping"
```

- [ ] **Step 4: Capture the unchanged baseline**

Build and time the profiling example with an isolated target directory:

```bash
SPUR_REMOTE=0 CARGO_TARGET_DIR=/tmp/spur-bd-2sy6-target scripts/spur-cargo build --profile profiling -p spur-tui --example react_trace_bench_sim
/usr/bin/time -l /tmp/spur-bd-2sy6-target/profiling/examples/react_trace_bench_sim --quick
```

Preserve the pre-change binary before the GREEN build and use multiple repetitions for medians.

- [ ] **Step 5: Add the minimal fail-closed dispatcher**

Keep the current walker as the generic reference. The dispatcher uses the ASCII helper only when every span byte is printable ASCII and width/input guards hold:

```rust
if printable_ascii_span_stream_eligible(line, width) {
    return wrap_printable_ascii_spans(line, width, starts);
}
wrap_line_to_width_generic(line, width, starts)
```

The helper walks `(span_index, byte_index)` positions without allocating a `(Style, char)` vector. Output is built directly from source byte ranges, adjacent equal styles are merged, and returned starts remain source character indices (equal to byte indices for ASCII).

- [ ] **Step 6: Verify GREEN and semantic equivalence**

Add table-driven equivalence cases for widths, whitespace runs, long words, multiple styles, and character-start metadata. Explicitly assert fallback equivalence for tabs, control bytes, Unicode, empty lines, and width zero.

Run:

```bash
SPUR_REMOTE=0 scripts/spur-cargo test -p spur-tui line_wrap -- --nocapture
SPUR_REMOTE=0 scripts/spur-cargo test -p spur-tui sustained_eight_append_journey_uses_ascii_fast_path -- --nocapture
```

- [ ] **Step 7: Re-measure and persist SOLVE POST**

Rebuild with the same profile/target/workload, record wall/user/sys/peak RSS, and compare medians. Re-run the threshold constraint with measured eligibility and the unsupported-input counterexample against the implemented guard.

- [ ] **Step 8: Verify and commit the production change**

Run formatting, targeted/full crate checks, inspect the scoped diff, and commit:

```bash
git commit -m "refactor(spur-tui): bd-2sy6 stream printable ASCII wrapping"
```

**Scope Drift Checkpoint:**
- If measured eligibility is below 818 permille, keep the RED measurement commit, do not add production code, and record a pivot to reuse-miss instrumentation.
- If preserving semantics requires touching files outside the three listed above, stop and amend the plan/bead first.
- If the after profile does not move in proportion to the modeled 93/585-sample opportunity, do not stack another optimization in this task.
