# SPUR WorktreeAuthority — Lease-Aware Lifecycle for Workspace-Global Worktrees

**Status:** Approved (post-MCTS-round-6, peer-reviewed by codex + gemini)
**Author:** brain (synthesized from `crates/spur-worktree/`, `crates/spur-blob-store/`, `crates/spur-core/src/orchestrator.rs`, and two rounds of parallel review)
**Closes:** Risk #4 (`docs/architecture.md:594` — worktree orphaning on unclean shutdown)
**Hard prerequisite:** Phase 0a (`kill_on_drop` on worker process spawn). Without it, the design is unsafe.

---

## 1. Problem

`spur-worktree` leaks every git resource it creates whenever the orchestrator does not exit cleanly. The leak is empirically active in this very repo:

| Metric | Value | Source |
|---|---|---|
| `.spur/worktrees/<session>` directories | 34 | `du -sh` |
| Disk consumed | 10 GB | filesystem |
| `spur/worker-*` branches | ~800 | `git branch --list` |
| `.git/worktrees/<id>` admin entries | 45 | filesystem |
| `spur/brain-snapshot-*` branches | ~265 | `git branch --list` |
| Empirical orphan rate | ~4/day, ~1.4 GB/day, ~42 GB/month | Apr 19 → Apr 26 |

Three simultaneous root-cause defects produce this leak:

**RC1 — No process-lifetime owner.** `Orchestrator` declares `pub worktrees: WorktreeManager` at `crates/spur-core/src/orchestrator.rs:792`, but `git grep '\.worktrees\.'` against that struct returns zero matches. The field is constructed at `:962` only to derive `repo_root` for the blob store, then never read. Every operational manager is created fresh per-delegation at `:4248` (`let mut worktrees = WorktreeManager::new(repo_root)`). When that delegation task ends, the manager drops — the worktree directory and branches survive.

**RC2 — `cleanup_orphans()` is dead code, and a naive fix is destructive.** `crates/spur-worktree/src/manager.rs:437` defines `cleanup_orphans` but it has zero call sites in the workspace. The obvious fix — `Arc<WorktreeManager>` shared via DashMap, sweep at boot — is **actively destructive** in SPUR's actual deployment model. The `fs4` session-attach lock at `crates/spur-acp/src/session_lock.rs` is **per-brain-session, not per-repo**, so multiple orchestrator processes can coexist in the same repo with different brain sessions. A boot-time sweep using only the booting process's local `DashMap` would see another orchestrator's active `spur/worker-...` worktrees, find them absent from its empty memory, and delete them mid-write.

**RC3 — Workers reparent on orchestrator death.** Worker process spawns at `crates/spur-acp/src/connection/native.rs:713,1222`, `stdio_adapter.rs:88`, `cli_wrap_adapter.rs:168`, `stream_json_adapter.rs:164` — **none have `kill_on_drop(true)`**. Only blob-store git plumbing (`crates/spur-worktree/src/git_blob_store.rs:106,128`) and the `bv` CLI (`crates/spur-pm/src/bv.rs:44`) do. On SIGKILL of the orchestrator, worker children reparent to init and can keep writing to the worktree for an unbounded grace window.

Any cleanup primitive that ignores RC3 risks racing a reparented worker mid-`git commit` — producing a corrupted repo state strictly worse than the disk leak it was solving.

## 2. Invariants

> **I-1 Lifetime alignment.** A worker child process MUST NOT outlive the orchestrator that spawned it. (Closed by Phase 0a.)
>
> **I-2 Lease anchor.** A worktree's liveness MUST be inferable from a kernel-enforced primitive that auto-releases on process death. SPUR uses `fs4` advisory locks under `.spur/sessions/<brain_session_id>.lock` for this purpose.
>
> **I-3 Probe non-destructiveness.** A liveness probe MUST NOT mutate the lockfile state observable by other processes (no holder-JSON write, no unlink).
>
> **I-4 Probe-and-sweep atomicity.** Between confirming a session is dead and removing its worktrees, the lock MUST be held continuously. A new orchestrator booting with the same brain_session_id must observe contention, not race the sweep.
>
> **I-5 Self-skip.** The probing orchestrator MUST NOT sweep worktrees belonging to its own active brain sessions. Self-DoS by sweeping one's own running deliveries is unrecoverable.
>
> **I-6 Filesystem honesty.** When the filesystem cannot provide reliable advisory locking (NFS/sshfs/SMB → `ENOTSUP`/`ENOLCK`), the authority MUST disable cross-process sweeping rather than risk false-positive deletes. Existing graceful-degradation contract from `crates/spur-acp/src/session_lock.rs` (Risk #41 mitigation) extends to this subsystem.
>
> **I-7 Conservative legacy handling.** Branches in the pre-v2 namespace (`spur/worker-{agent}-{session}`) lack the brain_session_id encoding required for I-2. The authority MUST NOT auto-sweep them. Operator-invoked cleanup is the only supported path for legacy debt.

## 3. Acceptance Criteria

| # | Criterion | Verification |
|---|---|---|
| C1 | A SIGKILL'd orchestrator's worktree is reclaimed by the next orchestrator that boots | Integration test: spawn orchestrator A, create worktree, `kill -9 A`; spawn orchestrator B, observe sweep counter increments to 1 |
| C2 | Two coexisting orchestrators with different brain sessions never delete each other's worktrees | Integration test: A holds session X, B holds session Y; A's authority probe observes B's lock as `Live`, skips |
| C3 | Probing the same brain_session_id the probing process holds returns `Self_` without acquiring | Unit test on `SessionLivenessProbe` |
| C4 | Probe API never writes to the lockfile or unlinks it | Test asserts mtime + size unchanged after `Self::Live` and `Self::Missing` paths |
| C5 | A reparented worker writing to a swept worktree path scenario does not occur (RC3 closed) | Phase 0a integration test: spawn child, drop connection, child PID dies within 100ms |
| C6 | Pre-v2 branches are not auto-deleted by the authority | Test: seed `spur/worker-claude-<uuid>` in the new authority's enumeration; assert it is reported as `unknown_owner` and skipped |
| C7 | On `fs_unsafe=true`, GC sweep is skipped entirely with a single tracing event per startup | Integration test against a tmpfs that fakes `ENOTSUP` |
| C8 | Authority panic does not crash the orchestrator; `JoinHandle` abort is observable | Test injects `panic!()` in sweep; orchestrator continues, `tracing::error!` emitted |
| C9 | Branch namespace `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}` parses unambiguously even when `agent` contains hyphens | Unit test parser with `claude-code`, `gemini-2.5-pro` |

## 4. Architecture

### 4.1 Phase 0a — Worker process kill semantics (PREREQUISITE)

Add `kill_on_drop(true)` to all worker/agent spawn sites in `crates/spur-acp/src/connection/`. The change is one line per call site:

| File | Line | Spawned process |
|---|---|---|
| `native.rs` | 713 | Worker (Native adapter) |
| `native.rs` | 1222 | Brain (Native adapter, second site) |
| `stdio_adapter.rs` | 88 | Worker (stdio adapter) |
| `cli_wrap_adapter.rs` | 168 | Worker (cli-wrap adapter) |
| `stream_json_adapter.rs` | 164 | Worker (stream-json adapter) |

**Pre-implementation safety check:** read native.rs:204, :884, :1340, :1367 — these are graceful-shutdown `kill` calls. Confirm there is no double-kill race where Tokio's Drop-time SIGKILL fires before/during the explicit shutdown. Both code paths sending `SIGKILL` to a dead PID is benign on POSIX (`ESRCH`), but on Windows `TerminateProcess` on a stale handle can spuriously succeed. Audit and document.

### 4.2 New module: `spur-acp::session_liveness`

```rust
// crates/spur-acp/src/session_liveness.rs (NEW)

use std::path::Path;
use std::fs::File;
use crate::BrainSessionId;

pub struct SessionLivenessProbe;

impl SessionLivenessProbe {
    /// Probe whether `brain_session_id` is held by any live process,
    /// without mutating the lockfile state.
    ///
    /// Variants encode every observable outcome:
    /// - `Live` — another orchestrator holds the lock; the worktree is in use.
    /// - `DeadAcquired(guard)` — the lock was acquired; the prior holder is
    ///   gone. Guard MUST be held for the entire sweep window (I-4).
    /// - `Self_` — the probing orchestrator itself holds this session;
    ///   skip without touching the lockfile (I-5).
    /// - `Missing` — `.lock` file does not exist; treat as dead, no guard
    ///   needed. Edge case: session crashed before lockfile creation, or
    ///   was retired cleanly with lockfile unlink.
    /// - `FsUnsafe` — the filesystem rejected the flock with `ENOTSUP`
    ///   or `ENOLCK`; cross-process inference is unsafe (I-6).
    pub fn probe(
        repo_root: &Path,
        target: &BrainSessionId,
        held_by_self: &SelfHeldSet,
    ) -> SessionLivenessProbeResult;
}

pub enum SessionLivenessProbeResult {
    Live,
    DeadAcquired(DeadSessionGuard),
    Self_,
    Missing,
    FsUnsafe,
}

/// Holds the advisory lock on a confirmed-dead session for the duration
/// of cleanup. `Drop` releases the lock by closing the underlying File
/// handle (per fs4 semantics: `~/.cargo/.../fs4-0.13.1/src/unix.rs:19,42`).
pub struct DeadSessionGuard {
    file: File,                    // closing this releases the flock
    brain_session_id: BrainSessionId,
}

impl DeadSessionGuard {
    pub fn brain_session_id(&self) -> &BrainSessionId { &self.brain_session_id }
}

/// Set of brain_session_ids the local orchestrator currently holds.
/// Updated atomically on `load_brain_session()` / `create_brain_session()`
/// / `retire_active_brain()`. Pattern mirrors the existing peer-mailbox
/// `brain_session_id_slot` at `orchestrator.rs:1066`.
pub struct SelfHeldSet {
    inner: Arc<RwLock<HashSet<BrainSessionId>>>,
}

impl SelfHeldSet {
    pub fn new() -> Self;
    pub fn insert(&self, id: BrainSessionId);
    pub fn remove(&self, id: &BrainSessionId) -> bool;
    pub fn contains(&self, id: &BrainSessionId) -> bool;
}
```

**Implementation rules for `probe`:**

```rust
let lock_path = repo_root.join(".spur/sessions").join(format!("{}.lock", target));

if held_by_self.contains(target) {
    return Self_;                  // I-5: never probe own
}

let file = match OpenOptions::new()
    .read(true).write(true)
    .create(false).truncate(false)  // I-3: do not mutate
    .open(&lock_path)
{
    Ok(f) => f,
    Err(e) if e.kind() == io::ErrorKind::NotFound => return Missing,
    Err(e) => { tracing::warn!(...); return Missing; }   // err on safe side
};

match fs4::FileExt::try_lock_exclusive(&file) {
    Ok(true)  => DeadAcquired(DeadSessionGuard { file, brain_session_id: target.clone() }),
    Ok(false) => Live,
    Err(e) if matches_enotsup_enolck(&e) => FsUnsafe,
    Err(e) => { tracing::warn!(error=%e, "probe failed"); Live }  // fail-safe
}
```

**Critical distinction from `SessionAttachGuard`** (`crates/spur-acp/src/session_lock.rs:80,148`): the probe MUST NOT write holder JSON on success and MUST NOT unlink the file on Drop. Codex caught this — the existing guard is unsafe to reuse for probing because it would corrupt the very state being read.

### 4.3 New branch namespace (codex's correctness argument)

Pre-v2: `spur/worker-{agent}-{worker_session_id}` (`crates/spur-worktree/src/manager.rs:165`)

Post-v2: `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}`

Slash-delimited because:
- Agent names contain hyphens (`claude-code`, `gemini-2.5-pro`); hyphen-delimited parsing is ambiguous.
- Git ref slash-segmentation is well-tested by the `refs/heads/` convention.
- Length budget: ~85 bytes (2 UUID-36 + agent + literal); MCP caps branch display at 256 bytes (`crates/spur-mcp/src/outcome_materializer.rs:29`). Safe.

Parser:

```rust
pub fn parse_v2_branch(branch: &str) -> Option<V2BranchOwner> {
    let rest = branch.strip_prefix("spur/worker/v2/")?;
    let mut parts = rest.rsplitn(3, '/');
    let worker_session = parts.next()?;
    let brain_session = parts.next()?;
    let agent = parts.next()?;             // remainder, may include hyphens
    Some(V2BranchOwner {
        agent: agent.to_string(),
        brain_session_id: BrainSessionId::parse(brain_session).ok()?,
        worker_session_id: SessionId::parse(worker_session).ok()?,
    })
}
```

`rsplitn(3, '/')` parses right-to-left so a hyphen-bearing agent in the leftmost slot survives unambiguously.

### 4.4 `WorktreeAuthority` actor

```rust
// crates/spur-core/src/worktree_authority.rs (NEW)

pub struct WorktreeAuthority {
    repo_root: Arc<PathBuf>,
    self_held: SelfHeldSet,
    funnel: FunnelHandle,                    // for emitting events
    config: AuthorityConfig,
}

pub struct AuthorityConfig {
    pub sweep_interval: Duration,            // default: 15 * 60s + jitter [0, 120s)
    pub quarantine_grace: Duration,          // default: 30s (G2 defense-in-depth)
    pub fs_unsafe_skip: bool,                // default: true; respect I-6
}

impl WorktreeAuthority {
    pub fn new(repo_root: PathBuf, self_held: SelfHeldSet, funnel: FunnelHandle, config: AuthorityConfig) -> Self;

    /// Run one sweep pass synchronously. Returns counts.
    pub async fn sweep_once(&self) -> Result<SweepReport, AuthorityError>;

    /// Spawn the periodic sweep loop. Returns a JoinHandle the caller
    /// stores in `Orchestrator.background_tasks`. Aborted on Drop.
    pub fn spawn_periodic(self: Arc<Self>) -> JoinHandle<()>;
}

pub struct SweepReport {
    pub probed: usize,
    pub swept: usize,
    pub skipped_self: usize,
    pub skipped_live: usize,
    pub skipped_quarantine: usize,
    pub skipped_unknown_owner: usize,    // pre-v2 branches; I-7
    pub skipped_fs_unsafe: usize,
    pub remove_failures: usize,
}
```

**Sweep algorithm:**

```text
1. If config.fs_unsafe_skip && self.detect_fs_unsafe(): emit one tracing event, return early.
2. Enumerate via `git worktree list --porcelain`:
   for each block:
     - parse 'worktree <path>' and 'branch <ref>'
     - if branch is None or path is None: continue
     - if !branch.starts_with("refs/heads/spur/worker/v2/"):
         report.skipped_unknown_owner += 1; continue        # I-7
     - owner = parse_v2_branch(branch).expect("validated above")
     - probe = SessionLivenessProbe::probe(&repo_root, &owner.brain_session_id, &self_held)
     - match probe:
         Self_:        report.skipped_self += 1; continue
         Live:         report.skipped_live += 1; continue
         FsUnsafe:     report.skipped_fs_unsafe += 1; continue
         Missing:
             apply_quarantine(); maybe sweep
         DeadAcquired(guard):
             apply_quarantine(); sweep within guard scope; guard drops on success
3. Run `git worktree prune` to reconcile orphan admin entries
   (NARROWED — only after this authority's own sweeps have run; never blanket).
4. Emit SweepReport via FunnelHandle as a tracing event:
   target="spur.metrics.worktree_authority", with all counters.
```

**Quarantine grace check:** maintain `last_seen_alive: HashMap<BrainSessionId, Instant>` on the authority. On every probe that returns `Live`, update entry. On a probe that returns `Missing`/`DeadAcquired`, check `now - last_seen_alive.get(id).unwrap_or(now) > config.quarantine_grace`. Only sweep if true. This is G2 defense-in-depth: even with `kill_on_drop(true)` from Phase 0a, a 30-second window absorbs any straggler IO. Cheap insurance.

**Sweep within guard scope:**

```rust
async fn sweep_one(&self, guard: DeadSessionGuard, owner: V2BranchOwner, path: PathBuf, branch: String) -> Result<()> {
    // Lock held throughout. I-4.
    let _g = guard;  // forces Drop at end of scope, releasing flock

    // Codex correction: --force --force for locked entries
    let removed = run_git(&["worktree", "remove", "--force", "--force", path.to_str()?])
        .await?;
    let branch_deleted = run_git(&["branch", "-D", &branch])
        .await?;

    // Best-effort prune; failures here are non-fatal
    let _ = run_git(&["worktree", "prune"]).await;

    Ok(())
}
```

### 4.5 Orchestrator integration

Replace the dead `pub worktrees: WorktreeManager` field at `orchestrator.rs:792` with:

```rust
pub struct Orchestrator {
    // ... existing fields ...
    worktree_authority: Arc<WorktreeAuthority>,
    self_held: SelfHeldSet,                  // already needed by 4.2
    // remove: pub worktrees: WorktreeManager     <-- Potemkin field
}
```

In `Orchestrator::new` at `orchestrator.rs:954`:

```rust
let self_held = SelfHeldSet::new();
let worktree_authority = Arc::new(WorktreeAuthority::new(
    repo_root.clone(),
    self_held.clone(),
    funnel_handle.clone(),
    AuthorityConfig::default(),
));

// Run startup sweep BEFORE orchestrator becomes reachable to MCP/TUI.
// Safe now because self_held is empty (no sessions loaded yet).
match worktree_authority.sweep_once().await {
    Ok(report) => tracing::info!(target: "spur.metrics.worktree_authority.startup",
        probed = report.probed, swept = report.swept, skipped_live = report.skipped_live,
        skipped_unknown_owner = report.skipped_unknown_owner),
    Err(e) => tracing::warn!(error=%e, "startup worktree authority sweep failed"),
}

// Spawn periodic sweep into background_tasks (existing infra at :918,
// parallel to peer-mailbox reconciler at :1085). Aborted on Drop.
let periodic = worktree_authority.clone().spawn_periodic();
orchestrator.background_tasks.push(periodic);
```

In `load_brain_session()` and `create_brain_session()` (around `orchestrator.rs:1448–1500` per the prior session-attach spec):

```rust
self.self_held.insert(brain_session_id.clone());
```

In `retire_active_brain()`:

```rust
self.self_held.remove(&brain_session_id);
```

### 4.6 Per-delegation `WorktreeManager` retirement

The per-delegation `let mut worktrees = WorktreeManager::new(repo_root)` at `orchestrator.rs:4248` is retained for the actual create/remove operations during a delegation's normal lifecycle (not the asynchronous cleanup). It does not participate in cross-process garbage collection. The `WorktreeManager` API is otherwise unchanged.

**Codex-required corrections to existing `WorktreeManager`:**

1. `remove_worktree` ordering bug (`crates/spur-worktree/src/manager.rs:347`): currently removes the in-memory entry before invoking `git worktree remove`. If Git fails, the only process-local reference is lost. Fix: remove from `self.active` only after Git succeeds.
2. `cleanup_orphans` namespace narrowing (`manager.rs:459`): `branch.contains("spur/")` is too broad. Narrow to `refs/heads/spur/worker/v2/` (and `refs/heads/spur/brain-snapshot-` for snapshot cleanup).
3. Snapshot branch collision (`manager.rs:87–90`): append `process_id + nonce` to the existing `timestamp + AtomicU64` for cross-process uniqueness. (Phase 2; not Phase 1' blocker.)

### 4.7 Telemetry

Counters emitted via `funnel` per sweep:

- `spur.metrics.worktree_authority.startup` — once per orchestrator boot
- `spur.metrics.worktree_authority.periodic` — every 15±2 min
- `spur.metrics.worktree_authority.on_session_retire` — at `retire_active_brain` exit

Each event carries the full `SweepReport`. Pattern matches `spur.metrics.outcome_swept` at `orchestrator.rs:1048`.

## 5. Out of Scope

- **Phase 0b (JoinSet/JoinHandle supervision for Tokio dispatch tasks).** Independent work; affects orchestrator-side shutdown but not WorktreeAuthority safety. Tracked separately.
- **Phase 2 fixes** — `fsync` for `FsOutcomeStore::put`, snapshot branch collision fix, double `--force` for `WorktreeManager::remove_worktree`, NFS deployment policy. All independent of this spec; landable after.
- **Phase 3** — converting blob-store sweep at `orchestrator.rs:1041` from one-shot to interval-driven. Independent of this spec.
- **Phase L** — operator-invoked `scripts/spur-worktree-gc-legacy.sh` for reclaiming the existing 10 GB. Standalone bash, no Rust changes. Documented separately.
- **Decomposition into 6th actor.** Gemini's "WorktreeAuthority as 6th actor" framing is the v1.0+ aspirational shape. For this spec, the authority lives on `Orchestrator.background_tasks` reusing existing supervisor infrastructure.

## 6. Risks & Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| `kill_on_drop(true)` interacts badly with explicit `kill` paths at `native.rs:204,884,1340,1367` (double-kill race) | Low | Pre-implementation audit of those four sites; integration test asserts no spurious errors on graceful shutdown |
| Probe acquires lock against a session that is mid-`retire_active_brain` (the `retire` path between `self_held.remove` and lockfile unlink) | Medium | `retire_active_brain` must update `self_held` BEFORE unlinking the lockfile, and the unlink ordering must be: drop guard (releases flock) → THEN `self_held.remove`. Document as invariant in `retire_active_brain`. |
| Quarantine 30s grace insufficient under heavy disk pressure (worker mid-flush longer than 30s) | Low (with kill_on_drop) | Defense-in-depth, not load-bearing. If telemetry shows `swept` events with reparented-IO evidence, escalate to G3 (mtime-based two-phase delete). |
| Authority panics; sweep stops happening | Medium | `JoinHandle` in `background_tasks`, abort on Drop; emit `tracing::error!` from `spawn_periodic`. Restart-on-panic supervisor is a Phase 3 follow-up; for v0, missing one sweep cycle is acceptable. |
| `git worktree list --porcelain` output format changes between git versions | Low | Pin parsing to the documented stable format; integration test covers git 2.30+ (the workspace's minimum). |
| `fs4` on Windows uses `LockFileEx` semantics that differ subtly from POSIX `flock` | Low | Existing `crates/spur-acp/src/session_lock.rs` already uses `fs4` on Windows; reuse the same primitive, inherit the same testing. |
| Two orchestrators boot simultaneously, both run startup sweep, both probe-acquire-release on the same dead session | Low | Both will hold the lock briefly; whichever loses the race probes `Live`; idempotent. |

## 7. Migration

**No automated migration.** The 800+ legacy `spur/worker-{agent}-{session}` branches and 34 worktree directories on disk are NOT swept by the new authority (I-7). They are unchanged. Operator opts in via Phase L (`scripts/spur-worktree-gc-legacy.sh`, separate work item).

For new orchestrators running v2 code:
- New worktrees use the `spur/worker/v2/...` namespace.
- Old worktrees (created by pre-v2 orchestrator instances) co-exist on disk indefinitely until Phase L is run.
- Authority sweep reports `skipped_unknown_owner` count for legacy entries on every cycle; this is observable.

## 8. Test Strategy

**Phase 0a tests** (`crates/spur-acp/src/connection/`):
- Per adapter (4 sites): spawn child, drop the connection, poll the child PID with `kill -0` for 100ms, assert the PID dies.
- Audit test: confirm explicit `kill` paths at native.rs:204,884,1340,1367 produce no spurious errors when followed by Drop-time SIGKILL.

**`SessionLivenessProbe` unit tests** (`crates/spur-acp/src/session_liveness.rs`):
- `probe_returns_self_for_held_session`
- `probe_returns_live_when_other_holds`
- `probe_returns_dead_acquired_when_lockfile_exists_but_unlocked`
- `probe_returns_missing_when_lockfile_absent`
- `probe_does_not_truncate_lockfile` (read mtime+size before/after, assert unchanged on `Live` and `Missing`)
- `probe_does_not_unlink_on_drop` (assert lockfile still exists after `DeadAcquired` Drop)
- `dead_session_guard_releases_lock_on_drop` (acquire, drop, re-probe returns `DeadAcquired` again)
- `probe_returns_fs_unsafe_on_enotsup` (mock with `nix::sys::syscall` or feature flag)

**Branch parser unit tests** (`crates/spur-worktree/src/manager.rs`):
- `parse_v2_branch_with_simple_agent`
- `parse_v2_branch_with_hyphenated_agent` (`claude-code`)
- `parse_v2_branch_with_dotted_agent` (`gemini-2.5-pro`)
- `parse_v2_branch_rejects_pre_v2_format`
- `parse_v2_branch_rejects_extra_segments`

**`WorktreeAuthority` integration tests** (`crates/spur-core/tests/worktree_authority.rs`):
- `startup_sweep_reclaims_dead_session_worktree`
- `startup_sweep_skips_live_peer_worktree`
- `startup_sweep_skips_legacy_branches`
- `periodic_sweep_emits_telemetry_event`
- `panic_in_sweep_does_not_crash_orchestrator`
- `quarantine_grace_prevents_immediate_sweep_after_lock_dies`

**Phase 1' end-to-end test** (the critical multi-process safety test):
- Spawn orchestrator A in subprocess, create worktree under brain session X
- Verify worktree exists on disk
- Spawn orchestrator B in subprocess (different brain session Y)
- B's startup sweep runs
- Assert A's worktree still exists on disk (B did NOT delete it)
- `kill -9 <pid of A>`
- Wait for A's lockfile to release (auto on process exit)
- Trigger B's periodic sweep (or call `sweep_once()` via test hook)
- Assert A's worktree IS deleted; A's branch IS deleted
- Assert `swept = 1`, `skipped_self = 0`, `skipped_live = 0` in B's sweep report

## 9. References

- Risk catalog: `docs/architecture.md:594` (Risk #4)
- Existing session lock: `crates/spur-acp/src/session_lock.rs`, `docs/architecture.md` §6
- Existing peer-mailbox `brain_session_id_slot` pattern: `crates/spur-core/src/orchestrator.rs:1066`
- Existing background_tasks infra: `crates/spur-core/src/orchestrator.rs:918`
- Existing blob-store sweep: `crates/spur-core/src/orchestrator.rs:1041` (one-shot, periodic conversion is Phase 3)
- `fs4` semantics: `~/.cargo/registry/src/index.crates.io-*/fs4-0.13.1/src/unix.rs:19,42`
- Round 1 review (codex): `crates/spur-acp/src/connection/{native,stdio_adapter,cli_wrap_adapter,stream_json_adapter}.rs` `kill_on_drop` audit; multi-object git transaction framing
- Round 1 review (gemini): destructive-Phase-1 critique; `WorktreeAuthority` naming; lease-aware sweeping anchored to session lock
- Round 2 review (codex): `SessionAttachGuard` unsafe-as-probe; v2 branch namespace; quarantine grace
- Round 2 review (gemini): block-on-prerequisite; agent-name observability; backwards-compat parser
- MCTS-driven synthesis: this document
