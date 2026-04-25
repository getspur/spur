# Quad-review synthesis — merge commit 7df4aea (Stage-1 peer mailbox hardening)

Four parallel reviewers, four different angles. This document consolidates the findings.

| Reviewer | Angle | Verdict | File |
|----------|-------|---------|------|
| claude-code | Architecture / forward-compat | APPROVE-WITH-FIXES | [7df4aea-architecture-claude-code.md](7df4aea-architecture-claude-code.md) |
| kimi | Operational / on-call | Structurally sound, 2 pager risks flagged | [7df4aea-operational-kimi.md](7df4aea-operational-kimi.md) |
| codex | Rust correctness | Rust-healthy, one main hardening item | [7df4aea-rust-correctness-codex.md](7df4aea-rust-correctness-codex.md) |
| gemini | Contract adherence | All 7 contracts verified; ready for Stage-2 | [7df4aea-contract-gemini.md](7df4aea-contract-gemini.md) |

## Convergent findings (≥2 reviewers)

1. **`RouterError::Ledger(String)` flattens typed `LedgerError`** — switch to `Ledger(#[from] LedgerError)` while <10 call sites — *claude-code + codex SHOULD-FIX*
2. **Reconciler `drain_quiet_window` ignored + drain/reconciler convergent design** — unify pre-Stage-2 — *claude-code + kimi + gemini SHOULD-FIX (3-way)*
3. **Post-prompt symmetric error-arm blocks 90% duplicated** — extract helper — *claude-code SHOULD-FIX, codex NIT*
4. **`let _ = ack_tx.send(())` swallow is load-bearing-but-undocumented** — *claude-code + codex*

## Reviewer-unique findings

**Kimi (operational):**
- **BLOCKER**: malformed-JSON in `_spur/peer_message_*` silently swallowed at `spur_ext_interp.rs:115` — no funnel signal to page on
- **SHOULD-FIX**: drain reset-timer has no absolute cap — chatty worker keeps drain alive forever
- **SHOULD-FIX**: `WorkerPeerMessageIgnored.reason` cardinality explosion (worker-supplied String)

**Claude-code (architecture):**
- `PeerMailboxLedger::non_terminal_entries()` returns whole table — won't scale to SQLite at Stage-2
- `Acceptance` enum missing `Rejected` variant (name mismatch with `accept_or_reject`)
- `#[doc(hidden)] pub` matrix predicates now load-bearing (proptest dep)
- Add `WorkerPeerMessageDrainTimedOut` + `WorkerPeerMessageAckReceived` events NOW (cheap, additive)

**Codex (Rust):**
- ACP `LedgerState` is `#[non_exhaustive]` but proptest strategies use wildcards — won't fail-fast on new variants. Suggest ACP-owned `KNOWN_LEDGER_STATES` constant.
- Mutex discipline clean; no `await`-while-locked.
- `Acceptance` consider `#[non_exhaustive]`.

**Gemini (contracts):**
- NIT: `WorkerPeerMessageDelivered` doesn't project `target_prompt_id`/`brain_session_id` into lineage graph

## Recommendation

**Land status**: Stage-1 chain stays merged. The chain is contract-correct, Rust-healthy, and Stage-2-ready in shape.

**Follow-up backlog** (priority order):

1. **Ops observability**: emit funnel signal for malformed-JSON; cap reason-string cardinality
2. **Typed errors**: `RouterError::Ledger(#[from] LedgerError)`
3. **Drain/reconciler unification** + enforce `drain_quiet_window` in reconciler
4. **Drain absolute-cap** alongside per-ack reset
5. **Refactor**: extract post-prompt symmetric arm helper
6. **Stage-2 prep**: `PeerMailboxLedger` filter/cursor on bulk reads; `Acceptance::Rejected`; `Acceptance` `#[non_exhaustive]`
7. **Additive events**: `WorkerPeerMessageDrainTimedOut` + `WorkerPeerMessageAckReceived`
