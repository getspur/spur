# ACP/TUI Remediation Design

**Status:** draft (design)
**Date:** 2026-04-22
**Owner:** TUI
**Related code:** `crates/spur-tui/src/components/react_trace/{dispatch.rs,mod.rs,render.rs}`, `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-core/src/{event_funnel.rs,orchestrator.rs}`
**Builds on:**
- RCA: `docs/rca/2026-04-22-acp-protocol-tui-rendering-fidelity-iceberg.md`
- Meta-RCA: `docs/rca/2026-04-22-acp-tui-rca-meta-review.md`

## 1. Problem

The current ACP/TUI integration is directionally correct but fails three core reliability properties:

1. **Protocol fidelity is lossy.** The TUI silently drops current ACP `ContentBlock` variants outside `Text`, renders tool diffs with a format that looks more truthful than it is, and hides user-visible `ResourceLink` content in some paths.
2. **State representation is split.** The same logical trace state is stored in multiple partially-overlapping structures (`text` vs markdown stream state, full-render cache vs compact cache, synthetic vs real `Act` entries), creating divergent behavior and stale UI math.
3. **Lifecycle state is inferred from the wrong signals.** The TUI uses a subset of visible content events as a proxy for whether a turn is active, which can hide live work, suppress cancel affordances, and drop important terminal outcomes from the primary surface.

The reviewed RCA priority order is:

1. F14 tool-bearing prefix stream gap
2. F3 `DelegationCompleted` no-op
3. F1 + F13 `ContentBlock` / `ResourceLink` loss
4. F9 markdown/text dual-source hazard
5. F12 duplicate `Act` entries (high severity, unproven reachability)
6. F11-revised stream timeout
7. F2 diff rendering in `dispatch.rs`
8. F5 non-markdown scroll metadata
9. F4 `tool_depth` theoretical cross-turn issue

This design turns that validated ordering into a remediation plan that is phased, testable, and bounded to ACP/TUI behavior.

## 2. Goals

1. Make active work visible and cancellable from the primary session view even when the turn begins with tool activity rather than text.
2. Make terminal worker outcomes visible in the main trace without requiring the inline workers panel to be expanded.
3. Replace silent data loss with fail-visible degradation for current ACP non-text content.
4. Remove split-source hazards where one logical trace entry can disagree with its own rendered or scrollable representation.
5. Fix reachable correctness and UX bugs first, while keeping lower-confidence or more architectural fixes explicitly sequenced behind them.

## 3. Non-goals

- Redesigning the ACP protocol or changing orchestrator event semantics.
- Building a rich-media TUI renderer for image/audio/resource payloads in this phase.
- Replacing the entire `ReactTrace` subsystem or cache architecture in one sweep.
- Reworking lineage projection, executor cards, or the workers panel outside what is necessary to make terminal outcomes visible in the session trace.
- Solving every theoretical ordering edge case in one pass. Some issues remain intentionally deferred if their current reachability is low and their fix interacts with broader architecture.

## 4. Design Principles / Invariants

### P1. Active work must be visible before it is verbose

The UI must not require a text chunk to recognize that a turn is running. Tool-bearing progress is active progress.

### P2. Primary interaction surfaces must retain terminal outcomes

A collapsed secondary panel may remove detail, but it must not be the only place a user can discover that delegated work failed, conflicted, or timed out.

### P3. Unsupported protocol content must degrade visibly, never silently

If the TUI cannot faithfully render a current ACP variant, it must render an explicit placeholder rather than erase the content from the trace.

### P4. One protocol entity maps to one trace entity

Each `tool_call_id` must correspond to at most one `Act` entry in the trace. Defensive synthesis is allowed, but it must be idempotent when the canonical event arrives later.

### P5. Render, debug, and scroll paths must observe the same logical content

If a trace entry contains content, the display renderer, scroll math, and debug/test read paths must not disagree on whether that content exists.

## 5. Proposed Design

The remediation is grouped into three phases that follow the RCA ordering and keep change sets reviewable.

### Phase 1: Reachable correctness and control fixes

Phase 1 addresses bugs that are either user-visible on common paths or directly affect control of an active session.

- F14: arm `stream_in_flight` on tool-bearing progress, not just text/thought chunks.
- F3: emit a terminal trace note for `DelegationCompleted`.
- F1 + F13: explicitly render `ResourceLink` and degrade other non-text content variants with placeholders.
- F9: synchronize `entry.text` with markdown stream raw text after append.
- F12: make `ToolCall` synthesis idempotent by merging into existing entries before pushing a new `Act`.

### Phase 2: Bounded representation and liveness fixes

Phase 2 addresses issues that are real but either less frequent or slightly broader in blast radius.

- F11-revised: add a timeout/heartbeat policy for `stream_in_flight` once armed.
- F2: replace `format_diff_truncated` with a real line-aware diff in `dispatch.rs`.
- F5: give non-markdown full-render scroll math the same metadata shape as markdown/full and compact modes.
- F4: replace blanket `tool_depth.clear()` on `TurnComplete` with targeted eviction of terminal entries plus a bounded map size.

### Phase 3: Architectural follow-ons

These remain out of scope for the immediate remediation but should be captured so Phase 1 and Phase 2 do not paint the codebase into a corner.

- Rich media trace surface (`TraceKind::Media` or equivalent) for proven ACP non-text payloads.
- Unified layout/cache abstraction so scroll, wrap, and render paths share one metadata contract.
- Formal turn-state modeling that does not infer lifecycle solely from trace-visible content.

## 6. Per-Finding Remediation Strategy

### F14. Tool-bearing prefix does not arm `stream_in_flight`

**Current behavior**
- `SessionDetailView` sets `stream_in_flight = true` only for `AgentThoughtChunk` and `AgentMessageChunk` in `session_detail.rs`.
- A valid ACP turn can begin with `ToolCall` / `ToolCallUpdate`, leaving the session apparently idle during that prefix window.
- `Esc`-cancel and the session-detail status hint both key off `stream_in_flight`.

**Design**
- Treat `ToolCall`, `ToolCallUpdate`, and `Plan` as stream-bearing progress for session lifecycle purposes.
- Arm `stream_in_flight` on the first tool-bearing update exactly as we already arm it on text/thought chunks.
- Arm on `Plan` for the same reason: it is already rendered into the primary trace and may precede the first text chunk in plan-oriented turns.
- Keep `TurnComplete` as the terminal clear point.
- Continue to exclude `UsageUpdate` and `CurrentModeUpdate`; they update mirrored session state but do not by themselves prove visible turn progress.

**Why first**
- This is the highest-priority reachable bug because it affects user control and observability of a live turn.

**Regression coverage**
- Add a test where the first live updates are tool-bearing and verify:
  - `stream_in_flight` becomes `true`
  - `Esc` emits `Action::CancelStream`
  - the status bar shows the in-flight hint

### F3. `DelegationCompleted` terminal status is discarded

**Current behavior**
- `SpurEventBody::DelegationCompleted` is a hard no-op in `SessionDetailView`.
- Inline executor cards carry the status, but a collapsed workers panel can hide that outcome from the primary interaction surface.

**Design**
- When a terminal delegation status arrives for the current session, push a concise terminal note into the main trace.
- Correlate the terminal note to the most recent matching `Delegate` entry when possible by matching `DelegationCompleted.worker_session` against the `executor_id` attached by `DelegationDispatched`.
- If no matching `Delegate` entry exists, still emit the terminal note as a session-level observation. This preserves setup-failure and pre-spawn cancel visibility without pretending correlation we do not have.
- The trace entry should be written as an informational/observational note, not as a fabricated agent message.
- The wording should summarize only the terminal outcome needed to preserve user understanding:
  - failed
  - conflicted
  - modified/succeeded with relevant qualification if needed
  - timed out

**Why second**
- This is a direct information-loss issue on a common workflow and does not depend on protocol anomalies.

**Regression coverage**
- Add tests for terminal delegation states with workers collapsed and expanded.
- Add one test where `DelegationDispatched` established an `executor_id` and verify the terminal note lands next to the correlated delegate flow.
- Add one test where no `executor_id` exists yet and verify the terminal note still appears as an uncorrelated session-level observation.
- Assert that the trace gets a terminal note regardless of panel visibility.

### F1 + F13. Non-text ACP `ContentBlock` variants are silently dropped

**Current behavior**
- `extract_text()` in `dispatch.rs` only preserves `ContentBlock::Text`.
- `UserMessageChunk` also only preserves `Text`, so a user-visible `ResourceLink` can disappear from the echoed trace.
- Current ACP schema includes `Text`, `Image`, `Audio`, `ResourceLink`, and `Resource`.

**Design**
- Introduce explicit placeholder rendering for current non-text ACP content in ACP/react-trace dispatch paths.
- Render `ResourceLink` as a stable, human-readable placeholder, for example `[mention: <name>]`, rather than dropping it.
- Render unsupported current variants with fail-visible placeholders such as:
  - `[image omitted]`
  - `[audio omitted]`
  - `[resource omitted]`
- Apply the same policy in both:
  - `extract_text()` / message-chunk handling
  - `UserMessageChunk`

**Why third**
- This is a current protocol-compatibility bug, but it is fidelity loss rather than loss of control.

**Regression coverage**
- Add one dispatch test for each current ACP variant across agent-message and user-message paths.
- Assert the trace contains placeholders rather than empty content.

### F9. Markdown stream and `entry.text` diverge

**Current behavior**
- In markdown mode, `append_message()` stores streamed content in the markdown stream and sets `entry.text = String::new()`.
- `render_to_strings()` compensates by reading `markdown.raw_text()`, but any direct read of `entry.text` sees an empty string.

**Design**
- After each markdown append, synchronize `entry.text` to `stream.raw_text().to_string()`.
- Preserve the markdown stream as the rich render source, but ensure `text` is a faithful plain-text mirror rather than a second, empty truth.

**Why fourth**
- This is a cheap, high-confidence fix that removes a class of debugging and test hazards.

**Regression coverage**
- Add tests that append streamed markdown content and assert:
  - `entry.text` contains the raw text
  - markdown rendering still works
  - `render_to_strings()` output is unchanged except for correctness of direct text reads

### F12. Defensive synthesis can create duplicate `Act` entries

**Current behavior**
- `ToolCallUpdate` can synthesize an `Act` if no matching tool call exists yet and enough metadata is present.
- If the canonical `ToolCall` later arrives, the current implementation unconditionally pushes another `Act`.
- `find_act_by_id_mut()` scans backward, so later updates attach to the second entry and orphan the first.

**Design**
- Treat synthesis as provisional identity creation.
- In the `ToolCall` arm, first check for an existing `Act` with the same `tool_call_id`.
- If found, merge the canonical metadata into the existing entry rather than pushing another one.
- The merge contract should preserve identity (`tool_call_id`) and existing timestamp ordering, fill canonical fields (`tool`, `family`, `input`, status), and only replace fallback text when the synthetic entry did not already accumulate better content.
- Keep the synthesis path because it is cheap defensive correctness, but document its reachability as unproven in current agents.

**Why fifth**
- High severity if it happens, but current observed reachability is weaker than the first four items.

**Regression coverage**
- Add a direct dispatch test for the sequence:
  1. `ToolCallUpdate` with synthesis metadata
  2. `ToolCall`
  3. later `ToolCallUpdate`
- Assert there is only one `Act` and that it receives the final merged status.

### F11-revised. `stream_in_flight` can stay armed forever once set

**Current behavior**
- The original late-chunk race is false under funnel/orchestrator ordering.
- However, once `stream_in_flight` is armed, the TUI has no timeout policy if the agent stalls without reaching `TurnComplete`.

**Design**
- Keep event ordering logic unchanged.
- Add a bounded liveness timeout/heartbeat policy on the TUI side for an armed stream.
- Use a fixed 60-second local timeout for the first implementation; keep it internal to the TUI rather than exposing a new configuration surface.
- The timeout should:
  - clear `stream_in_flight`
  - clear `cancelling_in_flight`
  - remove the stale cancel hint
  - emit a visible note that the stream appears stalled or expired
- The timeout must not synthesize `TurnComplete`; it should only repair stale local UI state.

**Why sixth**
- Important liveness issue, but it only matters after the UI already knows the turn is active.

**Regression coverage**
- Add tests for a stream that arms and then receives no further events past the timeout threshold.
- Assert the local flags clear (`stream_in_flight`, `cancelling_in_flight`) and a visible note is emitted.

### F2. `dispatch.rs::format_diff_truncated` produces misleading fake diffs

**Current behavior**
- Tool output diff rendering in `dispatch.rs` is delete-all/add-all with truncation and no hunk headers.
- `adapter/claude.rs::make_unified` is not the user-facing issue; it is input preview plumbing.

**Design**
- Replace `format_diff_truncated` with a real line-aware diff algorithm in `dispatch.rs`.
- The output should remain bounded in size, but it must no longer impersonate a unified diff without the structure users expect.
- If adding a crate dependency, scope it narrowly and keep features minimal.

**Why seventh**
- Semantic correctness issue, but below the more reachable lifecycle and fidelity bugs.

**Regression coverage**
- Add snapshot tests for:
  - small edits
  - pure insertions
  - pure deletions
  - truncation behavior on large inputs

### F5. Non-markdown full-render path lacks scroll metadata

**Current behavior**
- `layout_for_scroll()` returns `None` for the non-markdown full-render path because `LineCacheEntry` does not track per-entry row starts.

**Design**
- Extend non-markdown `LineCacheEntry` with `entry_row_starts`.
- Make the non-markdown full-render path return the same scroll metadata shape used by compact and markdown/full modes.
- Verify the existing generation/dirty invalidation path still rebuilds the non-markdown cache when entry content changes, so the new row-start metadata cannot drift from rendered rows.
- Keep the existing scroll-anchor contract; this is metadata parity, not a new scroll model.

**Why eighth**
- Real bug, but narrower in user impact than the earlier issues.

**Regression coverage**
- Add non-markdown full-render scroll tests that verify:
  - `layout_for_scroll()` returns metadata
  - scroll up/down/page operations now change the anchor instead of no-oping

### F4. `tool_depth` is cleared per turn

**Current behavior**
- `tool_depth.clear()` runs on `TurnComplete`.
- Child depth is computed from the parent id map.
- Cross-turn nesting is currently theoretical under observed SPUR agents.

**Design**
- Replace blanket clearing with targeted eviction of terminal/stale tool-depth entries.
- Add a hard cap (for example 128) so the map remains bounded.
- Do not attempt to build full cross-turn tool-lifecycle semantics in this remediation.

**Why ninth**
- Low-confidence reachability and comparatively low user impact today.

**Regression coverage**
- Add unit tests for:
  - targeted eviction of terminal entries
  - bounded map size
  - existing in-turn nesting behavior remaining unchanged

## 7. Data Flow / Lifecycle Notes

The stream-state fixes must keep three responsibilities separate:

1. **Arm state on observable progress.**
   - Text/thought chunks arm the stream.
   - Tool-bearing progress also arms the stream.
   - `Plan` updates also arm the stream because they are already rendered as primary-trace progress and can precede text in plan-oriented turns.

2. **Clear state on authoritative terminal events.**
   - `TurnComplete` remains the canonical end-of-turn signal.

3. **Repair stale local state when the terminal event never arrives.**
   - The timeout only clears stale local UI state after a bounded window.
   - It must not pretend the orchestrator or ACP connection emitted a real completion event.

This gives the following lifecycle:

1. First text/thought/tool-bearing update arrives.
2. `stream_in_flight = true`.
3. User can cancel and sees the in-flight hint immediately.
4. Either:
   - `TurnComplete` clears the state authoritatively, or
   - timeout clears the state as a local repair and emits a stall note.

This separation avoids reintroducing the disproven F11 race while still fixing the reachable liveness gap.

## 8. Testing / Verification Strategy

### Unit / component tests

- `dispatch.rs`
  - placeholder rendering for current ACP `ContentBlock` variants
  - idempotent `ToolCall` merge after synthesis
  - real diff formatting behavior
- `react_trace/mod.rs`
  - markdown `entry.text` synchronization
  - non-markdown scroll metadata parity
- `session_detail.rs`
  - tool-bearing or plan-bearing prefix arms `stream_in_flight`
  - `Esc` cancel path works after a tool-bearing first update
  - delegation terminal note emission
  - timeout clears stale local stream state
  - `tool_depth` targeted eviction

### Integration tests

- A session whose first visible activity is a tool call rather than a text chunk.
- A session whose first visible activity is a `Plan` update rather than a text chunk.
- A failed delegation with the workers panel collapsed.
- A user message containing `ResourceLink` content.
- A long-running stream that times out locally without `TurnComplete`.

### Manual verification

- Launch a session and reproduce a tool-first turn; verify the status hint and `Esc` cancel appear immediately.
- Reproduce a plan-first turn; verify the same status hint and cancel behavior appear before the first text chunk.
- Collapse the workers panel, force a failed delegation, and confirm a visible terminal note appears in the main trace.
- Send a message containing a mention/resource and confirm the trace shows a placeholder rather than dropping the content.
- Use a diff-producing tool output and confirm the rendered diff is line-aware and bounded.

## 9. Rollout / Sequencing

### Phase 1a

- F14
- F3

Reason: restore user control and primary-surface visibility first.

### Phase 1b

- F1 + F13
- F9

Reason: make trace content truthful and eliminate the easiest split-source hazard.

### Phase 1c

- F12

Reason: cheap correctness hardening with low migration risk, but after the more clearly reachable bugs.

### Phase 2a

- F11-revised

Reason: liveness repair now that stream arming semantics are correct.

### Phase 2b

- F2

Reason: isolated rendering improvement that may introduce a dependency.

### Phase 2c

- F5
- F4

Reason: representation parity and theoretical depth hardening after higher-value fixes land.

### Phase 3

- architectural follow-ons only after the remediations above are stable in production use

## 10. Risks / Open Questions

### R1. Placeholder wording could become noisy or misleading

Mitigation:
- Start with stable, low-drama placeholders.
- Prefer concise shape markers over speculative detail.

### R2. Terminal delegation notes may feel chatty on success paths

Mitigation:
- If success notes prove noisy, retain mandatory notes for failure/conflict/timeout and consider downgrading success to a subtler wording or tighter summary.

### R3. A stream timeout could hide a slow-but-legitimate turn

Mitigation:
- Treat the timeout as a UI repair, not a completion.
- Emit a visible “stalled” note rather than silently clearing.
- Start with a fixed 60-second threshold.
- Keep the threshold configurable only if evidence demands it; do not introduce premature configuration.

### R4. Phase 1a improves control before full fidelity lands

Mitigation:
- Accept the temporary intermediate state where tool-first and plan-first turns become visible/cancellable before non-text placeholders land.
- Keep Phase 1a and Phase 1b adjacent in rollout and avoid a long-lived release boundary between them.

### R5. Diff dependency choice can expand binary size or maintenance surface

Mitigation:
- Prefer a minimal line-diff crate with default features disabled.
- Keep the interface isolated inside `dispatch.rs`.

### R6. Scroll metadata parity may expose previously-untested assumptions

Mitigation:
- Add focused non-markdown full-render tests before changing user-facing behavior.
- Keep the existing anchor contract intact.

### R7. F4 may be tempting to over-engineer

Mitigation:
- Limit the change to targeted eviction plus a cap.
- Do not attempt speculative multi-turn tool-lifecycle modeling in this phase.

### Open questions

1. Should successful `DelegationCompleted` states always emit a trace note, or only non-success terminal states?

## 11. Acceptance Criteria

This design is complete when the resulting implementation can show, with tests and manual validation, that:

1. A turn that begins with tool activity is visibly in-flight and cancellable.
2. Delegation terminal outcomes are visible in the main session trace.
3. Current ACP non-text `ContentBlock` variants no longer disappear silently from the trace.
4. Markdown trace entries expose the same underlying text to render/debug/test paths.
5. `ToolCall` synthesis cannot produce duplicate `Act` rows for one `tool_call_id`.
6. A stalled in-flight session eventually clears its stale local hint without fabricating protocol ordering.
7. Non-markdown full-render scrolling is no longer a structural no-op.
8. The prioritized fixes land in phases without reopening the disproven F11 race claim.
