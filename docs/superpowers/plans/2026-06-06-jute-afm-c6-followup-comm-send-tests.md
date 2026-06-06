# Jute-App AFM C6 follow-up: comm-send seam tests — Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`. Each task becomes a beads issue with `spur:plan-task-id` / `spur:plan-id` labels. Base pinned to the commit that contains C6 (`75da545a` or its descendant — re-pin if a concurrent writer advanced main).

**Source:** C6 audit residual gap #1 (3-phase audit of plan `87d4587e`, landed `75da545a`). The production comm-send path is type-checked but its *behavior* is only covered through a `RecordingCommGateway` double; `build_comm_msg`+`call_shell` are tested in isolation. There is no test that drives the **real** send path to actual `comm_msg` bytes, nor one that drives the **real** `JuteModelStateCommGateway` to a live kernel.

**Goal:** Lock the comm-send seam with two complementary tests — (1) a fast, deterministic jute unit test that asserts the *real* `send_comm_msg` body emits a correct `CommMsg` on the shell channel, and (2) a real-kernel integration test that drives the *real* gateway end-to-end (frontend intent → `JuteModelStateCommGateway` → `jute::commands::send_comm_msg` → live `ipykernel` comm).

**Architecture:** Two independent test seams. Task 1 splits a conn-level helper (`send_comm_msg_on_conn`) out of `send_comm_msg` and adds a `#[cfg(test)]` `KernelConnection` constructor that exposes the shell receiver — no process needed. Task 2 reuses the existing `rust_go_ports_e2e.rs` real-kernel harness and adds a `pub` entry into the AFM handler so an integration test can call the real default gateway.

**Tech Stack:** Rust 2021, `async_channel`, `tokio::test`, `serde_json`, `ipykernel` (Python, via existing kernel-provision test harness). Build/test through `scripts/spur-cargo` only.

---

## Grounded facts (HEAD 75da545a, graph hash b648a856 — fresh, analyst-aligned)

- `KernelConnection` (`crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs:597`) has **all-private fields** (`shell_tx: async_channel::Sender<KernelMessage>`, `control_tx`, `iopub_tx`, `process_stderr_tx`, `reply_tx_map: Arc<DashMap<...>>`, `signal: CancellationToken`, `_drop_guard: Arc<DropGuard>`); `#[derive(Clone)]`; no public constructor. Only code in `wire_protocol.rs` can build one.
- `KernelConnection::call_shell<T: Serialize>` (`wire_protocol.rs:609`) sends `message.into_json()` over `self.shell_tx` and registers a `oneshot` reply in `reply_tx_map`. The `shell_tx` receiver is the observable seam.
- `build_comm_msg(comm_id, data, buffers) -> KernelMessage` (`wire_protocol.rs:196`) is already covered in isolation by `build_comm_msg_sets_wire_type_content_and_buffers`.
- jute `send_comm_msg(state, slot_id, comm_id, data, buffers)` (`.../src-tauri/src/commands.rs:1651`) = `kernel_connection_for_slot(state, slot_id)?` then `conn.call_shell(build_comm_msg(...)).await?`.
- `kernel_connection_for_slot` (`commands.rs:1258`) reads `state.kernels.get(slot_id)?.kernel.as_ref()?.conn().clone()`. `KernelSlot.kernel: Option<LocalKernel>`; `LocalKernel { child: tokio::process::Child, kernel_id, spec, conn }`. **`child` is a real OS process handle with no fake constructor → a `state.kernels` slot cannot be fabricated without spawning a process.** Hence Task 1 tests at the conn level; the state+slot hop is covered by Task 2's real kernel.
- spur-notebook `handle_anywidget_command_intent(&state, engine, intent)` (`crates/spur-notebook/src/commands.rs:144`) is the crate-private core that defaults to the real `JuteModelStateCommGateway`; the public `#[tauri::command] anywidget_command` (`commands.rs:131`) wraps it but takes `tauri::State<…>` (not callable from an integration test). Making the core `pub` is the minimal integration seam.
- Real-kernel harness to model on: `crates/spur-notebook/tests/rust_go_ports_e2e.rs` — builds `ServerDeps`, calls `tools::start_kernel` / `tools::run_cell` / `tools::stop_kernel` against a real `python3` kernel, gated to **skip gracefully when python3 is unavailable** (`python_binary_for_test`). The python3 kernelspec is `ipykernel`, so `from ipykernel.comm import Comm` is available with no extra deps.

---

## Task DAG

Two independent tasks, **no dependencies** (different crates, different seams) — dispatch in parallel.

```
task-1-jute-conn-wire-test   (jute)            — no deps
task-2-real-kernel-e2e       (spur-notebook)   — no deps
```

---

### Task 1: jute conn-level comm-send wire test

**Task ID:** `task-1-jute-conn-wire-test`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (split `send_comm_msg`; add test)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs` (add `#[cfg(test)]` `KernelConnection` test constructor)

**Depends on:** none

**Suggested Worker:** codex (mechanical, single-crate, well-specified)

**Scope Boundary:**
- IN scope: the two files above, test code, and the minimal `send_comm_msg` refactor.
- OUT of scope: `state.rs`, the spur-notebook crate, the reactive engine, `PortStore`/Arrow. Do not change `call_shell`/`build_comm_msg` behavior.
- If you discover you must touch OUT-OF-SCOPE files, emit `scope_drift`.

**Acceptance Criteria:**
- [ ] `send_comm_msg` delegates to a new `send_comm_msg_on_conn(&KernelConnection, comm_id, data, buffers)`; external signature/behavior of `send_comm_msg` unchanged.
- [ ] A `#[cfg(test)]` constructor on `KernelConnection` returns a connection plus the `shell_tx` receiver so tests can observe sent messages.
- [ ] A new test asserts the real `send_comm_msg_on_conn` emits exactly one shell message that deserializes to `msg_type == CommMsg` with the right `comm_id`, `data`, and envelope `buffers`.
- [ ] `scripts/spur-cargo build -p jute` and `scripts/spur-cargo test -p jute` are green (paste output).
- [ ] No NEW clippy warnings (`SPUR_REMOTE=1 scripts/spur-cargo clippy -p jute -- -D warnings`; pre-existing jute backlog is out of scope — note it, don't fix it).

**Implementation:**

- [ ] **Step 1: Write the failing test** (in `wire_protocol.rs` `mod tests`, alongside `build_comm_msg_sets_wire_type_content_and_buffers`). The `#[cfg(test)]` constructor must live in `wire_protocol.rs` because `KernelConnection`'s fields are private to that module.

```rust
// In wire_protocol.rs, inside `impl KernelConnection` guarded by #[cfg(test)],
// OR as a #[cfg(test)] free fn in the same module. Construct every field with
// throwaway endpoints and hand back the shell receiver.
#[cfg(test)]
impl KernelConnection {
    /// Build a connection wired to in-memory channels for tests.
    /// Returns (conn, shell_rx) so the test can observe shell sends.
    pub(crate) fn for_test() -> (Self, async_channel::Receiver<KernelMessage>) {
        let (shell_tx, shell_rx) = async_channel::unbounded();
        let (control_tx, _control_rx) = async_channel::unbounded();
        let (iopub_tx, _iopub_rx) = tokio::sync::broadcast::channel(16);
        let (process_stderr_tx, _stderr_rx) = tokio::sync::broadcast::channel(16);
        let signal = CancellationToken::new();
        let conn = Self {
            shell_tx,
            control_tx,
            iopub_tx,
            process_stderr_tx,
            reply_tx_map: Arc::new(DashMap::new()),
            signal: signal.clone(),
            _drop_guard: Arc::new(signal.drop_guard()),
        };
        (conn, shell_rx)
    }
}
```

> NOTE: match the EXACT field set and types of `KernelConnection` as they exist at HEAD (listed in Grounded facts). If `DropGuard` is not constructible via `CancellationToken::drop_guard()`, adapt to however the production code builds `_drop_guard` (read the real constructor in `wire_protocol.rs`). Keep all `_`-prefixed receivers bound so channels are not closed during the test.

The behavioral test lives in `commands.rs` `mod tests` (so it exercises the jute `send_comm_msg_on_conn`):

```rust
#[tokio::test]
async fn send_comm_msg_on_conn_emits_comm_msg_on_shell_channel() {
    use crate::backend::wire_protocol::{CommMessage, KernelConnection, KernelMessageType};
    let (conn, shell_rx) = KernelConnection::for_test();

    let data = serde_json::json!({ "method": "update", "state": { "value": 7 } });
    let buffers = vec![b"abc".to_vec(), vec![1, 2, 3]];

    send_comm_msg_on_conn(&conn, "comm-xyz", data.clone(), buffers.clone())
        .await
        .expect("send_comm_msg_on_conn");

    let sent = shell_rx.try_recv().expect("one shell message");
    assert!(shell_rx.try_recv().is_err(), "exactly one message");
    assert_eq!(sent.header.msg_type, KernelMessageType::CommMsg);

    let typed = sent
        .into_typed::<CommMessage>()
        .expect("comm_msg content deserializes");
    assert_eq!(typed.content.comm_id, "comm-xyz");
    assert_eq!(typed.content.data, data);
    let got: Vec<Vec<u8>> = typed.buffers.iter().map(|b| b.to_vec()).collect();
    assert_eq!(got, buffers);
}
```

- [ ] **Step 2: Run the test, verify it fails.** `scripts/spur-cargo test -p jute send_comm_msg_on_conn_emits_comm_msg_on_shell_channel`. Expected: FAIL (`send_comm_msg_on_conn` / `for_test` not defined).

- [ ] **Step 3: Split the helper** in `commands.rs`:

```rust
/// Send a Jupyter `comm_msg` to a live kernel slot over the shell channel.
pub async fn send_comm_msg(
    state: &State,
    slot_id: &str,
    comm_id: &str,
    data: serde_json::Value,
    buffers: Vec<Vec<u8>>,
) -> Result<(), Error> {
    let conn = kernel_connection_for_slot(state, slot_id)?;
    send_comm_msg_on_conn(&conn, comm_id, data, buffers).await
}

async fn send_comm_msg_on_conn(
    conn: &crate::backend::KernelConnection,
    comm_id: &str,
    data: serde_json::Value,
    buffers: Vec<Vec<u8>>,
) -> Result<(), Error> {
    let _pending = conn
        .call_shell(crate::backend::wire_protocol::build_comm_msg(comm_id, data, buffers))
        .await?;
    Ok(())
}
```

(Use the import paths already present in `commands.rs`; `build_comm_msg` is already imported there.)

- [ ] **Step 4: Run the test, verify it passes**, then the full crate suite: `scripts/spur-cargo test -p jute`.

- [ ] **Step 5: Commit** test-then-impl:

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs \
        crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs
git commit -m "test(jute): cover real comm_msg send over shell channel"
# (squash or order so the failing test commit precedes the helper-split commit)
git commit -m "refactor(jute): extract send_comm_msg_on_conn for testability"
```

---

### Task 2: real-kernel e2e through the real gateway

**Task ID:** `task-2-real-kernel-e2e`

**Files:**
- Create: `crates/spur-notebook/tests/afm_comm_send_e2e.rs`
- Modify: `crates/spur-notebook/src/commands.rs` (make `handle_anywidget_command_intent` `pub` — the minimal integration seam)

**Depends on:** none

**Suggested Worker:** claude-code-acp (integration test + Python comm mechanics + a public seam — benefits from broad context); codex acceptable if acp unavailable.

**Scope Boundary:**
- IN scope: the new integration test file; the one-word visibility change (`async fn` → `pub async fn`) on `handle_anywidget_command_intent`.
- OUT of scope: any behavioral change to the handler, gateway, jute send path, `state.rs`, or the data plane. Do NOT alter the response shape. Do NOT add ipywidgets as a dependency.
- If python3/ipykernel is unavailable in the runner, the test MUST skip gracefully (mirror `python_binary_for_test` in `rust_go_ports_e2e.rs`) — never hard-fail on a missing interpreter.
- If you discover you must touch OUT-OF-SCOPE files, emit `scope_drift`.

**Acceptance Criteria:**
- [ ] `handle_anywidget_command_intent` is `pub` (still defaults to the real `JuteModelStateCommGateway`); no behavior change.
- [ ] New integration test: starts a real python3 kernel via the existing harness, opens a kernel-side comm (so the real `run_cell` path records `comm_id → slot` in `comm_owner`), then calls the **real** `handle_anywidget_command_intent` with a `model-state.update` intent carrying that `comm_id`, and asserts the Python comm's `on_msg` actually received `{method:"update", state:{...}}` — proving delivery through the real gateway → real `jute::commands::send_comm_msg` → live kernel.
- [ ] Response `kernelDelivery.status == "sent"` for the known comm; the `{method,state}` echo is still returned.
- [ ] Test skips cleanly (does not fail) when python3 is unavailable.
- [ ] `scripts/spur-cargo test -p spur-notebook` is green where python3 is present (paste output, including the test running — not just skipped).
- [ ] No NEW clippy warnings on the touched files (pre-existing spur-notebook backlog out of scope).

**Implementation:**

- [ ] **Step 1: Make the seam public.** In `crates/spur-notebook/src/commands.rs`, change `async fn handle_anywidget_command_intent(` to `pub async fn handle_anywidget_command_intent(`. (Leave `_with_gateway` private; the public default-gateway entry is all the test needs.)

- [ ] **Step 2: Write the failing integration test** `crates/spur-notebook/tests/afm_comm_send_e2e.rs`. Model the kernel-startup/run-cell harness on `crates/spur-notebook/tests/rust_go_ports_e2e.rs` (copy its `python_binary_for_test`, `ServerDeps` construction, `start_kernel`, and `run_cell` usage; reuse `jute::state::notebook_slot_id`). Structure:

```text
1. If python_binary_for_test() is Err → eprintln!("skipping: {e}") and return.   // graceful skip
2. Build ServerDeps with a fresh Arc<jute::state::State> (as rust_go_ports_e2e does).
3. start_kernel for a python3 slot on a temp notebook path.
4. run_cell a Python cell that opens a comm and captures inbound messages:

       from ipykernel.comm import Comm
       _spur_rx = []
       _c = Comm(target_name="spur.afm.test", data={})
       @_c.on_msg
       def _on(msg):
           _spur_rx.append(msg["content"]["data"])
       print("COMM_ID=" + _c.comm_id)

   Creating the Comm sends a comm_open kernel→frontend, so the real run_cell
   path emits RunCellEvent::CommOpen and records comm_id → slot in comm_owner.
   Capture the printed COMM_ID from the cell stdout outputs.
5. Resolve the State + slot the test started (the same Arc<State> in ServerDeps;
   slot id via notebook_slot_id / the id returned by start_kernel).
6. Call the REAL handler:

       let intent = AnyWidgetCommandIntent {
           id: "e2e-1".into(),
           kind: "anywidget-command".into(),
           name: "model-state.update".into(),
           comm_id: Some(comm_id.clone()),
           msg: json!({ "state": { "value": 99 } }),
           buffers: vec![],
       };
       let resp = spur_notebook::commands::handle_anywidget_command_intent(
           &state, None, intent).await;
       assert_eq!(resp.response["kernelDelivery"]["status"], "sent");

   (AnyWidgetCommandIntent / AnyWidgetCommandResponse may need to be exported
   from spur_notebook::commands; if they are not already `pub`, prefer building
   the intent via serde_json::from_value(json!({...})) against a `pub` type, or
   add the minimal `pub` re-export. Keep additions to visibility-only.)
7. run_cell a Python readback cell and assert the message arrived:

       import json, time
       for _ in range(50):
           if _spur_rx: break
           time.sleep(0.05)
       print("RX=" + json.dumps(_spur_rx))

   Assert the captured stdout contains an entry equal to
   {"method":"update","state":{"value":99}}.
8. stop_kernel; drop guards.
```

> The shell channel is serial: the comm_msg is processed after the open cell completes and before/around the readback cell. The poll loop in the readback cell absorbs iopub/shell scheduling latency. If `kernelDelivery` is `skipped:comm_not_open`, the comm_open event was not drained before the send — ensure step 4's `run_cell` fully completes (the MCP `run_cell` tool drains to terminal) before step 6.

- [ ] **Step 3: Run, verify the test actually executes (not skipped) locally with python3**, and fails before the seam/visibility wiring is correct:
  `scripts/spur-cargo test -p spur-notebook --test afm_comm_send_e2e -- --nocapture`.

- [ ] **Step 4: Make it pass.** Adjust visibility/exports as needed (visibility-only). Re-run until green; then `scripts/spur-cargo test -p spur-notebook`.

- [ ] **Step 5: Commit** test-then-seam:

```bash
git add crates/spur-notebook/tests/afm_comm_send_e2e.rs crates/spur-notebook/src/commands.rs
git commit -m "test(spur-notebook): e2e AFM model-state.update reaches live kernel comm"
```

**Scope Drift Checkpoint:**
- If wiring the test requires changing handler/gateway/jute *behavior* (not just visibility) → emit `scope_drift` before proceeding.
- If the real kernel comm round-trip proves infeasible in-harness (e.g. comm_open events are not surfaced by the MCP `run_cell` path) → emit `risk` with the specific blocker rather than weakening the assertion to a no-op.

---

## Constraints (all tasks)

- TDD: failing test first, then the minimal change. Conventional commits (`test`/`refactor`/`fix`).
- Build/test through `scripts/spur-cargo` only (Rust, remote default). Never bare `cargo`.
- Clippy: no NEW warnings; the pre-existing cross-crate backlog (spur-license/jute/spur-notebook) is out of scope — note it, don't fix it.
- Tests only: do not change comm-send *behavior*, the response shape, the reactive engine, or the data plane (`PortStore`/Arrow). The only non-test edits allowed are the `send_comm_msg_on_conn` extraction (Task 1) and the `pub` visibility change (Task 2).
- Reuse existing types: `KernelMessage`, `CommMessage`, `KernelMessageType`, `build_comm_msg`, the `number[][]`/`Vec<Vec<u8>>` buffer representation.

## Self-review

- **Coverage:** Task 1 locks the real jute wire path (`send_comm_msg` body → `build_comm_msg` + `call_shell` → CommMsg bytes) deterministically with no process. Task 2 locks the real cross-crate gateway path to a live kernel. Together they close residual gap #1 from both ends. ✔
- **Feasibility grounded:** conn fields are module-private (test constructor must live in `wire_protocol.rs`); `LocalKernel.child` can't be faked (so the state hop is covered by a real kernel, not a fake slot); the real-kernel harness + graceful python3 skip already exist in `rust_go_ports_e2e.rs`; `ipykernel.comm.Comm` needs no extra deps. ✔
- **DAG:** two independent tasks, no deps, parallel. ✔
- **No placeholders:** concrete test bodies, the exact `KernelConnection` field set, and the Python snippets are inline. The one explicitly-flagged unknown (exact `_drop_guard` construction) is bounded with a read-the-real-constructor instruction. ✔
- **Risks:** (a) `KernelConnection::for_test` must mirror the real field set exactly — flagged. (b) Task 2 timing/visibility — flagged with poll loop + scope-drift/risk checkpoints. (c) `AnyWidgetCommandIntent`/`Response` export visibility — handled as visibility-only additions.
