# Jute-App AFM C6: wire `save_changes` → kernel comm — Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan`. Each task becomes a beads issue with `spur:plan-task-id` / `spur:plan-id` labels. Base pinned to `f664eeeb` (repo has an active concurrent writer — re-pin if stale).

**Source:** AFM audit follow-up item C6 / limitation #1 (documented in `2026-06-05-jute-app-afm-reactive-control-plane.md` §Known Limitations). Resolves the gap where `model.save_changes()` is a frontend-only echo that never reaches the Python kernel.

## Goal

Make the AFM `model-state.update` intent (triggered by an in-iframe `model.save_changes()`) actually deliver a Jupyter `comm_msg` `{method:"update", state}` to the kernel's open comm for that widget, so Python-side traitlet `@observe` handlers fire. Today `handle_model_state_update_intent` only echoes the state back to the host-side registry mirror. As a free follow-on, route generic `model.send(...)` (gap #3, currently dropped) through the same comm channel as a `custom` comm_msg.

## Grounded architecture (HEAD f664eeeb)

The kernel comm-send transport **already exists** — this is plumbing, not new transport:

- **`KernelConnection::call_shell<T: Serialize>`** (`crates/spur-notebook/jute-notebook/src-tauri/src/backend/wire_protocol.rs:592`) — generic "send a message to the kernel over the shell channel". A `comm_msg` is just a shell message.
- **`kernel_connection_for_slot(state, slot_id)`** (jute backend `src-tauri/src/commands.rs`) — resolves the live `KernelConnection` for a slot. Used by `run_cell_events` → `resolve_run_cell_dispatch` (slot routing: python vs `#deno`) → `run_cell_with_mode(&conn, …)`. This is the exact template the comm-send follows.
- **`comm_run_cell_event_from_message`** (jute `backend/commands.rs`) already maps kernel→frontend `CommOpen`/`CommMsg`/`CommClose`. The reverse (frontend→kernel) has no path today.
- **`handle_model_state_update_intent`** / **`AnyWidgetCommandIntent`** (`crates/spur-notebook/src/commands.rs`) — the AFM intent handler + struct; the handler echoes `{method:"update", state}` and does not touch the kernel. `handle_anywidget_command_intent` already has `state: &jute::state::State` in scope.
- **`afmHost.ts`** (`crates/spur-notebook/jute-notebook/src/ui/notebook/`) — `intentFromMessage` builds the intent; for `save_changes` the `modelId` is currently buried in a synthetic id and not a clean field. Generic `model.send` (type `"send"`, non-command) is dropped with a warn.

**The one missing piece of state:** which slot owns a given `comm_id` (== widget `model_id`). The comm was opened by whichever kernel ran the cell that displayed the widget (base vs `#deno`). Record `comm_id → slot_id` when a `CommOpen` flows from the kernel.

**Out of scope:** the data plane (file-based `PortStore`/Arrow ports) is untouched. This is control-plane only. C5 (`ipc://` streaming bus) is a separate epic.

## Task DAG

Linear chain (t1→t2→t3→t4). Rationale: t1+t2 both edit the large jute `backend/commands.rs`; t3 edits spur-notebook `commands.rs` + `afmHost.ts`; sequencing avoids same-file merge friction under the active concurrent writer.

### Task 1 — jute: kernel comm-send primitive
Add a backend function `send_comm_msg(state, slot_id, comm_id, data: Value, buffers: Vec<Vec<u8>>) -> Result<(), Error>` that resolves the connection via `kernel_connection_for_slot` and sends a `comm_msg` `KernelMessage` through `KernelConnection::call_shell`. Add a `comm_msg` builder in `wire_protocol.rs` (comm_id + data + buffers → `KernelMessage` with `msg_type = CommMsg`). TDD: a wire-shape/roundtrip test (model the existing `run_cell_event_roundtrip.rs`/`call_shell` tests). Build/test `-p jute`; remote clippy.
- Files: `…/src-tauri/src/backend/commands.rs`, `…/src-tauri/src/backend/wire_protocol.rs` (+ test).

### Task 2 — jute: record `comm_id → slot` at comm_open  *(depends: t1)*
When a `CommOpen` is observed from the kernel (in/around `comm_run_cell_event_from_message` / `run_cell_with_mode`), record `comm_id → slot_id` in `State` (a small concurrent map). Expose a resolver `slot_for_comm(state, comm_id) -> Option<slot_id>`. Clear/overwrite on `CommClose` and kernel restart. TDD a unit test for record + resolve + close. Build/test `-p jute`.
- Files: `…/src-tauri/src/backend/commands.rs`, `…/src-tauri/src/state.rs` (+ test).

### Task 3 — spur-notebook: wire the handler + thread `model_id`  *(depends: t2)*
- Add `comm_id` (a.k.a. `model_id`) to `AnyWidgetCommandIntent` (camelCase serde).
- `afmHost.ts` `intentFromMessage`: populate `comm_id`/`model_id` as a real field for both the command and `save_changes` branches.
- Pass `state` into `handle_model_state_update_intent`; resolve the slot via `slot_for_comm`; call jute `send_comm_msg(state, slot, comm_id, {method:"update", state}, buffers)`. Keep returning the `{method:"update", state}` echo so the host-registry mirror stays consistent (belt-and-suspenders). If no slot is found (comm not open / kernel dead), return a structured non-fatal error and still echo — do not panic.
- Build/test `-p spur-notebook` and the notebook frontend (`spur-pnpm test` for afmHost) ; remote clippy.
- Files: `crates/spur-notebook/src/commands.rs`, `crates/spur-notebook/jute-notebook/src/ui/notebook/afmHost.ts` (+ tests).

### Task 4 — e2e + generic `model.send` (#3)  *(depends: t3)*
- e2e/integration test: a `save_changes` on a widget delivers a `comm_msg` update to the kernel and (where feasible in-test) mutates a Python traitlet so an `@observe` callback fires; at minimum assert the comm_msg reaches the kernel connection for the right slot.
- Close gap #3: route generic `model.send(content)` (currently dropped in `afmHost.ts`) through `send_comm_msg` as a `custom` comm_msg (`{method:"custom", content}`), reusing the t1 primitive. Stop dropping; keep a warn only for truly malformed messages.
- Files: integration test(s), `afmHost.ts`, handler glue.

## Constraints (all tasks)

- TDD: failing test first, then implementation. Conventional commits (`test`/`feat`/`fix`/`refactor`).
- Build/test through `scripts/spur-cargo` (Rust, remote default) and `scripts/spur-pnpm` (frontend). Do **not** use bare `cargo`/`pnpm`.
- Clippy: no **new** warnings; the workspace has a pre-existing cross-crate lint backlog (spur-license/jute) that is out of scope — note it, don't try to fix it.
- Control-plane only: do **not** touch `PortStore`/Arrow ports or the reactive engine cascade.
- Reuse the existing `number[][]` buffer encoding (landed in the AFM buffer fix) for any comm buffers.
- Scope each task to its listed files + tests.

## Self-review

- **Transport exists** (`call_shell`) — no new socket/zmq work. ✔
- **Slot resolution** is the only genuinely new state (comm_id→slot); recorded at the existing comm_open chokepoint. ✔
- **Independent of C5**; data plane untouched. ✔
- **Linear DAG** avoids same-file churn under the active concurrent writer. ✔
- **Risks:** ordering (shell is serial — comm_msg queues behind a running execute, acceptable); lifecycle (kernel restart/closed comm → structured non-fatal + echo); cross-slot (base vs `#deno`) handled by the comm_id→slot map.
