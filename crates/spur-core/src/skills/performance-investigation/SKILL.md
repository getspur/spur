---
name: performance-investigation
description: "Use when a command/binary is reported as slow, when profiling output drives an optimization decision, or before committing to a perf fix bigger than a one-line refactor. Establishes a measure->localize->quantify->fix loop that uses real profile data instead of a mental model."
role: both
---

# Performance Investigation - Profile, Don't Guess

The model of "where the time goes" is almost always wrong about the magnitudes,
even when the call sites are right. Before any perf change wider than a one-line
refactor, get a real measurement. Profiles routinely surprise: subprocess
overhead masquerading as serde, tree-sitter looking like a bottleneck that
contributes 6% of CPU, page-fault storms hiding behind innocent allocators.

<HARD-GATE>
Do not propose a perf fix that you cannot tie to a specific number from a
profile, a `time` output, or a microbenchmark. "It looks like X is slow" is not
enough. Capture first, then decide.
</HARD-GATE>

## The four-step loop

```
1. TIME - measure baseline wall + sys + user. One number, two minutes of work.
2. PROFILE - capture a flamegraph against the actual slow command, cold state.
3. QUANTIFY - split on-CPU vs off-CPU. Read inclusive stacks, not just leaves.
4. FIX ONE THING - make the smallest change the profile justifies. Re-profile.
```

Never skip step 1. Never go past step 2 without step 1's number to compare
against.

## Step 1 - Time before you profile

```bash
/usr/bin/time -l <command>                     # macOS, BSD time format
/usr/bin/time -v <command>                     # GNU time
```

What to read out of the output:

| Field | Meaning | What it tells you |
|---|---|---|
| `real` | Wall-clock time | Total elapsed. The number users care about. |
| `user` | CPU time in your process | On-CPU compute (your code + libraries). |
| `sys` | CPU time in the kernel on your behalf | Syscalls. High sys with high real usually means subprocess churn, page faults, or heavy IO. |
| `page reclaims` / `page faults` | Memory pressure | Many reclaims means lots of allocation churn or fresh mmaps. |
| `involuntary context switches` | Pre-emption | High count plus low CPU often means waiting on a child process or other off-CPU work. |

The wall minus (user + sys) gap is off-CPU time: waiting on IO, subprocess
return, locks, or network. If that gap is most of the wall, no amount of
optimizing on-CPU paths will help.

## Step 2 - Profile

Pick a profiler that fits the platform and does not change the problem. Pick
the one with the lowest setup cost that gives you accurate stacks.

| Platform | First choice | Fallback | Avoid |
|---|---|---|---|
| macOS | `samply record` | `cargo flamegraph --root` | `cargo flamegraph` without sudo |
| Linux | `perf record` -> flamegraph | `cargo flamegraph` | Userspace-only samplers for suspected syscall cost |
| Container/CI | `samply` | `perf` if available | Anything needing a GUI |

### Build with debug info, then profile release

Stripped release binaries produce flamegraphs full of addresses, not function
names. Either add line-table debug info to release builds, use a separate
profile with debug info, or symbolicate through the profiler after recording.

### Cold state matters

Caches lie. Before profiling, wipe every persistence layer the command would
otherwise hit:

```bash
rm -rf .spur/graph .git/spur-graph
```

If the second run is dramatically faster than the first, you measured the wrong
thing. Confirm the workload reports the same cold/full mode before trusting a
profile.

### Samply quick-start

```bash
cargo install samply
samply record --save-only --no-open --rate 4000 -o /tmp/profile.json.gz \
  ./target/release/<binary> <args>
samply load /tmp/profile.json.gz --no-open
```

### Cargo-flamegraph quick-start

```bash
cargo flamegraph --root --bin <name> --release -- <args>     # macOS
cargo flamegraph --bin <name> --release -- <args>            # Linux
```

## Step 3 - Quantify

### Split on-CPU vs off-CPU

```
on_cpu_seconds  ~= total_samples / sample_rate_hz
off_cpu_seconds ~= wall_real - on_cpu_seconds
```

A 15 s wall command with 4 s on-CPU is 11 s off-CPU. If the profiler only shows
on-CPU stacks, you are seeing only part of where the time went.

### Read inclusive frames, not just leaf names

Leaf names lie under LTO. Rust's link-time optimization and identical-code
folding can merge function ranges, and the profiler's symbolicator picks one
name for an address that originally belonged to several. If the leaf name
surprises you, walk up the inclusive stack before believing it.

### Numbers to write down before deciding the fix

| Metric | Why it matters |
|---|---|
| Wall time | User-visible baseline. |
| On-CPU vs off-CPU split | Distinguishes compute from waiting. |
| Inclusive percent for the top frames | Shows where time actually lives. |
| Subprocess count if relevant | Repeated spawns create a wall-time floor. |
| Repo/workload scale | Speedup predictions depend on size. |

## Step 4 - Fix one thing

Rank candidate fixes by measured impact divided by implementation cost. Land
the smallest fix the profile justifies, then re-profile. A single fix shifts the
next bottleneck; stacked unverified fixes make regressions hard to attribute.

## Anti-patterns

| Thought / action | Reality |
|---|---|
| "I know what's slow." | Profiles routinely surprise. Measure first. |
| "Tree-sitter parsing must be the bottleneck." | Parser cost is often smaller than subprocess or IO cost. |
| "The leaf says serde." | Verify with the inclusive stack. |
| "I'll profile a warm run." | Warm runs hide cold-path bottlenecks. |
| "Sample rate 1000 Hz is fine." | For short commands, use a higher rate. |
| "I'll do all three optimizations at once." | Land and re-profile one fix at a time. |

## When the bottleneck is off-CPU

Off-CPU dominance is common. Symptoms include high `sys` time, high involuntary
context switches, many page reclaims, or low on-CPU samples relative to wall
time.

Common causes:

1. Subprocess fan-out through repeated `Command::new(...)`.
2. Synchronous IO or per-row database queries.
3. Lock contention.
4. Network round trips.

Common fixes for subprocess fan-out:

- Long-lived child process driven over a pipe.
- In-process library binding.
- Batched input so one spawn covers many items.

## TL;DR

```
1. /usr/bin/time -l <cmd> or /usr/bin/time -v <cmd>.
2. Wipe caches and confirm cold/full mode.
3. Record with samply, perf, or cargo flamegraph.
4. Compute on-CPU vs off-CPU split.
5. Walk inclusive stacks before trusting surprising leaf names.
6. Land the smallest measured fix. Re-profile. Repeat.
```
