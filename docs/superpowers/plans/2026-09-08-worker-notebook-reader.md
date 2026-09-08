# Worker notebook reader implementation plan

User approved the proposal in this conversation and explicitly selected inline
implementation with TDD and pre/post solve per task. Execute locally in the two
shared worktrees; do not submit to the automatic worker dispatcher.

Epic: `bd-162pw`. Architecture receipt: `sol_2caa59e472244d91` (Z3 Optimize,
lexicographic current-state access then additional worker credentials; complete).

## Contract

Authenticated workers use the existing worker MCP endpoint. The server grants
only notebooks explicitly supplied as `.ipynb` task context files. Canonical
brain-side paths bind the grant; a worker worktree or active notebook tab cannot
change it. `notebook_list_cells` returns targeted cell identifiers and notebook
identity/revision; `notebook_read_cell` returns effective source, outputs and etag.
The server forwards only these two names and validates arguments and returned
identity. No edit, execute, open, save, kernel or unknown operation is forwarded.
Completion revokes grants. Unavailable/incorrect upstream state produces an error.

## T1 — Target-scoped discovery (`bd-3ikk1`)

Repository: `spur-notebook`; no dependencies.
Files: `src/mcp/tools/list_cells.rs`, `src/mcp/tools/{mod,annotations}.rs`,
`src/mcp/server.rs`, `tests/worker_notebook_reads.rs`.

Add `call(deps, {notebook_path|notebook_id})` through the existing explicit
target resolver. Return `{notebook_id,path,revision,cells:[{id,kind}]}` from the
target store. Reject absent, mismatched, unopened and unexpected arguments.
Preserve daemon focus and durable notebook state.

Pre-solve compares active-focus selection to explicit-target selection. RED uses
real MCP `tools/call` for the missing discovery tool with two open notebooks.
GREEN registers and implements discovery; POST checks returned target equals the
requested target and focus remains unchanged.

Run `scripts/spur-cargo test -p spur-notebook --no-default-features --test worker_notebook_reads`.

## T2 — Worker reader and transport (`bd-2jbnj`)

Repository: `spur`; no dependency on T1 for isolated policy tests.
Files: new `crates/spur-core/src/worker_notebook.rs` and its tests;
`src/lib.rs`, `src/mcp/{mod,catalog}.rs`, `src/worker_server.rs`, scoped access
to existing notebook transport helpers in `src/orchestrator.rs`.

Implement server-owned per-delegation grants and a narrow reader. Authorization
must precede any upstream request. Reuse the internal length-prefixed JSON-RPC
transport with existing frame bounds; initialize MCP and preserve tool errors.
Scope source reads by canonical notebook path and optional cell IDs. Check
returned path, requested cell identity, and required provenance. Filter cell
discovery when a grant includes a cell subset. Worker catalog contains only the
two notebook read operations, with no aliases or generic passthrough.

Pre-solve uses catalog permission reachability plus an uncatalogued guard/dataflow
model. RED covers missing catalog and denied unauthorized requests using the real
worker server and a local daemon transport fixture. GREEN supplies forwarding and
auditing. POST evaluates permissions and the implemented guard predicates.

Run `scripts/spur-cargo test -p spur-core --lib worker_notebook` and targeted
worker-server integration tests.

## T3 — Dispatch binding and lifecycle (`bd-1hmnc`)

Depends on T1 and T2. Repository: `spur`.
Files: `src/orchestrator/delegation/execute.rs`, `src/orchestrator/worker_mcp.rs`,
worker notebook binding helpers/tests, `src/worker_server.rs` integration tests.

Resolve explicit notebook context paths against the brain repository before
starting the worker. Register the grant with the existing per-brain server and
the stable notebook socket for that repository. Supply only the worker endpoint
in agent MCP configuration. Remove notebook grants during delegation flush.

Pre-solve models binding by delegation and revocation. RED covers two isolated
delegations, missing grants and post-completion reads; GREEN wires dispatch and
cleanup. POST checks the same guards from the final code and reruns the approved
architecture model with the implemented assignment fixed.

Run targeted remote tests, related worker catalog/HTTP suites, and local fmt.
Review only this task's diff; preserve pre-existing edits and record test/solve
receipts in the respective beads issue before closing it.

## Usage and operational boundary

Enable worker MCP and include the intended saved `.ipynb` paths in the task's
`context_files`. Relative paths resolve against the brain repository. Workers
receive exact canonical paths in a read-only context section, then call
`notebook_list_cells({notebook_path})` and
`notebook_read_cell({notebook_path,id})`. No notebook socket is added to their
agent MCP configuration. Whole-notebook context grants are the initial public
workflow; the internal reader also supports cell-subset grants.

Deploy both repository changes: an older notebook daemon lacks list_cells and
returns an explicit tool error. The daemon must already be running and own the
open notebook's writer store; worker access itself remains read-only. The reader
never launches the daemon, opens a notebook, changes focus, or reads stale disk
content as a fallback. Unix sockets are required; other platforms fail explicitly.
Missing explicit notebook files abort dispatch. Empty context creates no grant.

This restricts the worker MCP notebook surface, not every capability an agent may
independently possess (for example filesystem tools or user-configured MCPs).
Revocation cancels pending reads and checks again before releasing a response;
it cannot retract bytes already returned to the client. No notebook is edited or
executed by either of the two exposed tools.

## Verification receipts

T1 PRE: `sol_a7e98e40ef914176` feasible; `sol_99ee70a2f56e42e0` rejects wrong
target/focus change; legacy focus counterexample `sol_f8eb5f9f324448c5`.
POST: `sol_88561c847b7a4729` feasible and `sol_e25adb9b0aac4ef7` rejects the
same violation. Remote integration test passed. Notebook commit: `29cf21c2`.

T2 PRE policy: `sol_419d01c33d444729` allows the two reads;
`sol_fb30fda2f09d444c`, `sol_6a75609bbd6442b5`, `sol_d058e9593ad146c1`, and
`sol_d8362c13296041b6` deny write/run/open/unknown. PRE guard violation:
`sol_c8623094485e44ba` UNSAT. POST guard violation:
`sol_fcb64e57ba4a4100` UNSAT, with positive read witness
`sol_67e14e47046f47d2` SAT. Four remote reader/HTTP tests passed before T3.

T3 PRE: bounded four-event workflow `sol_c9599a0230524798` passes;
`sol_9b594925ed9e469f` rejects post-revocation leakage. Arbitrary delegation-ID
guard `sol_32589df5fcc44fdd` rejects cross-delegation/revoked release. Final
guard refinement includes strict arguments and the exact method allowlist:
`sol_0dbff46a22404f81` feasible; `sol_cf2b1bfdce8d4c87` rejects forbidden release.

T3 POST: `sol_4673c2b382254909` SAT positive-read witness and
`sol_4721b979072d44a7` UNSAT forbidden-release query. Four-event workflow
`sol_1bd3f3e1bb7c43aa` passes (catalog cache reused). Implemented architecture
assignment is feasible in `sol_cc1484e437a145a3`. Final Z3 Optimize
`sol_c6a2352d6b8a4be6` is SAT with complete lexicographic optimization:
live_reads=1, new_worker_credentials=0, architecture=1 (integrated reader),
matching the approved choice among the four encoded candidates. The final T1,
T2, T3 guard proofs and architecture optimization receipt are pinned.

Final remote worker regression command:
`SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-core --lib worker`
exited 0: **156 passed, 0 failed**, including all nine new notebook tests,
worker HTTP/auth/catalog/schema tests, dispatch configuration, and shutdown.
T1's remote integration command exited 0: **1 passed, 0 failed**. Scoped
rustfmt checks and `git diff --check` passed. Review was performed inline per
the user's request; no implementation or review agents were dispatched.

These receipts evaluate explicitly encoded models, not Rust source. Runtime tests
connect those models to implementation behavior. Workflow claims are limited to
the declared four-event trace; no unbounded liveness claim is made.

Broader notebook verification limitations: the no-default-features lib-test
target has existing datasource feature-gating compile errors. With
datasource-introspect enabled, five annotation tests passed; the existing
`tools_dot_tools_stamps_every_registered_tool` test failed because
`notebook_attach_datasource` has no explicit annotation (also absent from HEAD's
mapping). The new discovery annotation is verified by the passing integration
test. Unrelated datasource/annotation behavior remains untouched.
