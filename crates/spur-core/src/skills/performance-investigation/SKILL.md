---
name: performance-investigation
description: "Use when a command/binary is reported as slow, when profiling output drives an optimization decision, or before committing to a perf fix bigger than a one-line refactor. Establishes a measure, localize, quantify, fix loop that uses real profile data instead of a mental model."
role: both
---

# Performance Investigation - Profile, Don't Guess

The model of "where the time goes" is often wrong about magnitudes, even when
the call sites are right. Before any perf change wider than a one-line
refactor, get a real measurement. Profiles routinely surprise: subprocess
overhead masquerading as serde, tree-sitter looking like a bottleneck that
contributes a small fraction of CPU, or page-fault storms hiding behind
innocent allocators.

<HARD-GATE>
Do not propose a perf fix that you cannot tie to a specific number from a
profile, a `time` output, or a microbenchmark. Capture first, then decide.
</HARD-GATE>

## The Four-Step Loop

1. TIME - measure baseline wall, sys, and user time.
2. PROFILE - capture a flamegraph against the actual slow command.
3. QUANTIFY - split on-CPU vs off-CPU and read inclusive stacks.
4. FIX ONE THING - make the smallest change the profile justifies, then
   re-profile.

Never skip the baseline. Never go past profiling without a wall-clock number
to compare against.

## Time Before Profiling

```bash
/usr/bin/time -l <command>  # macOS
/usr/bin/time -v <command>  # GNU/Linux
```

Read wall time first. Then compare user and sys time against wall time. The
wall minus user plus sys gap is off-CPU time: waiting on IO, subprocesses,
locks, or the network. If that gap dominates, optimizing on-CPU code will not
move the user-visible number enough.

## Profile

Pick the lowest-friction profiler that gives accurate stacks:

- macOS: `samply record` first; `cargo flamegraph --root` if needed.
- Linux: `perf record` or `cargo flamegraph`.
- Container/CI: `samply` or `perf` when available.

Build release binaries with enough debug info to symbolize stacks. Profile the
cold path the user actually reports, not a warmed cache path.

## Quantify

Record these before deciding on a fix:

- Baseline wall time.
- On-CPU vs off-CPU split.
- Inclusive percentage for the top frames.
- Subprocess count if command spawning appears in the stack.
- Workload scale, such as commits, files, rows, or bytes processed.

Read inclusive stacks before trusting surprising leaf names. LTO and identical
code folding can make leaf symbols misleading; the call chain is more reliable.

## Fix One Thing

Rank candidate fixes by measured impact divided by implementation cost. Land
the smallest change the profile justifies, then re-profile under the same cold
conditions. If the wall-time drop does not match the prediction, update the
model before making another change.

## Anti-Patterns

- "I know what's slow" without a measurement.
- Profiling a warm run when users complain about cold starts.
- Trusting a surprising leaf frame without checking its inclusive callers.
- Stacking several optimizations before re-measuring.
- Optimizing compute when off-CPU time dominates.

## TL;DR

```text
1. Measure wall/user/sys with time.
2. Profile the cold path.
3. Split on-CPU and off-CPU time.
4. Read inclusive stacks.
5. Fix one measured bottleneck.
6. Re-profile before continuing.
```
