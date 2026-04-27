# ACP 0.11 Blocker Scope Investigation

Date: 2026-04-27
Branch investigated: `spur/worker-codex-1b3c120e-bbb0-4561-85af-a4e2934f8010`
HEAD investigated: `8c60e756999c`
Dependency bump: `14189704 chore(deps): bump agent-client-protocol 0.10.4 -> 0.11.1`

## Executive Summary

`cargo check -p spur-acp` with `RUSTC_WRAPPER=` cleared emits 4 compiler-visible blockers in `crates/spur-acp/src`: two root-path schema imports, one removed connection type, and one `Client` trait-to-struct break. Static follow-up found additional root-path schema imports in `crates/spur-acp/examples` and `crates/spur-acp/tests` that will fail after the library blockers are removed. The real scope is moderate because `connection/native.rs` must move from the old `ClientSideConnection` plus `impl Client` model to ACP 0.11's `Client.builder()` / `ByteStreams` / `ConnectionTo<Agent>` model.

A sibling branch already contains this migration: `spur/worker-claude-code-fb5f7b67-0821-456e-a20e-e20b0a221929`, commit `035426b5`. I verified that sibling worktree with `env RUSTC_WRAPPER= CARGO_TARGET_DIR=/tmp/spur-acp-candidate-fb5f-target cargo check -p spur-acp`, which finished successfully. Recommendation: R4, cherry-pick the sibling migration commit rather than reimplementing it from scratch.

## Evidence Collected

Commands run:

```text
env RUSTC_WRAPPER= cargo check -p spur-acp
env RUSTC_WRAPPER= cargo check -p spur-acp 2>&1 | rg "error\[E"
git show --stat --oneline --decorate 14189704
git branch -a | rg -i acp
git log --all --oneline --grep="acp" --grep="0.11" -i
git log --all --oneline -- crates/spur-acp/
git branch -a --contains 035426b5
env RUSTC_WRAPPER= CARGO_TARGET_DIR=/tmp/spur-acp-candidate-fb5f-target cargo check -p spur-acp
```

Published ACP 0.11 references:

- `agent-client-protocol` 0.11.1 docs: <https://docs.rs/agent-client-protocol/latest/agent_client_protocol/>
- 0.11 quick start uses `Client.builder()` and `agent_client_protocol::schema::{InitializeRequest, ProtocolVersion}`.
- 0.11 root re-exports include `role::acp::{Agent, Client, Conductor, Proxy}` and the `schema` module, but not all schema structs at the crate root.
- 0.11 source confirms `schema/mod.rs` re-exports `agent_client_protocol_schema::*`, so schema structs should be imported through `agent_client_protocol::schema::*`.

## Compiler-Visible Broken Sites

The requested command reports:

```text
error[E0432]: unresolved import `agent_client_protocol::ToolCall`
error[E0432]: unresolved import `agent_client_protocol::ClientSideConnection`
error[E0404]: expected trait, found struct `Client`
error[E0425]: cannot find type `RequestPermissionRequest` in crate `agent_client_protocol`
```

| Site | Error | Classification | Proposed fix |
|---|---:|---|---|
| `crates/spur-acp/src/adapter/claude.rs:1` | E0432 | Renamed import | Change `agent_client_protocol::ToolCall` to `agent_client_protocol::schema::ToolCall`. This is derivable from ACP 0.11's `schema` module. |
| `crates/spur-acp/src/connection/native.rs:61` | E0432 | Removed type | `ClientSideConnection` is not in the 0.11 root API. Replace the old connection construction with `agent_client_protocol::ByteStreams::new(stdin_compat, stdout_compat)` plus `Client.builder().connect_with(transport, async move |cx: ConnectionTo<Agent>| { ... })`. |
| `crates/spur-acp/src/connection/native.rs:1060` | E0404 | Trait-to-struct refactor | `agent_client_protocol::Client` is now a role struct, not the callback trait. Remove `impl Client for SpurAcpClientDynamic`; register equivalent callbacks with `Client.builder().on_receive_request(...)` and `.on_receive_notification(...)`. |
| `crates/spur-acp/src/types.rs:227` | E0425 | Renamed import | Change `agent_client_protocol::RequestPermissionRequest` to `agent_client_protocol::schema::RequestPermissionRequest`. |

## Follow-On Sites

These do not appear in the first `cargo check -p spur-acp` output because the library fails before tests/examples are checked, but they are still under `crates/spur-acp/` and use schema types through the old root path:

| Site | Pattern | Classification | Proposed fix |
|---|---|---|---|
| `crates/spur-acp/examples/compat_spike.rs:182` | `agent_client_protocol::SessionUpdate::UserMessageChunk` | Renamed import | Prefix with `agent_client_protocol::schema::SessionUpdate` or import `SessionUpdate` from `schema`. |
| `crates/spur-acp/examples/compat_spike.rs:185` | `SessionUpdate::AgentMessageChunk` root path | Renamed import | Same as above. |
| `crates/spur-acp/examples/compat_spike.rs:188` | `SessionUpdate::AgentThoughtChunk` root path | Renamed import | Same as above. |
| `crates/spur-acp/examples/compat_spike.rs:191` | `SessionUpdate::ToolCall` root path | Renamed import | Same as above. |
| `crates/spur-acp/examples/compat_spike.rs:192` | `SessionUpdate::ToolCallUpdate` root path | Renamed import | Same as above. |
| `crates/spur-acp/examples/compat_spike.rs:195` | `SessionUpdate::Plan` root path | Renamed import | Same as above. |
| `crates/spur-acp/examples/compat_spike.rs:196` | `SessionUpdate::AvailableCommandsUpdate` root path | Renamed import | Same as above. |
| `crates/spur-acp/examples/compat_spike.rs:199` | `SessionUpdate::CurrentModeUpdate` root path | Renamed import | Same as above. |
| `crates/spur-acp/tests/adapter_fixtures.rs:1` | `use agent_client_protocol::{SessionNotification, SessionUpdate}` | Renamed import | Change to `agent_client_protocol::schema::{SessionNotification, SessionUpdate}`. |
| `crates/spur-acp/tests/adapter_smoke.rs:1` | `use agent_client_protocol::{ToolCall, ToolKind}` | Renamed import | Change to `agent_client_protocol::schema::{ToolCall, ToolKind}`. |
| `crates/spur-acp/tests/auto_approve_defensive.rs:5` | schema structs from root | Renamed import | Change grouped import to `agent_client_protocol::schema::{...}`. |
| `crates/spur-acp/tests/executor_events_roundtrip.rs:131` | schema structs from root | Renamed import | Change grouped import to `agent_client_protocol::schema::{...}`. |
| `crates/spur-acp/tests/load_session_error_propagation.rs:6` | `InitializeRequest`, `ProtocolVersion` from root | Renamed import | Change to `agent_client_protocol::schema::{InitializeRequest, ProtocolVersion}`. |
| `crates/spur-acp/tests/native_trailing_notification.rs:31` | schema structs from root | Renamed import | Change grouped import to `agent_client_protocol::schema::{...}`. |
| `crates/spur-acp/tests/session_notification_bus.rs:39` | `InitializeRequest`, `ProtocolVersion`, `SessionUpdate` from root | Renamed import | Change to `agent_client_protocol::schema::{InitializeRequest, ProtocolVersion, SessionUpdate}`. |
| `crates/spur-acp/tests/tool_meta_extraction.rs:1` | `SessionNotification`, `SessionUpdate` from root | Renamed import | Change to `agent_client_protocol::schema::{SessionNotification, SessionUpdate}`. |

The sibling migration also updates matching root-schema imports in `spur-core`, `spur-tui`, and `spur-bot`, which will matter for `cargo check --workspace` after `spur-acp` is unblocked.

## Native Connection Migration Shape

Old 0.10 shape:

- Construct `ClientSideConnection::new(client_impl, outgoing, incoming, spawn)`.
- Implement the `Client` trait on `SpurAcpClientDynamic`.
- Call typed methods like `connection.initialize(request).await`, `connection.prompt(request).await`, and `connection.cancel(notification).await`.

ACP 0.11 target shape:

- Construct `ByteStreams::new(stdin_compat, stdout_compat)`.
- Use `Client.builder().name(...).on_receive_request(...).on_receive_notification(...).connect_with(transport, async move |cx: ConnectionTo<Agent>| { ... })`.
- Replace outgoing typed method calls with `cx.send_request(request).block_task().await` for request/response methods.
- Replace cancel with `cx.send_notification(CancelNotification::new(session_id))`.
- Handle session/ext notifications through `on_receive_notification` over `AgentNotification`, preserving the existing broadcast and ext-notification channels.
- For `ExtMethod`, use the 0.11 `ClientRequest::ExtMethodRequest(request)` path and wrap the JSON response back into `ExtResponse` if the current `AgentConnection::call_ext` surface is kept.

The migration target for the native connection is therefore known, not a blind design problem. The remaining design decision is whether to accept the sibling branch's exact implementation, especially its conversion of some native-thread state from `Rc<RefCell<_>>` to `Arc<Mutex<_>>` so 0.11 handler closures satisfy `Send` bounds.

## Scope Estimate

If implemented from scratch in the current branch:

- Files touched: likely 3 `spur-acp/src` files for the visible blockers, plus `spur-acp` tests/examples and downstream root-schema consumers in `spur-core`, `spur-tui`, and `spur-bot` before workspace checks are green.
- SLOC delta: rough net +100 to +200, with 1,300 to 1,500 lines of churn due to reshaping `connection/native.rs`.
- Complexity tier: moderate. Schema import renames are trivial, but the native connection migration is semantic and touches lifecycle, callback ordering, cancellation, terminal state, permission requests, and extension notifications.
- Estimated wall-clock if dispatched as a fresh fix delegation: 3 to 5 hours including `cargo check -p spur-acp`, focused tests, and workspace follow-up compile.

If using the sibling migration commit:

- Patch size of `035426b5`: 26 files, 783 insertions, 638 deletions.
- `crates/spur-acp` portion: 12 files plus `native.rs`, with the main churn in `connection/native.rs`.
- Estimated wall-clock: 30 to 60 minutes to cherry-pick, resolve any current-branch drift, run `cargo check -p spur-acp`, then run a workspace compile gate.

## Sibling-Branch Analysis

Search results:

- `git log --all --oneline --grep="acp" --grep="0.11" -i` found `14189704` and `035426b5` as the relevant ACP 0.11 migration commits.
- `git log --all --oneline --grep="SDK 0.10" -i` found only `035426b5`.
- `git branch -a --contains 035426b5` found only `spur/worker-claude-code-fb5f7b67-0821-456e-a20e-e20b0a221929`.
- `git branch -r | rg -i acp` found no remote ACP branch names in this clone.

Candidate:

| Branch | Commit | Summary | Verification | Salvageable |
|---|---|---|---|---|
| `spur/worker-claude-code-fb5f7b67-0821-456e-a20e-e20b0a221929` | `035426b5` | Migrates from the 0.10 `ClientSideConnection`/`Client` trait model to the 0.11 `Client.builder()`/`ByteStreams`/`ConnectionTo<Agent>` model; fixes root-schema imports in spur-acp tests/examples and downstream consumers. | `cargo check -p spur-acp` passed in the sibling worktree with `CARGO_TARGET_DIR=/tmp/spur-acp-candidate-fb5f-target`. | Yes. Best candidate for cherry-pick, but use the commit patch rather than merging the branch head because the branch predates Plan B license changes. |
| `feat/acp-upgrade-and-codex-pickers` and descendants containing `14189704` | `14189704` | Dependency bump only: `Cargo.toml` and `Cargo.lock`. | Current branch fails `cargo check -p spur-acp` with 4 library errors. | No, this is the incomplete baseline, not a completed migration. |
| Other local `*-acp-*` worker branches | many | Mostly older ACP feature branches or worker outputs unrelated to the 0.10 -> 0.11 SDK API break. | No matching 0.11/builder-migration commit found by grep. | Not a migration source. |

## Recommendation

R4: Cherry-pick the ACP 0.11 migration from the sibling branch.

The sibling commit `035426b5` is the fastest credible path because it already resolves the non-mechanical native connection refactor and compiles for `spur-acp` against `agent-client-protocol = 0.11.1`. R1 would duplicate a moderate, error-prone lifecycle migration that is already present locally. R2 would make Plan B tests green sooner but would abandon the branch's explicit ACP-upgrade direction and leave the same migration to repeat later. R3 leaves workspace gates dark for Waves 4-8, which is too much blind spot now that a compile-clean candidate exists.

## Risk Register

| Risk | Impact | Mitigation |
|---|---|---|
| The sibling branch predates Plan B Waves 1-3 and full-branch merge would drop license/admin work. | High | Cherry-pick only `035426b5`; do not merge or reset to the sibling branch. Verify `git diff --stat` before commit and confirm Plan B files are preserved. |
| `035426b5` passes `cargo check -p spur-acp` but may expose downstream workspace errors after current Plan B changes. | Medium | After cherry-pick, run `cargo check -p spur-acp`, then `cargo check --workspace` or at minimum `cargo check -p spur-core -p spur-tui -p spur-cli` before Wave 4. |
| The `Arc<Mutex<_>>` conversion in native handlers changes concurrency characteristics around terminal/cwd state. | Medium | Run the existing native notification, permission, terminal, and session bus tests; review for lock-held-across-await patterns before accepting the patch. |

