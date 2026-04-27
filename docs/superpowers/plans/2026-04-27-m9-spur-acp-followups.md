# M9 spur-acp follow-ups (post-M8 merge backlog)

> **Origin:** Wave F 3-gate review on `m8-continue`
> (codex `91c14cdd-…`, gemini `93c92d2a-…`, kimi `34c54a40-…`).
> No reviewer blocked merge (zero 🔴). All four items below are 🟡 advisory.
> Captured here so they survive the merge into main.

## 1. Hoist `SessionInfoCache` from `SessionDetailView` to the orchestrator (M9 fast-follow)

**Source:** gemini Wave F § 2 (`SHIPPABLE-WITH-FOLLOWUPS` verdict)

**Problem.** Today `SessionInfoCache { title, updated_at }` lives on
`SessionDetailView` (`crates/spur-tui/src/views/session_detail.rs:177`).
The view is destroyed on navigation away from the session detail screen,
so the cached `title` is wiped even though the underlying session is
still live in the orchestrator. The session registry — which already
owns session metadata like `agent_name`, `cwd`, `started_at` — cannot
read the title because it lives on a transient UI view.

**Fix.** Move the cache to the orchestrator's session entry (parallel to
`config_options` and `Arc<SpurAgentCaps>`). `apply_session_update`'s
`SessionInfoUpdate` arm should mutate the orchestrator entry; the view
reads it via the existing capability-cache plumbing M9 lands.

**Why deferred.** This refactor naturally couples to the M9 work that
plumbs `LoadSessionResponse` into `SpurAgentCaps` (also living on the
orchestrator). Doing both in one PR keeps the view↔orchestrator boundary
clear; doing the cache hoist alone now would touch the orchestrator
without yet having a consumer that benefits.

**Acceptance test.** Navigate away from a session detail and back; the
cached title remains stable across the round-trip.

## 2. Wave D — `authenticate(method_id, meta)` `_meta` plumbing

**Source:** gemini Wave F § 5 (strategic remaining-debt assessment)

**Problem.** The M8 plan
(`docs/superpowers/plans/2026-04-27-spur-acp-capability-aware-m8.md`
§ Wave D) calls for extending `authenticate`'s signature to thread an
optional request `_meta` through the wire and into the UI auth path.
This was tagged **stretch** in the plan and did not ship in
`m8-continue`. The plan's R3 risk register notes that the signature
break must be propagated to all call sites; passing `None` preserves
behavior at every site that doesn't yet care.

**Fix.** Two tasks per the plan:
* **D.1** — extend `authenticate` signature in `crates/spur-acp/src/connection/native.rs`
  to accept `Option<Meta>` and forward it to the SDK's `AuthenticateRequest`.
* **D.2** — pipe through to the UI auth path so dialogs (gemini gateway,
  kimi terminal-auth) can attach client-side context.

**Why deferred.** No M8-blocking caller; capturing the gemini-gateway
and kimi-terminal-auth UX work properly needs a separate spec (the M8
plan explicitly defers "full UI for gemini gateway / kimi terminal-auth"
to that later spec).

**Acceptance test.** A new `crates/spur-acp/tests` round-trip that
proves `_meta` survives `AuthenticateRequest` → SDK serialization → wire
→ deserialization with no field loss.

## 3. M8 manual smoke tests against live agents

**Source:** gemini Wave F § 4 (acceptance-criteria readiness)

The four manual smokes from the M8 plan's acceptance criteria are still
open after merge. They require live agent credentials and a developer
at a terminal — not pre-merge gating, but a release-readiness checklist:

* [ ] **Codex session** — `/model`, `/effort`, `/mode` all visible
  (codex advertises `set_config_option`).
* [ ] **gemini-cli session** — `/model` and `/effort` are absent or
  greyed-out (gemini lacks `set_config_option`).
* [ ] **claude-code-acp session** — `/model` dispatches
  `SetSessionModelRequest`, not `set_session_config_option`. Verify
  via the wire trace log for the dispatched method name.
* [ ] **`initialize` payload** — `clientCapabilities.meta.terminal_output: true`
  is present in spur's request payload (verify via wire log against
  codex). This gate is the prerequisite for M9's terminal recovery
  capability.

Each check is an O(1) manual verification once spur + the agent CLI are
running locally. Capture results in this doc when run.

## Out of scope for this follow-up doc

* **kimi 🟡 commit-message style** (Wave F § 5) — historical commits
  use noun-phrase predicates and a `red —` prefix on TDD-red commits.
  Rebasing 14 commits to fix purely cosmetic message style trades real
  history-rewriting cost for stylistic uniformity that the
  `<type>(<scope>): <sub-id>` prefix already guarantees. Skip.
