# SPUR TUI Explicit Session Attach — Lockfile, Picker Landing, and Single-Attach Invariant

**Status:** Approved (Path Z′′, post-MCTS-round-2)
**Supersedes (partial):** [`2026-04-24-spur-tui-landing-experience-design.md`](./2026-04-24-spur-tui-landing-experience-design.md) — keeps the `LandingDecision` enum + CLI contract, redefines the rendering for `AutoResume`.
**Author:** brain (synthesized from peer review by gemini + kimi)

---

## 1. Problem

`spur tui` today auto-resumes the last active session if it exists and is `<24h` old. Two concurrent `spur tui` processes in the same workspace both read `.spur/session_metadata.json`, both fire `InteractiveInput::ResumeSession` against the same `acp_session_id`, and both call `Orchestrator::load_brain_session()` (`crates/spur-core/src/orchestrator.rs:1448-1500`) attaching to the same ACP transport.

This is **not just a UX wart.** The ACP layer is single-attach by protocol design. Two attached orchestrators on the same channel violate the protocol-level invariant — undefined behavior, not just confused UX. Today we get away with it because users rarely run multiple TUIs in the same repo, but the invariant is silently broken whenever they do.

## 2. The Single-Attach Invariant

> At most one orchestrator process MAY hold an active ACP attachment to a given `(brain_session_id, acp_session_id)` pair at any instant.

Every design decision in this spec flows from enforcing this invariant.

## 3. Acceptance Criteria

| # | Criterion | Verification |
|---|---|---|
| C1 | Single-attach invariant enforced under all multi-instance scenarios | Cross-process integration test: two `spur tui --session <id>` instances; second sees rejection modal |
| C2 | Returning user keeps muscle memory; explicit consent at most 1 keystroke from prior auto-resume | Manual UX test; landing time-to-attach ≤ 1 keystroke |
| C3 | When locked out, user has a path forward without restarting machine | Modal copy includes shell `kill <pid>` command; no terminal-only restart required |
| C4 | Cross-platform: Linux, macOS, Windows (per release matrix) | CI exercises `try_acquire` on all three runners |
| C5 | Filesystem agnostic: NFS/sshfs/SMB users not locked out | `ENOTSUP`/`ENOLCK` → degraded mode with `fs_unsafe` badge, attach proceeds |
| C6 | No silent regressions in single-window common case | All existing TUI integration tests pass unmodified |
| C7 | Phase 1 ships as a strict correctness improvement even without Phase 2 | Phase 1 alone: silent UB → loud rejection modal + Dashboard fallback |

## 4. Architecture

### 4.1 New module: `spur-acp::session_lock`

```rust
// crates/spur-acp/src/session_lock.rs (new)

pub struct SessionAttachGuard {
    file: std::fs::File,           // holds the flock
    pid_path: PathBuf,             // for cleanup on Drop
    acp_id: String,
}

impl SessionAttachGuard {
    /// Try to acquire an exclusive attach lock for `acp_id`.
    ///
    /// Returns:
    /// - `Ok(Acquired(guard))` — exclusive ownership, proceed
    /// - `Ok(DegradedNoLock { reason })` — filesystem rejected the lock
    ///   (ENOTSUP/ENOLCK on NFS/sshfs/SMB); guard is None, attach proceeds
    ///   under `fs_unsafe = true`
    /// - `Err(AlreadyAttached(holder))` — another process holds it
    /// - `Err(Io(e))` — unrecoverable IO error
    pub fn try_acquire(repo_root: &Path, acp_id: &str) -> AcquireOutcome;
}

pub enum AcquireOutcome {
    Acquired(SessionAttachGuard),
    DegradedNoLock { reason: String },  // fs_unsafe path
    Rejected { holder: HolderInfo },
    Io(std::io::Error),
}

impl Drop for SessionAttachGuard { /* kernel auto-releases on close; remove pid file */ }

pub struct HolderInfo {
    pub pid: Option<u32>,                     // for shell `kill` command
    pub started_at: Option<DateTime<Utc>>,
    pub tty: Option<String>,                  // best-effort; native shells only
    pub label: Option<String>,                // env: SPUR_TUI_LABEL
    pub workdir: Option<PathBuf>,             // disambiguates clones of same repo
}
```

**Lock primitive:** `fs4::FileExt::try_lock_exclusive` (new dependency in `spur-acp/Cargo.toml`). `fs4` is the maintained fork of `fs2` and supports Windows `LockFileEx` natively. `spur-pm` continues to use `fs2` for the brain-pidfile path (separate concern; migrate later if useful).

**Lockfile path:** `.spur/sessions/<acp_id>.attach.lock` (relative to `repo_root`).

**Lockfile content:** JSON-serialized `HolderInfo`, written AFTER successful flock acquire, truncated each time (`set_len(0)` before write — guards against stale-PID-with-trailing-junk bug). Content is informational only; flock acquire is the gating mechanism.

**Error classification:**
- `WouldBlock` / Linux `EAGAIN(11)` / macOS `EAGAIN(35)` / Windows `ERROR_LOCK_VIOLATION(33)` → `Rejected { holder }` (read JSON, parse best-effort, fields default to `None` on parse failure)
- `ENOTSUP(95)` / `ENOLCK(37)` → `DegradedNoLock { reason }` + `tracing::warn!("flock unsupported on {:?}; multi-instance protection disabled", path)`
- Other `io::Error` → `Io(e)`

**No PID-liveness inference.** Successful flock acquisition IS the "previous holder gone" signal. PID is purely diagnostic.

### 4.2 Orchestrator integration

Replace the current `agent_connection: Option<(Box<dyn AgentConnection>, String)>` tuple with a named struct:

```rust
// crates/spur-core/src/orchestrator.rs

struct ActiveConnection {
    transport: Box<dyn AgentConnection>,
    brain_name: String,
    /// `None` only when we attached under degraded fs_unsafe mode.
    /// Holding this guard for the lifetime of `transport` is what enforces
    /// the single-attach invariant.
    attach_guard: Option<SessionAttachGuard>,
    /// Whether this attachment is unprotected (NFS/sshfs/SMB without flock).
    fs_unsafe: bool,
}

// agent_connection: Option<ActiveConnection>
```

**Lifetime rule:** `attach_guard` is dropped when `transport` is dropped. Both live inside `ActiveConnection`. When `retire_active_brain` (`orchestrator.rs:2288-2366`) extracts the transport back into `agent_connection`, the guard MUST move with it. Code reviewers MUST verify no path drops `attach_guard` while `transport` survives.

**Acquisition sites:**
- `Orchestrator::load_brain_session` (`orchestrator.rs:2614`) — call `SessionAttachGuard::try_acquire` after the connection is established but before constructing the `BrainSession`. Lock acquired ONCE per ACP id, NOT pre-TUI in CLI.
- `Orchestrator::create_brain_session` — same pattern. New session id is unique by construction; expect `Acquired` always (log if not).

**Outcome handling:**
- `Acquired(guard)` → wrap into `ActiveConnection`; emit `AgentSessionReady { fs_unsafe: false, .. }`
- `DegradedNoLock { reason }` → wrap with `attach_guard: None, fs_unsafe: true`; emit `AgentSessionReady { fs_unsafe: true, .. }`
- `Rejected { holder }` → return `LoadBrainSessionError::AlreadyAttached { holder }`; orchestrator emits `SessionAttachRejected { acp_id, holder, fs_unsafe: false }` event
- `Io(e)` → propagate as today (`BrainError`)

### 4.3 Event surface

Add to `SpurEventBody`:

```rust
SessionAttachRejected {
    acp_id: String,
    holder: HolderInfo,
    fs_unsafe: bool,  // true if a prior degraded attach is the holder
}
```

Modify existing `SpurEventBody::AgentSessionReady` to include:

```rust
AgentSessionReady {
    // ...existing fields...
    fs_unsafe: bool,  // true when attached under DegradedNoLock
}
```

Both variants are serialized through the existing event channel — no new transport plumbing needed. Structured payload preserved end-to-end (no `format_error_chain` stringification).

### 4.4 CLI surface

Add one flag, retain everything else:

```
spur tui                       # Default. Land per resolve_landing() → picker (Phase 2)
spur tui --session <acp_id>    # NEW. Open picker preselected, auto-fire Enter on launch
spur tui --new                 # KEPT. Force ShowDashboard
spur tui --dashboard           # KEPT. Force ShowDashboard
spur tui --sessions            # KEPT. Force ShowPicker (no preselect)
spur tui --brain <name>        # KEPT
```

No `--force-attach` flag. The escape hatch is the modal's surfaced shell command.

`--session <bad-id>` → still launches TUI; first event is `SessionAttachRejected` with `holder: HolderInfo { all-None }` and the modal explains "no session by that id" (one of the modal's sub-states; see §6.2).

### 4.5 Landing decision

Modify `LandingDecision` in `crates/spur-tui/src/landing.rs`:

```rust
enum LandingDecision {
    AutoResume { acp_id: String, brain: String },          // KEPT, rendering changes
    AttachExplicit { acp_id: String, brain: String },      // NEW (--session <id>)
    ShowPicker { preselect: Option<String> },              // KEPT, gains optional preselect
    ShowDashboard,                                         // KEPT
    SetupRequired,                                         // KEPT
}
```

`resolve_landing()` (`crates/spur-cli/src/main.rs:56`):
- `--session <id>` → `AttachExplicit { acp_id: id, brain }` (highest priority after `--new`)
- `--new` → `ShowDashboard` (kept)
- `--sessions && !--dashboard` → `ShowPicker { preselect: None }`
- `--dashboard` → `ShowDashboard`
- `registry.list().is_empty()` → `SetupRequired`
- Last-active `<24h` fresh + brain matches → `AutoResume { acp_id, brain }`
- Has any session → `ShowPicker { preselect: None }`
- Else → `ShowDashboard`

The CLI translates `LandingDecision` into TUI startup state in `crates/spur-cli/src/main.rs:717-738`:
- `AutoResume { acp_id, .. }` → start TUI in `SessionPickerView` with `preselect = Some(acp_id)`. **No `UserInput` is sent.** User must press Enter. (This is the axiom: no implicit attach.)
- `AttachExplicit { acp_id, .. }` → start TUI in `SessionPickerView` with `preselect = Some(acp_id)` AND `tui_tx.send(UserInput::ResumeSession { session_id: acp_id })` exactly once at startup, mirroring the existing AutoResume dispatch (`main.rs:721-725`). **The CLI flag IS the explicit consent** — no synthetic `KeyEvent` is injected; the orchestrator receives a real `ResumeSession` and runs through `try_acquire`. If `try_acquire` rejects, the rejection modal renders over the picker landing.
- `ShowPicker { preselect }` → start TUI in `SessionPickerView` with optional preselect
- `ShowDashboard` → unchanged
- `SetupRequired` → unchanged

**Brain-mismatch handling for `--session <id>`:** if the user passes both `--session <id>` and `--brain <name>`, and the stored brain for that ACP id differs from `<name>`, `resolve_landing()` returns `AttachExplicit` anyway (the user asked for that session), but the `--brain` override is dropped with a `tracing::warn!` and the stored brain is used. Rationale: an ACP session id is bound to a brain; you cannot resume the same conversation under a different brain. If users want a fresh brain, they should `--new`.

## 5. SessionPickerView changes

`crates/spur-tui/src/views/session_picker.rs:116`. Additions:

```rust
pub struct SessionPickerView {
    // ...existing fields...

    /// Set when AutoResume or AttachExplicit lands here. Rendered as a
    /// top banner; does NOT auto-fire Enter — user must confirm.
    preselect: Option<String>,
}
```

**Behavior on first render:**
- If `preselect.is_some()` and a row matches: cursor jumps to that row, top banner renders `Last: <name> · <relative-time> · [Enter] resume · [n] new`
- If `preselect` does not match any row: render banner `Session <id> not found · [Enter] new · [Esc] cancel`
- If `preselect.is_none()`: existing behavior (cursor on row 0 = `[+ New session]`)

**Enter on a row:**
1. Send `UserInput::ResumeSession { session_id }` (existing flow)
2. Orchestrator calls `load_brain_session` → `try_acquire`
3. On `Rejected { holder }` → orchestrator emits `SessionAttachRejected` → TUI handler shows modal (§6.2)
4. On `Acquired` or `DegradedNoLock` → `AgentSessionReady` event → navigate to `SessionDetail`

**The render-time badge** showing `⚠ attached:<pid>` next to a row is **purely informational** — derived from `try_lock_exclusive` test polling at `Tick` intervals (every 2s, NOT every render). The MODAL only ever appears after a fresh `try_acquire` failure on `Enter`. If the badge says "attached" but the holder exited 1s ago, the user pressing Enter succeeds — no false-positive modal.

## 6. Wireframes

### 6.1 Step 2 — Returning user, picker preselect (the heart of Path Z′′)

```
$ spur tui   # 5 minutes after closing the previous session
```

```
╭─ Sessions ─────────────────────────────────────────── spur 0.4 ╮
│  Last: refactor-auth · 5m ago · [Enter] resume · [n] new       │
│ ─────────────────────────────────────────────────────────────  │
│ ▸ refactor-auth          claude-code   5m ago    2 workers     │
│   bd-cfb.2 badge regress claude-code   2h ago    ●idle         │
│   peer-mailbox brainstm  codex          1d ago    ●idle         │
│   brain-continuation p1  claude-code   3d ago    ●idle         │
│                                                                │
│  [↑↓] navigate · [Enter] attach · [/] filter · [n]ew  [Q]uit   │
╰────────────────────────────────────────────────────────────────╯
```

No input bar. No callout box. Single `List`. One-keystroke explicit consent.

### 6.2 Step 5 — Collision modal

When `SessionAttachRejected` fires on Enter:

```
   ╔══════════════════════════════════════════════════════════════╗
   ║  Session is attached in another window                       ║
   ║  ─────────────────────────────────────────────────────────   ║
   ║  refactor-auth                                               ║
   ║    holder: morning-coding (started 14:32, 13 min ago)        ║
   ║    workdir: /Volumes/Projects/spur                           ║
   ║                                                              ║
   ║   [N] start a new session    [P] open picker filter          ║
   ║   [Esc] cancel                                               ║
   ║                                                              ║
   ║   To take over manually, run in your shell:                  ║
   ║     kill 84321                                               ║
   ║   then press [Enter] to retry attach.                        ║
   ╚══════════════════════════════════════════════════════════════╝
```

**Holder field rendering priority** (first non-`None` wins, others stack as supplementary lines): `label > tty > pid + started_at`. `workdir` is always rendered if present. PID is rendered ONLY in the `kill` command line at the bottom.

**Sub-state for unknown session id (`--session <bad-id>`):** modal renders with `holder` showing only `(no holder — session id not found)`; `[N]` and `[Esc]` are the only active keys; the `kill` line is omitted.

### 6.3 Step 9 — fs_unsafe attach (NFS/sshfs/SMB)

When `AgentSessionReady { fs_unsafe: true }` arrives:

```
╭─ Session: refactor-auth ─────────────── claude-code · ●idle ⚠ NFS ╮
│ user · 5m ago                                                       │
│   …transcript…                                                      │
│                                                                     │
│ ─ unsafe-fs: flock unsupported on this volume ─────────────────     │
│   Multi-window protection is OFF. [?] details                       │
│                                                                     │
├─ INSERT ────────────────────────────────────────────────────────────┤
│ > _                                                                 │
╰─────────────────────────────────────────────────────────────────────╯
```

Phase 3 polish: after first user keystroke, the two-line banner collapses to the `⚠ NFS` tag in the header. `[?]` reopens the full explanation as a transient toast.

### 6.4 Step — Zero-session onboarding (Phase 3)

When `resolve_landing()` returns `ShowDashboard` because `meta.has_any_session() == false`, the existing `DashboardView` (`crates/spur-tui/src/views/dashboard.rs:500-587`) renders as today (`AgentsTree` empty + `InputBar` focused for first message). Phase 3 adds a centered welcome overlay above the input bar:

```
╭─ Dashboard ─────────────────────────────────────────── spur 0.4 ╮
│                                                                  │
│                                                                  │
│        Welcome to SPUR. Type a message to start working.         │
│                                                                  │
│                                                                  │
│  Active brain: claude-code · [/?] help                           │
├──────────────────────────────────────────────────────────────────┤
│  > _                                                             │
╰──────────────────────────────────────────────────────────────────╯
```

This is a `DashboardView` empty-state, NOT a SessionPickerView state. The picker is only the landing surface when there is at least one session to preselect or browse.

## 7. Phasing

### Phase 1a — `ActiveConnection` named struct (mechanical)

**Files modified:**
- `crates/spur-core/src/orchestrator.rs` — replace `(Box<dyn AgentConnection>, String)` tuple with `ActiveConnection { transport, brain_name }` struct at all sites (~8 pattern-match sites: `1399`, `1444`, `1828`, `2128`, `2366`, plus a few more — exhaustive list during implementation)
- No behavior change. Compile-only refactor.

**Tests:** all existing `spur-core` tests pass unmodified.

### Phase 1b — Lock module + event variants (correctness)

**New files:**
- `crates/spur-acp/src/session_lock.rs` (~250 LOC including unit tests)

**Files modified:**
- `crates/spur-acp/Cargo.toml` — add `fs4 = "0.13"` (or current stable)
- `crates/spur-acp/src/lib.rs` — export `session_lock` module + `HolderInfo` + `SessionAttachGuard`
- `crates/spur-acp/src/event.rs` (or wherever `SpurEventBody` lives) — add `SessionAttachRejected` variant; add `fs_unsafe: bool` to `AgentSessionReady`
- `crates/spur-core/src/orchestrator.rs` — `load_brain_session` and `create_brain_session` call `try_acquire`; `ActiveConnection` gains `attach_guard: Option<SessionAttachGuard>` and `fs_unsafe: bool`; emit new event on rejection
- `crates/spur-tui/src/app.rs` — handler for `SessionAttachRejected` opens collision modal; handler for `AgentSessionReady { fs_unsafe: true }` shows persistent banner + header tag
- `crates/spur-tui/src/components/` — new `CollisionModal` widget (centered popup, follows existing `QuitConfirmDialog` pattern)
- `crates/spur-tui/src/views/session_detail.rs` — render `fs_unsafe` banner + header tag

**Tests:**
- `session_lock` unit tests: acquire → release → reacquire (kernel-released on Drop); concurrent acquire fails with `Rejected`; ENOTSUP simulated via mock filesystem returns `DegradedNoLock`; PID file content roundtrip with `set_len(0)` truncation; HolderInfo JSON parse-defaults on malformed input
- Cross-process integration test in `crates/spur-cli/tests/`: spawn two `spur tui --session <id>` processes; assert second emits `SessionAttachRejected`
- TUI snapshot tests for the new modal (using existing `insta` infrastructure if present)

**Phase 1b alone (without Phase 2):** existing AutoResume flow + lock = silent UB → loud rejection modal landing on Dashboard. **Strict improvement.**

### Phase 2 — Picker landing + `--session` flag (UX)

**Files modified:**
- `crates/spur-tui/src/landing.rs` — add `AttachExplicit { acp_id, brain }` variant; add `preselect: Option<String>` to `ShowPicker`
- `crates/spur-cli/src/main.rs` — add `--session` clap flag; update `resolve_landing()`; update CLI-to-TUI dispatch in lines 717-738 to (a) start picker with `preselect` for both `AutoResume` and `AttachExplicit`, (b) fire synthetic `Enter` only for `AttachExplicit`
- `crates/spur-tui/src/views/session_picker.rs` — add `preselect` field; render top banner when populated; jump cursor to matching row on first render; handle "preselect not found" sub-state
- `crates/spur-tui/src/app.rs` — wire `start_in_picker` boolean into `preselect` plumbing

**Tests:**
- `resolve_landing` tests: `--session <id>` returns `AttachExplicit`; `--new` precedence over `--session`; `AutoResume` still returns `AutoResume` (variant kept)
- TUI test: picker with `preselect = Some(id)` jumps cursor to matching row, renders banner
- TUI test: picker with `preselect = Some(unknown_id)` renders "not found" sub-state
- Integration test: `spur tui --session <id>` launches and auto-attaches without user input (synthetic Enter)
- Manual UX test (recorded via terminal recording): bare `spur tui` lands on picker, banner present, single Enter resumes

### Phase 3 — Polish (separate PR, lower priority)

- `fs_unsafe` banner auto-collapse to header tag after first keystroke
- Zero-session centered onboarding state
- `SPUR_TUI_LABEL` env var → propagated into `HolderInfo.label` at lockfile-write time
- Render-time row badges for picker (the `⚠ attached:<pid>` informational glyph)

## 8. Test Plan

| Phase | Test type | Test name | Location |
|---|---|---|---|
| 1a | unit | refactor-only; existing tests pass | `crates/spur-core/src/orchestrator.rs` test mods |
| 1b | unit | `acquire_then_release_drops_lock` | `crates/spur-acp/src/session_lock.rs` |
| 1b | unit | `concurrent_acquire_in_same_process_fails` | same |
| 1b | unit | `crashed_process_lock_recovered_via_kernel` | same |
| 1b | unit | `enotsup_returns_degraded_no_lock` | same (with mock fs) |
| 1b | unit | `set_len_zero_prevents_stale_pid_junk` | same |
| 1b | unit | `holder_info_json_roundtrip_with_missing_fields` | same |
| 1b | integration | `two_concurrent_session_attaches_second_emits_rejected` | `crates/spur-cli/tests/` |
| 1b | TUI snapshot | `collision_modal_renders_holder_label` | `crates/spur-tui/tests/` |
| 1b | TUI snapshot | `session_detail_shows_fs_unsafe_banner` | same |
| 2 | unit | `resolve_landing_attach_explicit_for_session_flag` | `crates/spur-cli/src/main.rs` test mod |
| 2 | unit | `resolve_landing_new_overrides_session_flag` | same |
| 2 | TUI snapshot | `picker_with_preselect_jumps_cursor_and_renders_banner` | `crates/spur-tui/tests/` |
| 2 | TUI snapshot | `picker_with_unknown_preselect_renders_not_found` | same |
| 2 | integration | `attach_explicit_auto_fires_enter_and_attaches` | `crates/spur-cli/tests/` |
| 2 | manual | recording: `spur tui` → picker → Enter → SessionDetail | `docs/superpowers/recordings/` |

**CI matrix:** `try_acquire` unit tests run on `linux-x86_64`, `macos-aarch64`, `windows-x86_64`.

## 9. Migration & Compatibility

- `.spur/session_metadata.json` schema: unchanged. Still drives picker ordering (most-recent first) and the `AutoResume` decision.
- `.spur/sessions/<acp_id>.attach.lock` is new; created on first attach attempt. No migration needed.
- Existing CLI flags (`--new`, `--dashboard`, `--sessions`, `--brain`) all retained with identical behavior. The 2026-04-24 spec contract is preserved at the CLI surface.
- The `LandingDecision::AutoResume` variant is retained for API stability, but its rendering changes (now routes to `ShowPicker` with preselect). The variant identity matters; the side-effect changes.
- Users who relied on zero-keystroke resume now press one Enter. This is the intentional axiom: no implicit attach.

## 10. Open Questions

All resolved during MCTS rounds:

| Q | Decision | Round |
|---|---|---|
| Land on Dashboard or Picker? | Picker (kimi: Dashboard has no session list; reuses existing `SessionPickerView`) | Round 2 |
| Keep or drop `--force-attach`? | Drop CLI flag; surface `kill <pid>` in modal | Round 3 |
| Tuple or named struct for `agent_connection`? | Named struct `ActiveConnection` | Round 4 |
| TTY-only or rich `HolderInfo`? | Rich `HolderInfo` with priority-rendered fields | Round 5 |
| Phase 1+2 atomic or split? | Split allowed; Phase 1b is a strict improvement on its own | Round 6 |
| AutoResume auto-fire Enter? | Never (preserves "no implicit attach" axiom) | Round 2 |
| Lock crate? | `fs4` (cross-platform; `fs2` is unmaintained) | Pre-MCTS gemini review |
| PID-liveness inference? | Never. Successful flock IS the signal | Pre-MCTS gemini review |

## 11. Non-Goals

- **Read-only spectator mode** for the second window. Considered (gemini's "missed alternative" from review #1); rejected for v1 as a much larger project. May be added in a future spec.
- **Per-session UDS sockets as a preemptible lock primitive.** Considered in Round 3; rejected as scope creep. Advisory locks + shell `kill` is sufficient.
- **Daemon-owned attachments.** Considered in review #1; rejected as architecturally invasive.
- **Telegram bot frontend lock interaction.** Out of scope; the bot frontend (`spur-bot`) attaches via a different code path and is not currently affected by this spec.
- **GC of stale lockfiles.** Kernel-released on process exit; the `.lock` file itself stays as a pid-content artifact. A periodic cleanup could be added later but is not required for correctness.
