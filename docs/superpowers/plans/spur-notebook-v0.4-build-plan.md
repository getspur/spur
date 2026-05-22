# SPUR Notebook — v0.4 Build Plan

**Status:** approved through M4. M5–M8 sketched.
**Scope:** analyst-only MVP. No worktree, no delegation, no review gate from the notebook.
**Authored:** 2026-05-22

---

## 1. Product

A native desktop notebook (Tauri shell + Jute UI) where a SPUR brain agent operates `.ipynb` cells on the root repo through an in-process MCP server. The notebook is the agent's working surface — markdown cells = working notes / reasoning artifacts, code cells = computation, cell outputs = shared memory between user + agent.

**Chat lives in the SPUR TUI, not in the spur-notebook GUI.** The spur-notebook Tauri window is purely a notebook editor/viewer; the user keeps chatting with the brain through the TUI's existing chat surface. spur-notebook runs as an always-on daemon subprocess of the TUI (see M6).

## 2. Roles

| Actor | Owns |
|---|---|
| User | Chat input (in TUI), direct cell editing (in spur-notebook GUI), kernel start/stop, save |
| Brain agent (Claude Code / Codex over ACP) | All MCP-driven cell ops; `.ipynb` is its working surface |
| Worker | Does not exist in MVP |

## 3. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ spur-notebook (single Tauri process)                        │
│                                                             │
│  React UI ◄──► Tauri events / commands ◄──► JuteCore (kern) │
│                       ▲                                     │
│                       │ agent://request / agent_response    │
│                       ▼                                     │
│                  AgentBridge (Rust)                         │
│                       ▲                                     │
│                       │ tool calls                          │
│                       ▼                                     │
│   spur-notebook/src/mcp/  (the "notebook" MCP server)       │
│        ▲  unix socket: ~/.spur/notebooks/<id>.sock          │
└────────┼────────────────────────────────────────────────────┘
         │ MCP (JSON-RPC framed)
         ▼
   Brain agent (Claude Code / Codex)
     MCP clients: notebook + spur-mcp (existing)
```

**Authority:** JavaScript (React + Zustand) owns notebook content. Rust holds kernel state, the MCP server, and the agent bridge. MCP tool handlers round-trip through `AgentBridge` → Tauri event → JS handler → response.

## 4. State Ownership

| State | Owner | Persistence | Lost on |
|---|---|---|---|
| Notebook content (cells, outputs) | JS Zustand store (upstream Jute pattern) | Saved to `.ipynb` via Rust `save_to_disk` | nothing |
| `cell.version` (per-cell monotonic, source/order/type only) | JS Zustand | Persisted in `metadata.spur.version` | Reset on app reopen (no consequence) |
| Kernel process + kernel slot | `JuteCore` (Rust, upstream) | None (process) | Crash, user restart, app quit |
| Kernel generation (per slot) | `JuteCore` (Rust, `AtomicU64`) | In-memory; resets to 1 on app start | App restart |
| Brain ACP session | `.spur/sessions/<id>/` (existing SPUR) | Disk | Explicit "new session" |
| Pending MCP requests | `AgentBridge` (Rust) `HashMap<id, oneshot::Sender>` | None | App restart (drained with `app_restarted` error) |
| Autosave buffer | Last debounced JS snapshot | `.ipynb` atomic write | nothing (worst case = last few keystrokes) |

## 5. Brain MCP Tool Surface (final v0.4 MVP)

| Tool | Purpose | Lands in |
|---|---|---|
| `notebook.snapshot` | Coarse list of all cells with `id`, `kind`, `version`, `exec_count`, `status`, `source_preview`, `source_hash` | M3 |
| `notebook.read_cell` | Full source + outputs for one cell | M3 |
| `notebook.kernel_info` | `{kernel_id, spec_name, generation, status, cpu_pct, mem_mb}` | M3 |
| `notebook.insert_cell` | Insert after `after_id?` | M5 |
| `notebook.write_cell` | Replace source; `expected_version` enforced | M5 |
| `notebook.delete_cell` | Delete; `expected_version` enforced | M5 |
| `notebook.run_cell` | Run code cell; streams `RunCellEvent`s via MCP progress | M5 |
| `notebook.interrupt` | Signal kernel interrupt | M5 |

Brain-visible MCP server name: `notebook`. Tool prefix: `notebook.`.

**Excluded from MVP:** `notebook.read` (full doc — replaced by `snapshot` + per-cell reads), `notebook.subscribe`, `notebook.propose_execution`, `notebook.save_as`, `kernel.restart` (user-driven via UI).

## 6. Bridge Contract (lifecycle)

Round-trip primitive between MCP tool handlers and JS Zustand.

**States:** `pending: HashMap<RequestId, oneshot::Sender<BridgeResponse>>`, `listener_registered: AtomicBool`, `window_alive: AtomicBool`.

**Error surface (mapped to MCP error codes):**
| `BridgeError` | MCP error code | Semantics |
|---|---|---|
| `WindowClosed` | `app_restarted` | Terminal — brain stops |
| `AppRestarted` | `app_restarted` | Drained on shutdown |
| `NoListener` | `service_starting` | Retryable |
| `Timeout` | `bridge_timeout` | Brain decides |
| `Handler { code, ... }` | passthrough (e.g. `stale_version`) | JS-side errors |

**Atomic-handler invariant:** JS handler must run version-check + mutation synchronously, no `await` in between. Documented at top of `crates/spur-notebook/jute-notebook/src/agent/handlers.ts`.

**Listener handshake:** JS calls `bridge_ready` Tauri command after registering its `agent://request` listener. Prior to that, every `bridge.request()` returns `NoListener` immediately (no emit, no timeout wait).

## 7. Concurrency Model

Single user, single brain. No hard locks.

- User edits flow: CodeMirror → Tauri command → Zustand (no Rust round-trip on the hot path).
- Brain writes flow: MCP → `AgentBridge` → `agent://request` event → JS handler → Zustand → `agent_response` → MCP.
- Both increment `cell.version`. Brain passes `expected_version`; mismatch → `stale_version` error; brain re-reads and reconciles.
- `cell.version` tracks source/order/type only. Output bursts and run metadata do **not** bump it (so agent edits don't go stale during kernel output streaming).

## 8. The Refresh Contract

System prompt mandates: **every brain turn begins with `notebook.snapshot` + `notebook.kernel_info`**. The brain notes the `kernel_info.generation` value; if it differs from the value seen on a prior turn, the kernel was restarted between turns — variables from prior cells are gone. The brain re-runs cells whose variables it needs before referencing them. On the first turn (no prior generation), the brain proceeds optimistically and recovers from any `NameError`.

Cost per turn under v0.4: **1 snapshot + 1 kernel_info + 1–3 `read_cell` = 3–5 round-trips**. Acceptable at human-conversation cadence.

## 9. The Kernel Generation Model

A single kernel-level signal, no per-cell apparatus.

- `kernel.generation` lives on the kernel slot (`JuteCore`-owned), not the `LocalKernel` instance. `AtomicU64`. Starts at 1; increments on every (re)start of the slot's kernel.
- Triggers: first start, explicit restart, kernel death + restart, spec change.
- Exposed via `notebook.kernel_info.generation`.
- **No `cell.last_run_epoch`. No per-cell stale-detection. No persisted metadata.**
- The brain compares generation values across its own turns (held in conversation context). If the brain sees a generation different from the one it recorded last turn, the kernel restarted and prior-cell variables are gone.
- First-turn / fresh-session case: brain has no prior generation; proceeds optimistically; recovers from any `NameError` by re-running the relevant cells.
- App restart resets `kernel.generation` to 1. Brain's new ACP session also starts fresh; no cross-session continuity required.

Rationale: codex review (2026-05-22) found that pure reactive `NameError` recovery accumulates confusion under frequent restart, but per-cell epoch is more apparatus than v0.4 MVP needs. A single kernel-level generation field is the minimum proactive signal.

## 10. Build Sequence

| M | Deliverable | Effort | Status |
|---|---|---|---|
| M1 | Vendor Jute via git-subtree under `crates/spur-notebook/jute-notebook/`; add `crates/spur-notebook/` binary; workspace builds green on macOS; pre-commit verification checklist (see §17) passes | ~0.5 day | approved |
| M2 | `cell.version` field in Zustand (source/order/type only); stable cell IDs verified; Rust-side atomic `save_to_disk` Tauri command with coalesce policy | ~0.5 day | approved |
| M3 | `crates/spur-notebook/src/mcp/` module; `AgentBridge` with full lifecycle; unix socket transport; `notebook.snapshot`, `notebook.read_cell`, `notebook.kernel_info` (with real `generation: AtomicU64` on kernel slot, incrementing on every restart/spec-change); JS-side bridge dispatcher + atomic handlers + `bridge_ready` signal; kernel pill shows generation | ~2 days | approved |
| ~~M4~~ | (deleted — folded into M3) | — | — |
| M5 | Write tools (`insert_cell`, `write_cell`, `delete_cell`) with `expected_version` enforcement; `run_cell` with streamed `RunCellEvent` via MCP progress; `interrupt`; iopub stream multicast to both JS Channel and MCP progress | ~2 days | sketched |
| M6 | **Always-on `spur-notebook` daemon subprocess** spawned at TUI start; brain config pre-includes `notebook` MCP server pointing at daemon's stable socket; brain's MCP config never changes; user chats in TUI throughout; `/notebook [path \| new \| close]` TUI palette command sends `daemon://` control messages to load/create/close a notebook and show the Tauri GUI window; closing GUI keeps daemon + notebook loaded; daemon graceful-shutdown on TUI exit; system prompt includes NOTEBOOK AVAILABILITY paragraph | ~1 day | sketched |
| M7 | Thin-chat structural cap (240-char chat response, expand-to-cell affordance); cells reframed as "working notes / reasoning artifacts"; agent-edit indicators on cells | ~1 day | sketched |
| M8 | Restart-resume polish (resume brain session, restore notebook, kernel generation reset behavior); autosave correctness; multi-window deferred | ~1 day | sketched |

**Total v0.4 effort estimate:** ~8 days of focused work.

## 11. Crate / Module Layout

```
crates/
├── spur-notebook/                          # the app
│   ├── Cargo.toml                          # binary, depends on jute-notebook
│   ├── src/
│   │   ├── main.rs                         # Tauri app entry
│   │   └── mcp/                            # M3: the "notebook" MCP server (module)
│   │       ├── mod.rs
│   │       ├── bridge.rs                   # AgentBridge
│   │       ├── transport.rs                # unix socket
│   │       └── tools/
│   │           ├── snapshot.rs             # M3
│   │           ├── read_cell.rs            # M3
│   │           ├── kernel_info.rs          # M3 (with generation)
│   │           ├── insert_cell.rs          # M5
│   │           ├── write_cell.rs           # M5
│   │           ├── delete_cell.rs          # M5
│   │           ├── run_cell.rs             # M5
│   │           └── interrupt.rs            # M5
│   └── jute-notebook/                      # vendored upstream Jute (sub-crate)
│       ├── VENDOR.md                       # subtree pin + upstream SHA
│       ├── package.json                    # Vite / React / CodeMirror deps
│       ├── vite.config.ts
│       ├── tailwind.config.js
│       ├── tsconfig.json
│       ├── src/                            # React UI
│       │   ├── agent/                      # M3+: bridge.ts, handlers.ts, types.ts
│       │   ├── stores/notebook.ts          # M2: cell.version field
│       │   └── ui/notebook/                # CellInput, OutputView, etc.
│       └── src-tauri/
│           ├── Cargo.toml                  # the Jute Rust lib (workspace member)
│           ├── tauri.conf.json
│           └── src/                        # LocalKernel, ZMQ drivers, kernel slot generation
└── (existing spur-* crates, untouched)
```

Naming convention:
- **Separate crate** when the surface has multiple consumers (`spur-mcp`).
- **Module** when there's one consumer (`spur-notebook/src/mcp/` = "the `notebook` MCP server").
- **Sub-crate nesting** when a vendored or app-specific Rust unit exists only to power one app (`spur-notebook/jute-notebook/` = "the Jute frontend powering SPUR Notebook").

Workspace member paths in root `Cargo.toml`:

```toml
[workspace]
members = [
    # ...existing members...
    "crates/spur-notebook",
    "crates/spur-notebook/jute-notebook/src-tauri",
]
```

## 12. Configuration

`.spur/notebook.toml`:

```toml
[brain]
agent = "claude-code"
extra_args = []

[kernel]
default_spec = "python3"
restart_on_app_start = false

[ui]
# chat_response_char_cap applies in the SPUR TUI's chat rendering (M7).
# The spur-notebook GUI has no chat surface.
chat_response_char_cap = 240

[autosave]
interval_secs = 5

[bridge]
request_timeout_secs = 30
```

## 13. System Prompt (final)

```
You are the notebook agent for <path>.
Working directory: <repo root>.
You operate the notebook via the `notebook` MCP server.

REFRESH CONTRACT
At the start of every turn, call notebook.snapshot to see current cells.

KERNEL CONTINUITY
At the start of every turn, also call notebook.kernel_info. Note the
`generation` value. If it differs from the value you saw on a prior
turn, the kernel was restarted between turns — variables from prior
cells are gone. Re-run any cells whose variables you need before
referencing them. If you have no prior generation (first turn or new
session), just proceed and recover from any NameError.

WORKING SURFACE
The notebook is your working surface. Put reasoning in markdown cells
(working notes / reasoning artifacts) and computation in code cells.
Cell outputs are your shared memory with the user.

CHAT
Chat (visible to the user in the SPUR TUI, not the notebook GUI) is a
control channel. Reply in one or two sentences pointing at the cells
you wrote. If a reply would be longer, write a markdown cell and
reference it.

EDITING
- Append new cells; do not rewrite user-authored cells without explicit
  request.
- Use expected_version on write_cell / delete_cell. On stale_version,
  re-read and reconcile.

KERNEL
- Run cells you author; check outputs via read_cell after run.
- If a cell fails, fix it in place or insert a diagnostic cell. Do not
  summarize errors in chat alone.

NOTEBOOK AVAILABILITY
The `notebook.*` MCP server is always reachable, but a notebook may
not be loaded. If a notebook tool returns `notebook_not_open`, tell
the user in chat: "I need a notebook open to do that — try
`/notebook <path>` or `/notebook new`."

OUT OF SCOPE
You do not modify files outside the notebook. You may use spur-mcp
code-graph tools for read-only repo navigation. Do not call
delegate_to_worker — this is analyst mode.
```

## 14. Out of Scope (v0.4)

| Excluded | Rationale | Revisit when |
|---|---|---|
| `propose_execution` / Deliver / worktree path from notebook | Analyst-only product | Users repeatedly ask "how do I ship the fix?" |
| Watch mode (brain wakes on user cell-runs) | Burns brain attention; cost unbounded | Silent drift becomes a felt problem |
| Plan panel | Cells ARE the plan in analyst mode | Multi-step plans become common |
| Diff panel | Nothing produces a diff | If/when execution mode returns |
| Remote kernels | Jute supports it; not needed for MVP | Server-side compute becomes a need |
| Multi-window | Single notebook keeps lifecycle trivial | Power users complain |
| LSP cells | Orthogonal | After M8 |
| Beads writes for analyst activity | Event log + `.ipynb` are sufficient | Org-level tracking matters |
| Brain → SpurEvent funnel forwarding | TUI doesn't need to mirror notebook | Multi-frontend observability is a goal |
| Windows builds | macOS + Linux first | Demand |

## 15. Risks Tracked

1. **Brain regresses to prose in chat.** Mitigation: 240-char cap (M7) + expand affordance. Validate in M7 user testing.
2. **`expected_version` retries become chatty.** Mitigation: refresh contract means brain sees latest version on turn start; 409 should be rare. Monitor in M6.
3. **JS-thread blocking on slow MCP handlers.** Mitigation: atomic-handler invariant + 30 s timeout. Linted by review. If a handler ever needs IO, take snapshot first then IO outside atomic section.
4. **Kernel restart mid-brain-turn invalidates assumptions.** Mitigation: brain may re-read `kernel_info` between major reasoning steps, not just at turn start. Adjust prompt if observed.
5. **`.ipynb` metadata grows from `version` (and M7's `origin`/`last_edited_by`) per cell.** A handful of small fields per cell. Non-issue.
6. **Save coupling with kernel iopub burst.** Mitigation: Rust-side atomic save with coalesce policy (only one save in flight; queued saves collapse to "latest wins").
7. **Bridge timeout misclassification.** Mitigation: 5 distinct `BridgeError` variants mapped to distinct MCP error codes; brain handles each appropriately.
8. **Choice of MCP server crate.** Spike in M3 to pick one (rmcp candidate); fall back to ~500 LOC custom impl if needed.

## 16. Open Questions

- **Notebook autosave format on partial JS crash.** Atomic rename ensures the file is never half-written, but a JS crash mid-debounce loses ≤5 s of edits. Document; revisit only if users feel it.
- **Multiple `spur-notebook` windows = multiple sockets.** v0.4 supports one window. Document; M8 polish (or v0.5) handles multi-window.
- **Coarse snapshot includes `source_hash`?** Decided: yes, BLAKE3-16. Lets the brain detect content changes without full reads.
- **Brain crash mid-tool-call.** Pending MCP request times out; JS handler completes harmlessly. Documented in bridge contract.
- **Should the brain be allowed to edit user-authored cells?** System prompt says no without explicit user request. Not enforced by the tool layer (open).
- **Kernel auto-restart on death.** Out of scope for v0.4; relies on existing Jute behavior. Brain detects `kernel_info.status == "dead"` and asks user.

## 17. Verification Fixtures

**M1 pre-commit verification checklist** (Amendment B: nested layout). Before merging M1, confirm:

- [ ] `cargo metadata --workspace --no-deps` lists both `spur-notebook` and the Jute Rust lib at `crates/spur-notebook/jute-notebook/src-tauri`
- [ ] `cargo build -p spur-notebook` builds clean from repo root
- [ ] Tauri dev (`npm run tauri dev` from `crates/spur-notebook/jute-notebook/`) launches the Jute app exactly as upstream
- [ ] Vite dev server resolves `@` to `crates/spur-notebook/jute-notebook/src/` (smoke test: change a class in a component, verify HMR)
- [ ] Tailwind scans the nested source tree (smoke test: introduce a new utility class, verify it compiles)
- [ ] `tauri.conf.json` paths resolve from the new location (`dist`, `frontendDist`, `beforeBuildCommand`)
- [ ] `git subtree pull --prefix=crates/spur-notebook/jute-notebook jute main --squash` reaches upstream cleanly (dry-run before commit)

If any check fails, fall back to the **flat top-level layout** at `crates/jute-notebook/` (peer of `crates/spur-notebook/`, not nested under it) and document the reason in `VENDOR.md`.

**M3 smoke test** (manual): with `spur-notebook` running and a `.ipynb` open, an external Claude Code launched with the generated MCP config can:

1. Call `notebook.snapshot` → returns array of cells.
2. Call `notebook.kernel_info` → returns `{generation: 1, status: "idle", ...}`.
3. Call `notebook.read_cell { id: <first cell id> }` → returns full source + outputs.
4. Restart kernel via UI → next `notebook.kernel_info` returns `{generation: 2, ...}`.

**M6 smoke tests** (always-on daemon model):

*TUI start spawns daemon:*

1. Launch SPUR TUI. Verify a `spur-notebook --headless` child process is running. Daemon socket file exists at `~/.spur/notebooks/<session>.sock`. Brain spawn logs show `notebook` MCP server in config.
2. Without opening any notebook, brain calls `notebook.kernel_info` → returns `notebook_not_open` (handled gracefully per NOTEBOOK AVAILABILITY prompt section).

*/notebook foo.ipynb opens GUI + loads file:*

3. User in TUI: Ctrl+K → `/notebook foo.ipynb`. Within ~2s, Tauri GUI window appears showing foo.ipynb. Brain's next `notebook.snapshot` returns the cells. Brain PID unchanged from step 1.

*GUI close keeps brain working:*

4. Close the GUI window. Daemon process stays alive. Brain calls `notebook.snapshot` → still returns foo.ipynb's cells. Brain calls `run_cell` → succeeds (kernel + state still loaded in daemon).

*GUI reopen shows current state:*

5. `/notebook` (no arg) → daemon reopens GUI with foo.ipynb still loaded. Cells the brain inserted while GUI was closed are visible.

*Switch notebooks:*

6. `/notebook bar.ipynb` → daemon saves foo.ipynb, swaps to bar.ipynb, GUI updates. Brain's next snapshot reflects bar.

*New untitled notebook:*

7. `/notebook new` → daemon creates `~/.spur/scratch/<uuid>.ipynb`, opens GUI. Brain operates normally; first save prompts for filename.

*Close notebook (no file):*

8. `/notebook close` → daemon saves current notebook, clears state. Brain's `notebook.*` calls return `notebook_not_open`.

*Daemon crash recovery:*

9. Kill daemon externally. TUI detects via socket-down within ~2s and respawns. Brain's MCP client reconnects automatically. Notebook state is lost; `/notebook <path>` reloads.

*TUI exit cleanup:*

10. Quit TUI cleanly. Daemon receives shutdown control message, saves any open notebook, exits within ~1s. No orphan process.

*Brain PID stability:*

11. Across steps 1–10, the brain process PID is constant. Verify via process table at each step.

**M5 smoke test:**

1. Brain calls `insert_cell { kind: "code", source: "x = 2 + 2" }` → returns `{id: c1, version: 1}`.
2. Brain calls `run_cell { id: c1 }` → streams `Started`, then `Finished { exec_count: 1, status: "ok" }`.
3. Brain calls `insert_cell { after_id: c1, kind: "code", source: "print(x)" }` → returns `{id: c2}`.
4. Brain calls `run_cell { id: c2 }` → streams output "4", then `Finished { ok }`.
5. Brain calls `write_cell { id: c2, source: "print(x + 1)", expected_version: 1 }` → returns `{version: 2}`.
6. Brain calls `run_cell { id: c2 }` → streams "5".
7. User restarts kernel; brain calls `kernel_info` → sees `generation` changed; on next `run_cell { id: c2 }` either re-runs `c1` first or recovers from `NameError`.

If this sequence runs end-to-end with no manual coaching, M5 is done.

## 18. v0.5 Preview (out of v0.4 scope, documented for context)

- `notebook.subscribe` resource (streaming cell-state changes to brain).
- Rust-side notebook mirror for hot reads (if measured slow).
- Watch mode (brain auto-wakes on user cell-runs).
- `propose_execution` + Deliver button + worktree-bridged execution mode.
- Multi-window support.
- Notebook share/export.

### Yjs / CRDT migration path

v0.4 uses optimistic `expected_version` + 409 conflict protocol for cell mutations. Industry direction (validated against JupyterLab v4, Jupyter AI v3, jupyter-mcp-server, and recent "AI agents as CRDT peers" work) is to back the notebook with a Yjs CRDT so user and agent edit the same shared document with natural conflict resolution.

Migration shape (informational, not committed):

- Add a Yjs document layer in `crates/spur-notebook/jute-notebook/src/`. The Zustand store becomes a projection of the Yjs doc, not the source of truth.
- The MCP write tools (`write_cell`, `insert_cell`, `delete_cell`) become Yjs ops emitted into the same doc as the user's CodeMirror edits. `expected_version` drops; CRDT merge handles the race.
- Brain becomes a "CRDT peer" semantically — its edits arrive on the doc with a known authorship tag.
- The `AgentBridge` round-trip protocol stays (handshake, drain, timeouts); only the write payloads change shape.

The v0.4 architecture does not block this migration. `cell.version` is removable without breaking anything else.

### Permission gate on `run_cell`

Jupyter AI v3 ships per-action approval ("agents request approval before writing files or executing commands"). For v0.4 we deferred this — analyst-only scope and a single trusted brain make it unnecessary. For v0.5, a lightweight permission policy on `run_cell` is worth adding before any multi-brain or remote-worker integration:

- Per-cell or per-pattern approval (e.g., "auto-approve cells matching `print(*)`, prompt for `subprocess.*` or `os.remove`").
- Brain receives a structured `permission_required` MCP error; UI surfaces a yes/no toast.

Out of scope for v0.4; placeholder here so future contributors don't accidentally lock the architecture against it.

### Industry-validated shape

Research conducted 2026-05-22 surveyed Cursor (broken native + MCP workaround), VS Code Copilot (open agent-mode bugs), Claude Code's `NotebookEdit` (format-drift), Cline (broken `replace_in_file`), Jupyter AI v3, Notebook Intelligence, datalayer/jupyter-mcp-server, marimo, and Hex/Deepnote/Noteable. Conclusion: **direct .ipynb file editing is a known failure mode at every product that ships it; MCP-based cell manipulation is the convergent industry pattern.** The v0.4 architecture (ACP brain + MCP server inside Tauri host + JS-authoritative state) mirrors Jupyter AI v3 closely. SPUR Notebook's unique value is the integration with SPUR's broader orchestration (delegation, plan, beads, code-graph), not the notebook UX itself — that needs to be adequate, not novel.

---

## Decision Log (for plan-drift-auditor)

| Date | Decision | Rationale |
|---|---|---|
| 2026-05-22 | Scope to analyst-only; no worktree in MVP | User: avoid state management complexity |
| 2026-05-22 | Brain agent IS the notebook operator (no worker subprocess for analyst) | User: simplify operation model |
| 2026-05-22 | Drop `propose_execution` from MVP | Analyst-only product; execution belongs to existing SPUR flows |
| 2026-05-22 | Keep JS (React + Zustand) as authority for notebook content | User: minimize diff vs upstream Jute |
| 2026-05-22 | Defer `AgentBridge` from M2 to M3 | Codex review: bridge contract should be shaped by real ops, not ping smoke test |
| 2026-05-22 | Add `notebook.snapshot` as coarse refresh op (M3) | Codex review: avoid N round-trips per turn |
| 2026-05-22 | `cell.version` tracks source/order/type only, not output bursts | Codex review: prevent spurious `stale_version` |
| 2026-05-22 | Kernel generation lives on kernel slot, not `LocalKernel` instance | Survives restart; semantically tied to the notebook↔kernel relationship |
| 2026-05-22 | Rust-side atomic save with coalesce policy | Codex review: large iopub bursts shouldn't stutter saves |
| 2026-05-22 | MCP server name = `notebook`; module lives in `spur-notebook/src/mcp/` not a peer crate | User: avoid overlap with `spur-mcp`; module pattern when there's one consumer |
| 2026-05-22 | **Drop per-cell `last_run_epoch`; keep only kernel-level `generation`** (Amendment A → A-lite) | Codex review: per-cell epoch is more apparatus than v0.4 MVP needs; pure reactive `NameError` recovery is too weak; one kernel-level signal is the minimum proactive contract. M4 deleted; generation work folds into M3. |
| 2026-05-22 | **Nest `jute-notebook/` under `crates/spur-notebook/`** (Amendment B) | User: dependency direction and product ownership clearer with nesting. Codex review: approved with pre-commit verification checklist (root Cargo, Tauri cwd, Vite `@`, Tailwind scan, subtree prefix). |
| 2026-05-22 | **Industry research conducted; v0.4 architecture validated against Jupyter AI v3** | Surveyed 8+ products. Direct .ipynb file editing is known-broken in every product that ships it (Cursor, VS Code Copilot, Claude Code NotebookEdit, Cline). MCP-based cell manipulation is the convergent industry pattern. v0.4 design matches Jupyter AI v3's ACP + MCP + permission-gate shape. Differentiation is SPUR-orchestration integration, not notebook UX. |
| 2026-05-22 | **Add Yjs/CRDT migration path to v0.5 preview** (informational) | Industry direction is agent-as-CRDT-peer (JupyterLab v4 uses Yjs; jupyter-mcp-server piggybacks on it). v0.4's `expected_version` is MVP equivalent; migration is documented but not committed. |
| 2026-05-22 | **Add permission-gate placeholder to v0.5 preview** (informational) | Jupyter AI v3 ships per-action approval. Deferred for v0.4 (single trusted brain, analyst-only). Captured so future contributors don't lock architecture against it. |
| 2026-05-22 | ~~**TUI→Notebook handoff via Ctrl+K `/notebook` palette command**~~ ~~Session-resume across frontends; brain process unchanged; MCP config regen with notebook server added.~~ | **SUPERSEDED** — design relied on (a) hot-reloading the brain's MCP config (Claude Code/Codex don't support without process restart) and (b) a `spur-core::Orchestrator::resume_session(id)` primitive that was assumed to exist but was not verified in the codebase. Audit (2026-05-22) confirmed no such method. See next row for the replacement design. |
| 2026-05-22 | **Always-on `spur-notebook --headless` daemon subprocess; `/notebook` is a daemon control command, not a handoff** | Replaces the handoff design. Daemon spawned as child of TUI at TUI start; hosts notebook-mcp server on a stable socket; brain's MCP config pre-references it and never changes; user chats in TUI throughout; `/notebook foo.ipynb \| new \| close` is a `daemon://` control message that loads/creates/closes a notebook and shows/hides the Tauri GUI window. Closing the GUI window keeps the daemon and notebook state alive — brain continues operating. No brain restart, no session-lock dance, no MCP reconfig. Daemon graceful-shutdown on TUI exit. |
