---
name: performance-investigation
description: "Use when a command/binary is reported as slow, when profiling output drives an optimization decision, or before committing to a perf fix bigger than a one-line refactor. Establishes a measure→localize→quantify→fix loop that uses real profile data instead of a mental model."
role: both
---

# Performance Investigation — Profile, Don't Guess

The model of "where the time goes" is almost always wrong about the magnitudes, even when the call sites are right. Before any perf change wider than a one-line refactor, get a real measurement. Profiles routinely surprise: subprocess overhead masquerading as serde, tree-sitter looking like a bottleneck that contributes 6% of CPU, page-fault storms hiding behind innocent allocators.

<HARD-GATE>
Do not propose a perf fix that you cannot tie to a specific number from a profile, a `time` output, or a microbenchmark. "It looks like X is slow" is not enough. Capture first, then decide.
</HARD-GATE>

## The four-step loop

```
1. TIME — measure baseline wall + sys + user. One number, two minutes of work.
2. PROFILE — capture a flamegraph against the actual slow command, cold state.
3. QUANTIFY — split on-CPU vs off-CPU. Read inclusive stacks, not just leaves.
4. FIX ONE THING — make the smallest change the profile justifies. Re-profile.
```

Never skip step 1. Never go past step 2 without step 1's number to compare against.

## Step 1 — Time before you profile

```bash
/usr/bin/time -l <command>                     # macOS, BSD time format
/usr/bin/time -v <command>                     # GNU time
```

What to read out of the output:

| Field | Meaning | What it tells you |
|---|---|---|
| `real` | Wall-clock time | Total elapsed. The number users care about. |
| `user` | CPU time in your process | On-CPU compute (your code + libraries). |
| `sys` | CPU time in the kernel on your behalf | Syscalls. **High sys with high real → subprocess churn, page faults, or heavy IO.** |
| `page reclaims` / `page faults` | Memory pressure | Many reclaims = lots of allocation churn or fresh mmaps (subprocess spawns trigger these). |
| `involuntary context switches` | Pre-emption | High under CPU contention; in single-binary workloads, high count + low CPU = waiting on a child process to do something. |

The wall − (user + sys) gap is **off-CPU time**: waiting on IO, subprocess return, locks. If that gap is most of the wall, no amount of optimizing on-CPU paths will help — go after the off-CPU cause directly (usually subprocess synchronization, network, or disk).

## Step 2 — Profile

Pick a profiler that fits the platform AND doesn't change the problem. Pick the one with the lowest setup cost that gives you accurate stacks.

| Platform | First choice | Fallback | Avoid |
|---|---|---|---|
| **macOS** | `samply record` (no sudo, web UI) | `cargo flamegraph --root` (needs sudo for dtrace) | `cargo flamegraph` without sudo (silently produces empty profile) |
| **Linux** | `perf record` → flamegraph | `cargo flamegraph` (wraps perf) | Userspace-only samplers when you suspect syscall cost |
| **Container/CI** | `samply` (single binary) | `perf` if `--cap-add=SYS_ADMIN` available | Anything needing GUI |

### Build with debug info, then profile release

Stripped release binaries produce flamegraphs full of `0x806c` addresses, not function names. Either:

- Add `[profile.release] debug = "line-tables-only"` to `Cargo.toml` (small binary size cost, full debugging info preserved), OR
- Use a separate `[profile.release-with-debug]` so prod stays unaffected, OR
- Symbolicate via the profiler's HTTP API after recording (samply exposes `/symbolicate/v5`).

### Cold state matters

Caches lie. Before profiling, **wipe every persistence layer the command would otherwise hit**:

```bash
# Example: a build that caches into both .spur/graph (working) and .git/spur-graph (canonical)
rm -rf .spur/graph .git/spur-graph
# Then profile.
```

If the second run is 100× faster than the first, you measured the wrong thing. Always confirm `mode: Full` (or whatever your tool prints for "no incremental shortcut taken") in the workload's own output before trusting the profile.

### Samply quick-start (macOS, no sudo)

```bash
cargo install samply
samply record --save-only --no-open --rate 4000 -o /tmp/profile.json.gz \
  ./target/release/<binary> <args>
samply load /tmp/profile.json.gz --no-open   # serves UI at http://127.0.0.1:3000/...
```

Open the URL in your browser. The Firefox Profiler frontend symbolicates via samply's HTTP server — names appear automatically.

### Cargo-flamegraph quick-start (Linux or macOS+sudo)

```bash
cargo flamegraph --root --bin <name> --release -- <args>     # macOS, sudo for dtrace
cargo flamegraph --bin <name> --release -- <args>            # Linux, perf
```

Output: `flamegraph.svg` in CWD.

## Step 3 — Quantify

### Split on-CPU vs off-CPU

```
on_cpu_seconds  ≈ total_samples / sample_rate_hz
off_cpu_seconds ≈ wall_real - on_cpu_seconds
```

A 15 s wall command with 4 s on-CPU is **11 s off-CPU**. If the profiler only shows on-CPU stacks (samply's default, perf without `--call-graph dwarf -F 999 --switch-events`), you are seeing only ~26% of where the time went. Off-CPU dominance means: **subprocess fork/wait, network IO, disk IO, or lock contention**. Different tooling, different fix.

### Read inclusive frames, not just leaf names

Leaf names lie under LTO. Rust's link-time optimization and identical-code folding (ICF) merge function ranges, and the profiler's symbolicator picks one name for an address that originally belonged to several. Empirically: a samply profile attributed 7,000+ samples to `serde::TaggedContentVisitor::visit_map` — when the inclusive stack showed every one of those samples was inside `Command::spawn` / `read_output` called from `cat_file_blob`. The leaf was mislabeled by ~merged symbol ranges.

**Rule: if the leaf name surprises you, walk up the inclusive stack before believing it.** Same hex address can resolve to different names depending on the symbolication request shape; the call chain doesn't lie.

### Numbers to write down before deciding the fix

| Metric | Why it matters |
|---|---|
| Wall time (baseline) | The user-visible number. Track this across fixes. |
| On-CPU vs off-CPU split | Distinguishes "compute is slow" from "we're waiting." Different fix universe. |
| Inclusive % for the top 3 functions | Where the compute (or wait) actually lives. |
| Subprocess count if relevant | `strace -c` (Linux), `dtruss` (macOS, needs sudo), or static analysis of `Command::new` call sites. ~3 ms fork+exec each on darwin. |
| Repo/workload scale | N commits, M files, etc. Speedup predictions depend on these. |

## Step 4 — Fix one thing

Rank candidate fixes by **(measured impact ÷ implementation cost)**. Land the smallest fix the profile justifies, then re-profile.

Why one at a time:
- A single fix shifts the next bottleneck. The fix you'd have done second often becomes irrelevant after the first.
- Stacking two unverified fixes makes regressions impossible to attribute.
- Each re-profile cycle is ~5 minutes; cheaper than reasoning your way through an interaction matrix.

After each fix, re-record under the same cold conditions and the same input. Confirm the wall-time drop matches your prediction within ~30%. If it doesn't, the model was wrong and the next fix needs reframing.

## Anti-patterns

| Thought / action | Reality |
|---|---|
| "I know what's slow, let me just optimize it." | Profiles are surprising. Always measure first; the model is wrong about magnitudes more often than not. |
| "Tree-sitter parsing must be the bottleneck." | Routinely <10% of CPU even on parser-heavy workloads. Subprocess spawning and IO usually dominate. |
| "The leaf says serde, so it's a deserialization problem." | Verify with the inclusive stack. LTO / ICF makes leaf names lie. |
| "I'll profile a warm run because cold is too slow to wait for." | Warm runs hide the actual bottleneck behind cached results. Always profile the cold path the user complains about. |
| "Sample rate 1000 Hz is fine." | For sub-15-second commands, bump to 4000 Hz. Otherwise top frames have too few samples to rank confidently. |
| "I'll do all three optimizations at once." | Stacked unverified fixes make regressions impossible to attribute. Land + re-profile each. |
| "I'll just `time` it without `-l` / `-v`." | Default `time` output (real/user/sys only) loses page-fault and context-switch counts that diagnose subprocess churn. |
| "Profile doesn't have function names, must rebuild with debug = true." | Symbolicate via the profiler's HTTP API first; rebuilding is a 9-minute detour. |
| "I'll trust `cargo flamegraph` output on macOS without `--root`." | Without sudo it silently produces a near-empty SVG. Always use `--root` on macOS, or switch to samply. |
| "On-CPU dominates so the fix is to optimize compute." | Check the wall − on-CPU gap first. If off-CPU is most of wall, optimizing compute is mis-aimed. |
| "I rebuilt and re-ran but the new run was faster, so the fix worked." | Did you wipe caches? Confirm the workload printed the same "full / cold" indicator in both runs before crediting the change. |

## When the bottleneck IS off-CPU

Off-CPU dominance is the most common surprise. Symptoms:

- High `sys` time (`/usr/bin/time -l`).
- High `involuntary context switches`.
- High `page reclaims` (each fork/exec touches new pages).
- Low total on-CPU samples relative to wall time.

Common causes, in rough order of frequency in CLI tools:

1. **Subprocess fan-out** — repeated `Command::new("git")` / `pg_*` / `curl` invocations. Each is ~3–5 ms fork+exec on macOS, ~1–2 ms on Linux. At 1,000 spawns, that's a wall-time floor you cannot optimize past without consolidating.
2. **Synchronous IO** — file reads/writes without buffering, or per-row DB queries.
3. **Lock contention** — `Mutex::lock` waiting under high concurrency.
4. **Network round-trips** — RPC fan-out without batching.

Fix shapes for (1):
- Long-lived child process driven over a pipe (e.g. `git cat-file --batch`).
- In-process library binding (`gix`, `git2`, `rusqlite`, etc.).
- Batch the input so one spawn covers many items.

## TL;DR

```
1. /usr/bin/time -l <cmd>           # wall + sys + user. Don't skip.
2. Cold state. Wipe caches. Confirm the workload says "full" / "miss" / etc.
3. samply record (mac) | perf / cargo flamegraph --root (linux).
   Rate ≥ 4000 Hz for sub-15s commands.
4. Compute on-CPU vs off-CPU split.
   - Off-CPU dominant → fix the wait (subprocess / IO / locks).
   - On-CPU dominant  → fix the compute (the top inclusive frame).
5. Walk inclusive stacks before trusting any surprising leaf name (LTO lies).
6. Land the smallest fix the profile justifies. Re-profile. Repeat.
```
