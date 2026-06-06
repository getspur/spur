# Jute-App Kernel Supervision (Heartbeat-Driven Auto-Restart) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-01-jute-app-notebook-as-application-container-design.ipynb` (§8 Container lifecycle & supervision; §12 "Robustness you didn't build")

**Design epic:** n/a (audit-derived slice; see audit verdict 2026-06-06)

**Goal:** Detect a dead notebook kernel via the (currently unused) Jupyter heartbeat socket and automatically restart it through the existing restart recipe, closing the §8 supervision gap with no new bus.

**Architecture:** The Jupyter heartbeat `ReqSocket` is already connected in `create_zeromq_connection` but discarded (`driver_zeromq.rs:113 let _ = (stdin, heartbeat); // Not supported yet`). We add (1) a liveness probe loop that pings that socket and flips a `CancellationToken` on the `KernelConnection` after N consecutive misses; (2) a reusable `restart_kernel_in_slot` extracted from the existing `notebook.restart_kernel` MCP tool; (3) a per-slot supervisor task that awaits the liveness token and re-runs that recipe. The restart recipe already re-injects the port bootstrap, and view cells already rehydrate from `manifest.json`, so recovery reuses existing machinery rather than adding new infrastructure. No `ipc://` bus is introduced — this slice is deliberately bus-independent.

**Tech Stack:** Rust, `tokio`, `zeromq` crate, `tokio_util::sync::CancellationToken`, `async_channel`, existing `jute` crate (`crates/spur-notebook/jute-notebook/src-tauri`) + `spur-notebook` MCP crate.

**Invariants honored:** `manifest.json` stays the single source of truth (recovery re-reads it, never invents state); the heartbeat socket uses kernel auth (it is the Jupyter wire, not a new bus); changes are scoped to the kernel transport + slot lifecycle.

---

## File Structure Map

| File | Crate | Responsibility | Tasks |
|---|---|---|---|
| `crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs` | jute | `KernelConnection` gains a `liveness` token + accessors; pure death-decision helper | T1 |
| `crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol/driver_zeromq.rs` | jute | spawn the heartbeat probe loop instead of dropping the socket | T1 |
| `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` | jute | new reusable `restart_kernel_in_slot`; spawn the per-slot supervisor on install | T2, T3 |
| `crates/spur-notebook/src/mcp/tools/restart_kernel.rs` | spur-notebook | MCP tool delegates to `restart_kernel_in_slot` (behavior-preserving) | T2 |

---

## Task DAG

```
T1 (heartbeat liveness)  ─┐
                          ├─▶ T3 (supervisor loop)
T2 (restart recipe extr.)─┘
```

T1 and T2 are independent and dispatch in parallel. T3 joins both.

---

### Task 1: Heartbeat liveness detection in the ZeroMQ driver

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs` (`KernelConnection` struct + `impl`; add helper + unit test)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol/driver_zeromq.rs:75-113` (construct + spawn probe)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `KernelConnection` has a `liveness: CancellationToken` field, distinct from the existing `signal` field, plus `pub(crate) fn liveness_token(&self) -> CancellationToken` and `pub(crate) fn is_alive(&self) -> bool` (returns `!self.liveness.is_cancelled()`).
- [ ] Pure helper `heartbeat_declares_dead(consecutive_misses: u32, threshold: u32) -> bool` exists with a unit test covering below/at/above threshold.
- [ ] `driver_zeromq.rs` no longer contains `let _ = (stdin, heartbeat);`; the heartbeat `ReqSocket` is moved into a spawned probe loop that cancels `liveness` after `HEARTBEAT_MISS_THRESHOLD` consecutive misses, and exits when `signal` (the drop guard) is cancelled.
- [ ] The existing `for_test()` constructor (added in the C6 follow-up) is updated to build the new `liveness` field.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p jute backend::wire_protocol` passes; `SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -- -D warnings` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `wire_protocol.rs`, `driver_zeromq.rs` only.
- OUT of scope: `commands.rs`, `state.rs`, `restart_kernel.rs`, any supervisor wiring (that is T3). Do NOT call restart from here — this task only *signals* death.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing unit test** (in the existing `#[cfg(test)] mod tests` of `wire_protocol.rs`):

```rust
#[test]
fn heartbeat_declares_dead_only_at_or_above_threshold() {
    assert!(!heartbeat_declares_dead(0, 3));
    assert!(!heartbeat_declares_dead(2, 3));
    assert!(heartbeat_declares_dead(3, 3));
    assert!(heartbeat_declares_dead(5, 3));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute heartbeat_declares_dead -- --nocapture`
Expected: FAIL — `heartbeat_declares_dead` not found.

- [ ] **Step 3: Add the helper and the liveness field.** In `wire_protocol.rs`, near the other free helpers:

```rust
/// Consecutive heartbeat misses before a kernel is declared dead.
pub(crate) const HEARTBEAT_MISS_THRESHOLD: u32 = 3;
/// Interval between heartbeat pings.
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);
/// Per-ping timeout waiting for the kernel's echo.
pub(crate) const HEARTBEAT_RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

/// Pure decision: has the kernel missed enough consecutive heartbeats to be dead?
pub(crate) fn heartbeat_declares_dead(consecutive_misses: u32, threshold: u32) -> bool {
    consecutive_misses >= threshold
}
```

Add the field to `KernelConnection` (keep `#[derive(Clone)]`; `CancellationToken` is `Clone`):

```rust
    /// Fired by the heartbeat probe when the kernel stops echoing. Distinct from
    /// `signal` (which is the shutdown/drop guard for the whole connection).
    pub(crate) liveness: tokio_util::sync::CancellationToken,
```

And the accessors in `impl KernelConnection`:

```rust
    pub(crate) fn liveness_token(&self) -> tokio_util::sync::CancellationToken {
        self.liveness.clone()
    }

    pub(crate) fn is_alive(&self) -> bool {
        !self.liveness.is_cancelled()
    }
```

Update the `#[cfg(test)] fn for_test(...)` constructor to set `liveness: CancellationToken::new()`.

- [ ] **Step 4: Wire the probe in `driver_zeromq.rs`.** Construct the token before building `conn` (around line 79-89), add `liveness: liveness.clone()` to the `KernelConnection { .. }` literal, and replace line 113 (`let _ = (stdin, heartbeat);`) with a spawned loop. `stdin` remains unsupported, so keep `let _ = stdin;`:

```rust
    let liveness = CancellationToken::new();
    // ... add `liveness: liveness.clone(),` to the KernelConnection { .. } literal above ...

    let _ = stdin; // stdin replies not supported yet.

    // Heartbeat liveness probe: Jupyter HB is a strict REQ/REP echo. On a missed
    // echo we recreate the REQ socket (a timed-out REQ is left in a bad send state),
    // and after HEARTBEAT_MISS_THRESHOLD consecutive misses we declare the kernel dead.
    let hb_signal = signal.clone();
    let hb_liveness = liveness.clone();
    let hb_addr = format!("tcp://127.0.0.1:{heartbeat_port}");
    tokio::spawn(async move {
        let mut sock = heartbeat;
        let mut misses: u32 = 0;
        loop {
            tokio::select! {
                _ = hb_signal.cancelled() => break,
                _ = tokio::time::sleep(super::HEARTBEAT_INTERVAL) => {}
            }
            let ping = ZmqMessage::from(b"ping".to_vec());
            let ok = match sock.send(ping).await {
                Ok(()) => matches!(
                    tokio::time::timeout(super::HEARTBEAT_RECV_TIMEOUT, sock.recv()).await,
                    Ok(Ok(_))
                ),
                Err(_) => false,
            };
            if ok {
                misses = 0;
            } else {
                misses += 1;
                // Recreate the REQ socket after a failed exchange.
                let mut fresh = zeromq::ReqSocket::new();
                if fresh.connect(&hb_addr).await.is_ok() {
                    sock = fresh;
                }
                if super::heartbeat_declares_dead(misses, super::HEARTBEAT_MISS_THRESHOLD) {
                    warn!("kernel heartbeat lost; declaring dead");
                    hb_liveness.cancel();
                    break;
                }
            }
        }
    });
```

- [ ] **Step 5: Run to verify the helper test passes and the crate builds**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute heartbeat_declares_dead -- --nocapture`
Expected: PASS.
Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs \
        crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol/driver_zeromq.rs
git commit -m "feat(jute): task-1 heartbeat liveness probe on KernelConnection"
```

---

### Task 2: Extract reusable `restart_kernel_in_slot` from the MCP tool

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (add `restart_kernel_in_slot`)
- Modify: `crates/spur-notebook/src/mcp/tools/restart_kernel.rs:61-105` (delegate to it)

**Depends on:** none

**Acceptance Criteria:**
- [ ] New `pub async fn restart_kernel_in_slot(state: &Arc<State>, slot_id: &str, spec_name: &str) -> Result<u64, Error>` in `jute::commands` performs exactly the recipe currently inlined in `restart_kernel.rs:71-100` (take → kill → `start_local_kernel` → `inject_port_bootstrap` (kill on failure) → `install_kernel_in_slot`) and returns the new `generation`.
- [ ] `notebook.restart_kernel` MCP `call` resolves `spec_name` as today, then delegates to `restart_kernel_in_slot`, preserving its current JSON result `{ slot_id, generation }` and error messages.
- [ ] Behavior-preserving: the existing restart-kernel tests still pass unchanged.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p jute -p spur-notebook restart_kernel` passes; `SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -p spur-notebook -- -D warnings` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the extraction in `commands.rs` and the delegation in `restart_kernel.rs`.
- OUT of scope: `wire_protocol.rs`, `driver_zeromq.rs`, any supervisor loop, any liveness usage. Do NOT change the restart algorithm — this is a pure move + reuse.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Add the reusable function** in `jute-notebook/src-tauri/src/commands.rs`, lifting the body of `restart_kernel.rs:71-100` and using the already-exported helpers (`take_kernel_from_slot`, `start_local_kernel`, `inject_port_bootstrap`, `install_kernel_in_slot`, `notebook_path_from_slot_id`) plus the existing `notebook_port_root` helper:

```rust
/// Restart the kernel bound to `slot_id`: kill the prior process, start a fresh
/// kernel, re-inject the port bootstrap, and install it into the slot. Returns the
/// new slot generation. Shared by the `notebook.restart_kernel` MCP tool and the
/// heartbeat supervisor.
pub async fn restart_kernel_in_slot(
    state: &Arc<State>,
    slot_id: &str,
    spec_name: &str,
) -> Result<u64, Error> {
    let mut prior = take_kernel_from_slot(state, slot_id)?;
    prior.kill().await?;

    let port_root = notebook_path_from_slot_id(slot_id, spec_name).map(notebook_port_root);
    let mut kernel = start_local_kernel(spec_name, port_root.as_deref()).await?;
    if let Err(error) = inject_port_bootstrap(kernel.conn(), spec_name).await {
        let _ = kernel.kill().await;
        return Err(error);
    }
    let (generation, _previous) =
        install_kernel_in_slot(state, slot_id, spec_name.to_string(), kernel);
    Ok(generation)
}
```

Note: `notebook_port_root` currently lives in `spur-notebook` (`crate::dag::notebook_port_root`). Keep `restart_kernel_in_slot` in `jute` by inlining the port-root derivation it needs, OR move/duplicate the small `notebook_port_root` mapping into `jute::commands`. Prefer inlining the one-line mapping the recipe already uses so `jute` has no new dependency on `spur-notebook`. If this forces touching more than the two listed files, emit `scope_drift`.

- [ ] **Step 2: Delegate from the MCP tool.** Replace `restart_kernel.rs:71-105` so `call` keeps its param parsing + `spec_name` resolution, then:

```rust
    let generation = jute::commands::restart_kernel_in_slot(state, &params.slot_id, &spec_name)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.restart_kernel failed to restart kernel",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    Ok(CallToolResult::structured(json!({
        "slot_id": params.slot_id,
        "generation": generation,
    })))
```

- [ ] **Step 3: Run the existing restart tests (must stay green)**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute -p spur-notebook restart_kernel -- --nocapture`
Expected: PASS (unchanged behavior).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs \
        crates/spur-notebook/src/mcp/tools/restart_kernel.rs
git commit -m "refactor(jute): task-2 extract restart_kernel_in_slot for reuse"
```

---

### Task 3: Per-slot heartbeat supervisor (auto-restart on death)

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (spawn supervisor on kernel install; add a test with a fake decision path)

**Depends on:** task-1, task-2

**Acceptance Criteria:**
- [ ] When a kernel is installed into a slot (the `install_kernel_in_slot` path used by `start_local_kernel` callers and by `restart_kernel_in_slot`), a supervisor task is spawned that awaits `kernel.conn().liveness_token().cancelled()` and then calls `restart_kernel_in_slot(state, slot_id, spec_name)` exactly once for that death event.
- [ ] The supervisor does not loop hot: after a restart it relies on the new kernel's fresh liveness token (a new supervisor is spawned for the replacement kernel by the same install path); the prior supervisor task exits after one restart attempt.
- [ ] On restart failure, the supervisor logs an error (`tracing::error!`) and exits without panicking; the slot is left as-is for manual `notebook.restart_kernel`.
- [ ] A unit test `supervisor_restarts_once_when_liveness_cancelled` drives a `CancellationToken`, asserts the restart callback fires exactly once, using an injected restart closure (see Step 1) — no real kernel/socket required.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p jute supervisor -- --nocapture` passes; `SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -- -D warnings` clean.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p jute -p spur-notebook` is green end-to-end.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: supervisor spawn + its test in `commands.rs`.
- OUT of scope: changing the heartbeat probe (T1) or the restart recipe (T2); adding any `ipc://` bus; multi-client fan-out. Keep the supervisor strictly one-restart-per-death.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** (in `commands.rs` tests). Factor the wait-then-act core into a pure, injectable async helper so it is testable without a kernel:

```rust
#[tokio::test]
async fn supervisor_restarts_once_when_liveness_cancelled() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    let liveness = CancellationToken::new();
    let calls = Arc::new(AtomicU32::new(0));
    let calls_in = calls.clone();

    let token = liveness.clone();
    let handle = tokio::spawn(async move {
        supervise_until_dead(token, || {
            calls_in.fetch_add(1, Ordering::SeqCst);
            async { Ok::<(), Error>(()) }
        })
        .await;
    });

    liveness.cancel();
    handle.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute supervisor_restarts_once -- --nocapture`
Expected: FAIL — `supervise_until_dead` not found.

- [ ] **Step 3: Implement the injectable core + the spawn wiring.** In `commands.rs`:

```rust
/// Await a kernel's death (liveness cancelled), then invoke `restart` exactly once.
/// Pure of slot/state plumbing so it is unit-testable with a fake restart closure.
pub(crate) async fn supervise_until_dead<F, Fut>(
    liveness: tokio_util::sync::CancellationToken,
    restart: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), Error>>,
{
    liveness.cancelled().await;
    if let Err(error) = restart().await {
        tracing::error!(?error, "kernel supervisor restart failed");
    }
}
```

Spawn it wherever a freshly installed kernel becomes the slot's active kernel (the `start_local_kernel` + `install_kernel_in_slot` call sites, and inside `restart_kernel_in_slot`). Capture the data the closure needs by value (clone `Arc<State>`, `slot_id`, `spec_name`) so no borrow escapes the task:

```rust
let liveness = kernel.conn().liveness_token();
let sup_state = Arc::clone(state);
let sup_slot = slot_id.to_string();
let sup_spec = spec_name.to_string();
tokio::spawn(async move {
    supervise_until_dead(liveness, || async move {
        restart_kernel_in_slot(&sup_state, &sup_slot, &sup_spec).await.map(|_| ())
    })
    .await;
});
```

The replacement kernel installed by `restart_kernel_in_slot` carries its own fresh liveness token and gets its own supervisor via the same install path, so one supervisor handles exactly one death.

- [ ] **Step 4: Run the test + full crate suite**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute supervisor -- --nocapture`
Expected: PASS.
Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute -p spur-notebook`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs
git commit -m "feat(jute): task-3 per-slot heartbeat supervisor auto-restart"
```

---

## Self-Review

**Spec coverage (§8):** "worker heartbeat lost → Degraded → supervisor restarts worker + re-announces ports" → T1 detects loss (heartbeat socket now wired), T3 restarts (Degraded→Running), T2's recipe re-injects the port bootstrap and view cells rehydrate from `manifest.json` (existing behavior) = re-announce. The bus-level `pong` and the cross-process liveness in §8 are explicitly OUT of this slice (they require the `ipc://` bus, deferred per the audit). Stated here so the gap is not silent.

**Placeholder scan:** No TBD/TODO/"handle edge cases" — every code step is concrete; constants, signatures, and test bodies are spelled out.

**Type consistency:** `liveness_token() -> CancellationToken` (T1) consumed by T3; `restart_kernel_in_slot(&Arc<State>, &str, &str) -> Result<u64, Error>` (T2) consumed by T3 and by the MCP tool. `heartbeat_declares_dead(u32, u32) -> bool` used only in the T1 probe. Names match across tasks.

**DAG validation:** T1 ⟂ T2 (disjoint files, no shared symbols) → both root; T3 depends on both. Acyclic. Maximum parallelism (2 wide, depth 2).

**beads compatibility:** Each task has a unique ID, explicit `depends_on`, brain-verifiable acceptance criteria (named tests + clippy + green suite), and a scope boundary with a `scope_drift` checkpoint.

**Known risk:** the real heartbeat/restart loop cannot be exercised on a kernel-less CI VM; T1/T3 prove logic via pure helpers + injected closures (mirroring the C6 `afm_comm_send_e2e` graceful-skip pattern). A real-kernel integration test is a candidate follow-up, not part of this slice.
