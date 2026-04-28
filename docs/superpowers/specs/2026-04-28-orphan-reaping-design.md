# Orphan ACP Tree Reaping — Design

## Problem

After a crash or force-quit of `spur tui`, the agent subprocess trees
spawned by `NativeAcpConnection` and the three ACP adapters survive
indefinitely. ETIME up to 1d 18h, PPID=1 (reparented to launchd on
macOS / init on Linux). 14 such orphans were observed on a single dev
machine, each holding small file FDs and idle RSS.

The cleanup *code* is correct:

- `crates/spur-acp/src/connection/native.rs:906` — `cmd.process_group(0)`
  puts each agent (and its `node` / actual-binary descendants) into its
  own pgid.
- `:900` — `kill_on_drop(true)`.
- `:349-356` — `killpg(pgid, signal)` helper using `kill -SIG -pgid`.
- `:358-378` — `Drop for NativeAcpConnection` does `killpg(pgid, "KILL")`
  if `cmd_tx` is still `Some`.
- `:1622-1651` — graceful `shutdown()`: SIGTERM → wait → SIGKILL via killpg.

The problem is that **all of this only fires when `Drop` runs.** `Drop`
does not run when:

- spur receives `SIGKILL` (kernel cannot run handlers): force-quit, OOM
  killer, `kill -9`.
- the tokio runtime aborts tasks during `Runtime::shutdown_timeout`
  before `NativeAcpConnection::drop` runs.

When that happens, the in-memory `child_pgid: Arc<Mutex<Option<i32>>>`
registry vanishes with the spur process. The agents stay alive, their
pgid is still set, but **nothing on disk records that pgid 8801 was
ever spur-owned**, so there is no reconciliation path on the next spur
boot. PID 1 (launchd / init) does *not* reap live orphans — only
zombies.

The gap is also wider than `native.rs`: three other spawn sites set
`kill_on_drop(true)` but **do not set `process_group(0)` at all**, so
`killpg` is meaningless for those agent types even when Drop does run:

- `crates/spur-acp/src/connection/stdio_adapter.rs:102-107`
- `crates/spur-acp/src/connection/stream_json_adapter.rs:178-183`
- `crates/spur-acp/src/connection/cli_wrap_adapter.rs:182-188`

A fourth site — the ACP `terminal/create` block in
`native.rs:1103-1108` — also lacks a process group, with later kill
sites at `:1263`, `:1294`, `:1605` reaching only the direct PID.

Ancillary: any `signal_hook` based shutdown handler interacts badly
with crossterm's raw mode + alternate screen (`spur-tui/src/tui.rs:13-35`).
Re-raising SIGINT before crossterm restores the terminal corrupts the
user's shell. We must route fatal signals through crossterm's event
stream so terminal teardown happens before any forced exit.

The principle being violated: *cleanup state for resources that outlive
the owner must itself outlive the owner.* Today the cleanup record dies
before the resource does.

## Solution

Persistent pgid registry + crash-safe startup sweep with identity
verification, plus closing the four spawn-site gaps. No runtime
dependency on `signal_hook`; fatal signals route through crossterm.

### Component changes

#### 1. Close the spawn-site gap — add `process_group(0)` to all four sites

Identical change at each site, immediately after the existing
`kill_on_drop(true)`:

```rust
#[cfg(unix)]
cmd.process_group(0);
```

Sites:
- `crates/spur-acp/src/connection/stdio_adapter.rs:107`
- `crates/spur-acp/src/connection/stream_json_adapter.rs:183`
- `crates/spur-acp/src/connection/cli_wrap_adapter.rs:188`
- `crates/spur-acp/src/connection/native.rs:1108` (ACP `terminal/create`)

The kill sites in `native.rs:1263, 1294, 1605` should be updated to
`killpg` (matching the helper at `:349`) once their spawned children
have a pgid.

#### 2. Durable pgid registry — `.spur/pgids/<pgid>.toml`

At spawn (each site, after `child_pgid` lock update), write:

```toml
spur_pid = 81282
spur_pid_start_time = 1745825534    # i64; canonical form below
agent_name = "claude-code"
cmd = "/opt/homebrew/bin/npm exec @anthropic-ai/claude-agent-acp@0.26.0"
pgid = 8801
pgid_leader_start_time = 1745825534
spawned_at = 1745825534              # unix epoch seconds
```

All start-time fields are `i64`. The canonical encoding is:
- **macOS:** `proc_bsdinfo.pbi_start_tvsec` (unix epoch seconds, via
  `libproc::proc_pidinfo`). Stable across daylight-saving and locale.
- **Linux:** the offset between `/proc/<pid>/stat` field 22
  (`starttime`, clock ticks since boot) and the boot epoch from
  `clock_gettime(CLOCK_BOOTTIME)`, normalized to unix epoch seconds.

`cmd` is the full command line (argv joined with spaces) — matching
what `ps -o command=` returns. argv[0] alone is too coarse: `/usr/bin/node`
is the same string for many distinct child processes.

Three identity fields (`spur_pid_start_time`, `cmd`,
`pgid_leader_start_time`) are the **anti-PID-reuse** evidence — see
sweep logic below.

On graceful `shutdown()` and `Drop` — after the `killpg` succeeds —
delete the corresponding `.toml`. Mid-write crashes can leave partial
files; the sweeper treats `Err(toml::de::Error)` as "not a valid
record, log and skip" rather than panicking.

#### 3. Startup sweep — `crates/spur-acp/src/orphan_sweeper.rs` (new)

Run once during spur startup, after `init_tracing()` and before any
new agent spawn. **Unconditional** — orphan accumulation is a defect
of spur's lifecycle, not a feature; every edition runs the sweep.

```
for entry in read_dir(.spur/pgids/) {
    let rec = match parse(entry) {
        Ok(r) => r,
        Err(_) => { warn!("skipping unparseable pgid record"); continue; }
    };

    // Step 1: is the owning spur dead?
    match ProcessInspector::starttime_of(rec.spur_pid) {
        Some(ts) if ts == rec.spur_pid_start_time => continue,  // owner alive
        _ => {} // owner dead OR PID recycled to a different process
    }

    // Step 2: is the recorded pgid leader still the same process?
    let leader_now = ProcessInspector::starttime_of(rec.pgid);
    let leader_cmd = ProcessInspector::cmd_of(rec.pgid);
    if leader_now != Some(rec.pgid_leader_start_time)
        || leader_cmd != Some(rec.cmd)
    {
        // pgid recycled or already reaped — drop the record, do NOT killpg
        remove_file(entry);
        continue;
    }

    // Step 3: safe to reap
    inspector.killpg(rec.pgid, Signal::TERM);
    sleep(250 ms);
    inspector.killpg(rec.pgid, Signal::KILL);
    remove_file(entry);
    emit SpurEvent::OrphanReaped {
        agent_name: rec.agent_name,
        pgid: rec.pgid,
        age_secs: now - rec.spawned_at,
    };
    warn!(...);
}
```

Both checks (owner alive AND leader identity match) must pass before
any signal is sent. This prevents the "innocent-process-slaughter via
PGID reuse" failure mode.

#### 4. `ProcessInspector` trait — mockable cleanup seam

Defined alongside the sweeper:

```rust
trait ProcessInspector: Send + Sync {
    /// Unix epoch seconds at which the process started. Returns
    /// `None` if no live process holds the PID.
    fn starttime_of(&self, pid: i32) -> Option<i64>;

    /// Full command line (argv joined with spaces) — matches the
    /// `cmd` recorded at spawn time. Returns `None` if PID is dead.
    fn cmd_of(&self, pid: i32) -> Option<String>;

    /// Send `sig` to every member of process group `pgid`. ESRCH /
    /// EPERM are swallowed (sweep is best-effort).
    fn killpg(&self, pgid: i32, sig: Signal);
}
```

**No `ps` shell-out.** The original revision proposed `ps -o command=,lstart=`
but `lstart=` is locale- and timezone-dependent (`Tue Apr 28 07:32:14 2026`),
making `==` comparison fragile across DST boundaries and machine
locale changes. Numeric APIs are stable.

**Production impl (one trait, two backends):**
- **macOS** — `libproc` crate. `proc_bsdinfo.pbi_start_tvsec` for
  `starttime_of`; `proc_pidinfo` + `proc_pidpath` joined with argv
  from `proc_pidargs` for `cmd_of`.
- **Linux** — `/proc/<pid>/stat` field 22 + `/proc/uptime` →
  unix epoch seconds for `starttime_of`; `/proc/<pid>/cmdline`
  (NUL-separated argv) joined with spaces for `cmd_of`.

`killpg` reuses the existing helper at `native.rs:349-356` (which
shells out to `kill`) — that path is already in production and is
not on a per-pgid hot loop, so the shell-out cost is acceptable.

**Boot latency:** because `starttime_of` and `cmd_of` are direct
syscalls / file reads (not `ps` invocations), an O(N) sweep over even
hundreds of pgid records is sub-millisecond per entry. No batching
needed.

Tests inject a `MockProcessInspector` (hand-rolled struct, not
`mockall`-generated — matching the project's existing mock pattern at
`orchestrator.rs:8534`) that drives the sweep deterministically
without spawning real processes.

#### 5. Crossterm-event-driven shutdown (no `signal_hook` dependency)

**Framing correction.** An earlier revision said this work "replaces
`signal_hook`." That was wrong: `signal_hook` is **not** in the spur
source today (zero matches across `*.rs`; only present transitively
in `Cargo.lock`). This is an **addition**, not a replacement — we
add a tokio-signal handler and a Ctrl-C key handler, neither using
`signal_hook`.

`spur-tui` already owns the crossterm event loop. Ctrl-C in raw mode
is already delivered as a key event (existing pattern at
`spur-tui/src/dashboard.rs:885-888` matching `KeyModifiers::CONTROL`
+ `KeyCode::Char('c')`). Reuse that pattern; do not re-derive.

**Key events:** `Ctrl-C` and `Ctrl-Q` route to the existing dashboard
key-handler, which on receipt:

1. Tears down crossterm raw mode + alternate screen
   (`spur-tui/src/tui.rs:27-35`).
2. Drops the `Orchestrator` (which drops every `NativeAcpConnection`,
   firing their `killpg` Drop safety net and `.toml` removal).
3. Exits cleanly via `std::process::exit(0)`.

**Signal events:** install minimal `tokio::signal::unix` handlers for
`SIGTERM`, `SIGHUP`, and `SIGQUIT`:

- `SIGHUP` is critical — closing the iTerm/Terminal tab delivers
  SIGHUP. Without a handler, the TUI exits without firing Drop and
  leaks the same orphan tree this spec is designed to prevent.
- `SIGTERM` covers `kill <pid>` (without `-9`).
- `SIGQUIT` covers Ctrl-\.

Each handler pushes a synthetic `Event::Shutdown` into a bounded
`tokio::sync::mpsc::channel(1)`. The TUI event loop selects across
the crossterm event stream and this channel; either source triggers
the same orderly-shutdown sequence above. Bounded capacity 1 is
sufficient — duplicate signals are coalesced (`try_send` returns
`Err(Full(_))` on subsequent signals, which is harmless: the first
already triggered shutdown).

`SIGKILL` of spur itself remains uncatchable; the on-startup sweep is
the safety net for that path.

#### 6. SpurEvent emission for observability

Add `SpurEventBody::OrphanReaped { agent_name, pgid, age_secs }` —
kimi's review point. The TUI dashboard already consumes `SpurEvent`
for activity logs; this surfaces the cleanup so users see it happen.
Persists into `.spur/events/` like all other events.

#### 7. Legacy orphans — accepted one-time miss

Existing orphan trees from prior crashes have no `.toml` records
(this spec introduces the registry). On first boot of a build that
includes this spec, those legacy orphans will not be reaped. Going
forward, every spawn writes a record, so the steady state is
self-healing.

This is an accepted one-time miss. Operators wanting immediate
cleanup can run `pkill -f 'codex-acp\|claude-agent-acp'` once after
upgrade. A dedicated `spur reap-orphans --legacy` subcommand is not
in scope.

### What does NOT change

- The existing `Drop` and graceful `shutdown()` killpg paths in
  `native.rs:358-378` and `:1622-1651` — they remain the fast-path.
  The sweep is a *safety net*.
- `child_pgid: Arc<Mutex<Option<i32>>>` field — kept as the
  in-memory registry; the on-disk `.toml` is the durable mirror.
- `process_group(0)` on the native ACP main spawn (`native.rs:906`) —
  already correct.
- ACP protocol semantics, agent registry, capability negotiation.

### Test plan

- **Unit**: `OrphanSweeper::run` against a `MockProcessInspector` that
  reports various states (owner alive / dead / PID-recycled / pgid
  leader changed). Each scenario asserts the right action (skip / reap
  / drop record).
- **Integration**: `process_kill_on_drop_test.rs` extended to also
  verify the `.toml` file is created at spawn and removed at graceful
  shutdown.
- **Integration**: `orphan_sweep_e2e_test.rs` (new) — spawn a sleeping
  agent, write a fake `.toml` for it, simulate a dead-spur scenario,
  run the sweeper, assert the agent was reaped and the file removed.
- **Integration**: identity-mismatch scenario — write a `.toml` whose
  recorded `pgid_leader_start_time` does NOT match the current
  inspector reading; assert the sweeper drops the record without
  signalling anything.

### Sequencing (independently shippable)

1. Add `process_group(0)` to the 4 missing spawn sites. Closes the
   biggest hole (those agent types currently leak on every shutdown).
2. Update kill sites in `native.rs:1263, 1294, 1605` to use the
   `killpg` helper so the new pgid is actually reached.
3. Pgid registry write/delete on spawn/shutdown (no sweep yet).
4. Implement `ProcessInspector` + production impl.
5. Implement `OrphanSweeper::run` and wire into spur startup.
6. Add `SpurEventBody::OrphanReaped` and dashboard surface.
7. Replace any `signal_hook` usage in TUI with crossterm-event shutdown.

Steps 1–2 are same-day low-risk fixes (close the leak source). Steps
3–7 are the design proper.

## Acceptance criteria

- [x] All four ACP-spawning sites set `process_group(0)`. (T1, commit 1fef45d0)
- [x] After `kill -9` of a running spur tui, the next spur tui startup
      reaps every orphan tree from the prior session within 1 second.
      (T9 e2e at `crates/spur-acp/tests/orphan_sweep_e2e.rs` — passes
      `cargo test -p spur-acp --test orphan_sweep_e2e -- --ignored` in 3.17s.)
- [x] If a recorded pgid has been recycled to an unrelated process, the
      sweep drops the record without sending any signal. (T6,
      `pgid_recycled_drops_record_no_kill` test in `orphan_sweeper.rs`.)
- [x] Ctrl-C, Ctrl-Q, SIGTERM, SIGHUP, and SIGQUIT all restore raw mode
      and alternate screen before exit (no terminal corruption). (T8,
      commits 0381d188 + f61241bf — signals route through `app.confirm_quit()`
      so the same loop break + `tui::teardown` runs on every path; manual
      tab-close verification deferred to release-day smoke.)
- [x] `SpurEvent::OrphanReaped` fires once per reaped tree. (T7,
      commit ee894dc5 — emitted from `spur-cli/main.rs` per killed record,
      rendered by `dashboard.rs` activity-log arm.)
- [x] Sweep runs unconditionally on every spur startup (no license gate).
      (T6 sweep block at top of `run()` in `spur-cli/main.rs:452`, runs
      before the `match cli.command` branch.)
- [x] No `signal_hook` dependency added. (Verified zero source refs;
      T8 uses `tokio::signal::unix` exclusively.)

## References

- Original analysis & two rounds of multi-gate review: this
  conversation, 2026-04-28.
- Sibling spec: `2026-04-28-log-rotation-design.md`.
- Existing pattern: `killpg` helper at `native.rs:349-356`.
- Existing Ctrl-C-as-key pattern at `dashboard.rs:885-888`.
- Followup tickets for medium/low-severity amendments: see beads
  ticket created alongside this spec.
