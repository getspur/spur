# ACP Mention Blocks and Display Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** evaluation of ACP content / prompt-turn (`https://agentclientprotocol.com/protocol/v1/content`, `…/prompt-turn`, `…/initialization`) plus in-repo TUI send/display paths. No separate design notebook.
**Formal @spec cells:** none
**Design epic:** `bd-1ow` (open)

**Goal:** Encode @-mentions as advertised ACP content blocks, and show each mention once in the TUI transcript/history.

**Architecture:** One flatten helper is the display contract (`Text` except `[UI hint]`, `ResourceLink`/`Resource` → `@name`). Inbound `UserMessageChunk` uses that helper and skips chunks already present in the local echo. `assemble_blocks` consults `SpurAgentCaps.agent.prompt_capabilities` (`embedded_context`, `image`) instead of always emitting `ResourceLink` + `Image`. Continuation delivery applies the same `embedded_context` gate.

**Tech Stack:** Rust 2021, `agent-client-protocol` schema v1 `ContentBlock` / `PromptCapabilities`, `scripts/spur-cargo` tests.

**Solve artifacts (evaluation):**
- `sol_a0574ef538fa4e32` sat — ResourceLink+Text is legal
- `sol_2ea7b8de3c134e70` unsat — ResourceLink-only is not illegal under embeddedContext
- `sol_bc834cb42eb84bb1` sat — preference miss when embeddedContext and TUI stays ResourceLink
- `sol_2479adcf2bf84573` sat — Image without image cap is reachable
- `sol_6a7d1786c9e048cc` sat — worker hint + @name duplicate is reachable
- `sol_1b4d160c519c48ed` sat — `ends_with("review ")` does not skip local `"review @src/lib.rs now"`

---

### Task 1: Flatten helper — skip UI hints, render Resource as @name

**Task ID:** `task-1`
**Beads:** `bd-j2e`
**Depends on:** none

**Files:**
- Modify: `crates/spur-tui/src/commands/submit_router.rs` (`blocks_preview`)
- Modify: `crates/spur-tui/src/input_history.rs` (`InputStateSnapshot::from_blocks`)
- Test: `crates/spur-tui/tests/submit_router.rs`

**Suggested Worker:** self (sequential in-session)

**Scope Boundary:**
- IN: preview/history flatten only
- OUT: assemble encoding, react_trace dispatch, continuation_bridge

**Acceptance Criteria:**
- [ ] `blocks_preview([hint Text, ResourceLink name=claude-code]) == "@claude-code"`
- [ ] `from_blocks` of the same does not restore `[UI hint]`
- [ ] `ContentBlock::Resource` with `file:///…/src/lib.rs` previews as `@src/lib.rs`
- [ ] Existing text-only preview tests still pass

**Implementation:**
- Export `flatten_prompt_block(block: &ContentBlock) -> Option<String>`
  - `Text` starting with `[UI hint]` → `None`
  - other `Text` → `Some(text)`
  - `ResourceLink` → `Some(format!("@{}", name))`
  - `Resource` → `Some(format!("@{}", name_from_resource(uri)))` where name is the last non-empty path segment
  - else `None`
- `blocks_preview` concatenates `flatten_prompt_block` results
- `from_blocks` uses the same flatten; `@…` ranges stay `RangeKind::Atom` with uri from the block

---

### Task 2: Coalesce UserMessageChunk against mention-shaped local echo

**Task ID:** `task-2`
**Beads:** `bd-3bm`
**Depends on:** `task-1`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs` (`append_user_message`)
- Test: existing `append_user_message_*` tests in `react_trace/mod.rs` plus one new mention-shaped case

**Suggested Worker:** self

**Scope Boundary:**
- IN: user-message chunk flatten + skip
- OUT: assemble_blocks, continuation_bridge

**Acceptance Criteria:**
- [ ] Seeded `"review @src/lib.rs now"` + inbound `"review "` does not grow the tail
- [ ] ResourceLink inbound flattens to `@name` and is skipped if already in tail
- [ ] New user text that is not a substring of the tail still appends
- [ ] Empty flatten (hint-only / ignored variant) is a no-op

---

### Task 3: Capability-aware assemble — Resource vs ResourceLink, gate Image

**Task ID:** `task-3`
**Beads:** `bd-23n`
**Depends on:** `task-1`

**Files:**
- Modify: `crates/spur-tui/src/commands/submit_router.rs`
- Modify: `crates/spur-tui/src/views/session_detail/input.rs` (pass caps into code-mention assemble)
- Modify: `crates/spur-tui/src/views/dashboard/mod.rs` (same)
- Test: `crates/spur-tui/src/commands/submit_router.rs` image tests + new embed tests

**Suggested Worker:** self

**Scope Boundary:**
- IN: assemble path + call sites that already have `SpurAgentCaps`
- OUT: continuation_bridge
- When `caps` is `None`: `image=false`, `embedded_context=false` (omit = unsupported)
- Embed only `file://` URIs that are readable UTF-8 and `<= PER_PROMPT_CAP_BYTES`; else ResourceLink
- Worker / datasource / issue / `graph://` URIs unchanged (graph still expands to Text)

**Acceptance Criteria:**
- [ ] `embedded_context=true` + readable `file://` → `ContentBlock::Resource` with uri + text
- [ ] `embedded_context=false` → `ResourceLink`
- [ ] `image=false` → no `ContentBlock::Image` (text fallback naming the attachment)
- [ ] `image=true` → existing PNG encode behavior
- [ ] Image unit tests pass an explicit `image: true` cap

---

### Task 4: Gate continuation Resource on embeddedContext

**Task ID:** `task-4`
**Beads:** `bd-1qg`
**Depends on:** `task-1`

**Files:**
- Modify: `crates/spur-core/src/continuation_bridge.rs`
- Test: existing continuation tests in that crate

**Suggested Worker:** self

**Scope Boundary:**
- IN: continuation prompt block choice
- OUT: TUI assemble

**Acceptance Criteria:**
- [ ] `embedded_context=false` → no `ContentBlock::Resource`
- [ ] `embedded_context=true` → existing JSON Resource body unchanged

---

## DAG

```
task-1 (bd-j2e)
   ├── task-2 (bd-3bm)
   ├── task-3 (bd-23n)
   └── task-4 (bd-1qg)
```

## Execution

User approved in-session sequential implementation (not orchestrator auto-dispatch).
