# M10.1 — Agent Model + Effort + Usage Status Surface — implementation plan

**Spec:** `docs/superpowers/specs/2026-04-28-agent-model-effort-surface-design.md`
**Predecessor:** `5cd1474b Merge branch 'm9-integration'` on `main`
**Date:** 2026-04-28
**Owner:** Kevin Truong
**Status:** ready for execution
**Bundle scope:** ~300 LOC. Ships ~80% of user-felt value (model label visible, effort label live-reactive, usage% with conditional placeholder).

This bundle delivers the visible status-bar surface + the wiring-gap fix that closes the orchestrator → TUI caps delivery hole found during dual review. It does NOT include `/model` mutation reactivity (deferred to M10.2) or legacy stream-json usage parity (deferred to M10.3).

---

## Wave 0 — `AgentSessionReady` carries caps (wiring-gap fix)

**Why:** today `BrainSession.spur_agent_caps` is set at `crates/spur-core/src/orchestrator.rs:292`, but no production code calls `SessionDetailView::set_spur_agent_caps()` (only the test at `crates/spur-tui/tests/session_detail_caps_filter.rs:59`). TUI's caps snapshot is always `None` in production. Without this fix, every label this bundle adds will render blank.

### Tasks

**0.1 (RED)** — `crates/spur-acp/tests/agent_session_ready_carries_caps.rs` (new): assert that `SpurEventBody::AgentSessionReady` constructed via the orchestrator's emit path includes `caps: Some(_)` populated from `BrainSession.spur_agent_caps` after `session/new`. Assert serde round-trip through `SpurEvent::now`. Failing because the field doesn't exist yet.

**0.2 (GREEN)** — extend payload:
```rust
// crates/spur-acp/src/domain/events.rs:355-366 (replace AgentSessionReady)
AgentSessionReady {
    session: SessionId,
    acp_session_id: String,
    brain: String,
    resumed: bool,
    cancel_mode: CancelMode,
    fs_unsafe: bool,
    /// Caps snapshot at session-create. None for resumed-pre-M9 sessions
    /// (M8 §F-3 permissive fallback).
    caps: Option<std::sync::Arc<crate::SpurAgentCaps>>,
}
```
- Update emit site at `crates/spur-core/src/orchestrator.rs:1046-equivalent` (search for current `SpurEventBody::AgentSessionReady {`) to populate `caps: brain.spur_agent_caps.clone()`.
- Update existing handler/test references that destructure `AgentSessionReady`.

**0.3 (GREEN)** — TUI handler:
```rust
// crates/spur-tui/src/app.rs ~ line 1363
SpurEventBody::AgentSessionReady { caps, .. } => {
    if let Some(view) = self.session_detail_for_mut(&session) {
        view.set_spur_agent_caps(caps);
    }
    // ... existing handler logic ...
}
```

**0.4 (REFACTOR)** — verify Wave 0 tests + downstream tests still pass:
```
cargo test -p spur-acp -p spur-core -p spur-tui
```

**Acceptance:** `view.spur_agent_caps.is_some()` after `AgentSessionReady` for fresh sessions; remains `None` for resumed-pre-M9.

---

## Wave A — `AgentKind` field on `SpurAgentCaps`

**Why:** quirks (`usage_supported`) need agent identity; today `SpurAgentCaps` has no kind. `AgentKind` enum already exists at `crates/spur-acp/src/types.rs:163-177`.

### Tasks

**A.1 (RED)** — unit test in `spur_agent_caps.rs`: `SpurAgentCaps::new(...)` accepts an `AgentKind` parameter and stores it on the returned struct. Failing because `new` doesn't accept it.

**A.2 (GREEN)** — extend struct + constructor:
```rust
// crates/spur-acp/src/spur_agent_caps.rs
pub struct SpurAgentCaps {
    pub agent: AgentCapabilities,
    pub modes: Option<SessionModeState>,
    pub models: Option<SessionModelState>,
    pub config_options: Vec<SessionConfigOption>,
    /// NEW
    pub agent_kind: AgentKind,
}

impl SpurAgentCaps {
    pub fn new(
        initialize: &InitializeResponse,
        new_session: &NewSessionResponse,
        agent_kind: AgentKind,  // NEW
    ) -> Self {
        Self {
            agent: initialize.agent_capabilities.clone(),
            modes: new_session.modes.clone(),
            models: new_session.models.clone(),
            config_options: new_session.config_options.clone().unwrap_or_default(),
            agent_kind,
        }
    }
}
```

**A.3 (GREEN)** — update all `SpurAgentCaps::new(...)` callers (~3-5 sites): pass `AgentKind` from `AgentConfig.kind` at the construction site. Search: `rg "SpurAgentCaps::new\("`.

**A.4 (REFACTOR)** — full crate test pass.

---

## Wave B — `agent_quirks.rs` module

**Why:** isolate per-agent allow/deny lists out of protocol-pure modules per gemini's amendment.

### Tasks

**B.1 (RED)** — `crates/spur-acp/src/agent_quirks.rs::tests::usage_emit_default_table`: assert truth-table:
```
ClaudeStreamJson  → true
ClaudeCodeAcp     → false   ← only deny
CodexAcp          → true
Kiro              → true
Generic           → true
```

**B.2 (GREEN)** — implement:
```rust
// crates/spur-acp/src/agent_quirks.rs (new)
//! Per-agent quirk allow/deny tables. Isolates transport-aware policy
//! from protocol-pure modules. New quirks land here.

use crate::types::AgentKind;

/// Whether this agent kind is expected to emit
/// `SessionUpdate::UsageUpdate`. Default: true. Explicit deny only for
/// agents verified upstream-blocked. See spec §3.
pub fn usage_emit_default(kind: AgentKind) -> bool {
    !matches!(kind, AgentKind::ClaudeCodeAcp)
}

#[cfg(test)]
mod tests { /* ... */ }
```
Wire into `crates/spur-acp/src/lib.rs`: `pub mod agent_quirks;`.

**B.3 (REFACTOR)** — verify quirks tests pass standalone: `cargo test -p spur-acp agent_quirks`.

---

## Wave C — `SpurAgentCaps` accessors

**Why:** display layer needs resolved labels, not raw IDs.

### Tasks

**C.1 (RED)** — three accessor tests in `spur_agent_caps.rs`:
- `current_model_label_resolves_via_available_models`: caps with `models = Some({ current_model_id: "gpt-5", available_models: [{ id: "gpt-5", name: "GPT-5" }] })` returns `Some("GPT-5")`.
- `current_model_label_falls_back_to_raw_id`: caps with `current_model_id` but empty `available_models` returns `Some("gpt-5")` (raw id).
- `current_model_label_returns_none_when_models_none`: caps with `models = None` returns `None`.
- `current_effort_label_resolves_via_select`: `config_options[id="reasoning_effort"]` is `Select { options: [{value: "medium", name: "Medium"}], current: "medium" }` returns `Some("Medium")`.
- `usage_supported_delegates_to_quirks`: caps with `agent_kind = ClaudeCodeAcp` returns false; `CodexAcp` returns true.

**C.2 (GREEN)** — implement per spec §5.1 (use `models.as_ref()?` not `flatten`).

**C.3 (REFACTOR)** — `cargo test -p spur-acp spur_agent_caps`.

---

## Wave D — TUI render

**Why:** the actual user-visible work.

### Tasks

**D.1 (RED)** — render-golden test in `crates/spur-tui/tests/render_golden.rs` (or new `status_bar_model_effort.rs`):
- Codex with caps showing `current_model_id="gpt-5-codex"`, effort `medium`: status bar contains `"GPT-5 Codex · Medium · ctx"`.
- Claude-code-acp with caps: status bar contains `"Sonnet 4.5"`, NO effort segment, NO usage segment (usage_supported=false).

**D.2 (GREEN)** — extend `StatusBarProps`:
```rust
// crates/spur-tui/src/components/status_bar.rs
pub struct StatusBarProps<'a> {
    // ... existing fields ...
    pub current_model_label: Option<&'a str>,
    pub current_effort_label: Option<&'a str>,
    pub usage_supported: bool,
}
```

**D.3 (GREEN)** — `truncate_model_label(label: &str, available: u16) -> Cow<'_, str>`:
1. If `label.chars().count() <= available as usize`, pass through.
2. Strip common prefixes: `claude-3-5-`, `claude-4-`, `gpt-5-`.
3. Strip date-like trailing `-YYYYMMDD` matching `\-\d{8}$`.
4. Cap at 14 chars; if still longer, truncate to 13 + `…`.

Unit-test the helper standalone for each rule.

**D.4 (GREEN)** — `metric_spans` adds two new spans between mode and usage:
```
[<mode>] <model> · <effort> · ctx <pct>%
```
- Hide `model` segment when `current_model_label.is_none()`.
- Hide `effort` segment when `current_effort_label.is_none()`.
- usage_text logic update: when `usage_supported && context_used.is_none()`, render ` ctx --%` placeholder; when `!usage_supported`, hide entirely (matches existing zero-when-empty path).

**D.5 (GREEN)** — `SessionDetailView` builds props:
```rust
let caps = self.spur_agent_caps.as_deref();
let model_label = caps.and_then(SpurAgentCaps::current_model_label);
let effort_label = caps.and_then(SpurAgentCaps::current_effort_label);
let usage_supported = caps.map(SpurAgentCaps::usage_supported).unwrap_or(true);
// ...
StatusBarProps {
    current_model_label: model_label.as_deref(),
    current_effort_label: effort_label.as_deref(),
    usage_supported,
    // ... existing fields ...
}
```

**D.6 (REFACTOR)** — full TUI test pass; update any render-golden snapshot fixtures that reference status_bar lines.

---

## Wave E — Polish + width tests

**Why:** confirm graceful degradation across realistic terminal widths.

### Tasks

**E.1 (RED)** — render-golden tests at three widths:
- 60-col (compact): `[<mode>] <model14> · ctx <pct>%` (effort dropped).
- 100-col (full): `[<mode>] <model> · <effort> · ctx <pct>%`.
- 160-col (full + headroom): same as 100-col, all hints visible.

**E.2 (GREEN)** — extend `metric_spans` compact branch to drop effort label first when narrow; drop model label suffix (`model: `) before dropping the value. Reuse the existing `compact` flag.

**E.3 (REFACTOR)** — review all render-golden snapshots for stability; commit golden updates as a single follow-up commit if changed.

---

## Dependencies & ordering

```
  Wave 0 ──┐
           ├──> Wave A ──> Wave B ──> Wave C ──> Wave D ──> Wave E
   (independent of each other below this line)
```

Wave 0 (event payload) can run in parallel with Wave A (caps struct field) at the cost of slightly larger test setup. Recommend strictly serial 0 → A → B → C → D → E for one worker; parallelizable for two workers (0+A on one, B+C on another, then D+E sequential).

## Acceptance gates

Each wave commits as a separate `feat`/`test`/`refactor` commit. Bundle merges to `main` only when:

1. `cargo test -p spur-acp -p spur-core -p spur-tui` green (excluding pre-existing `palette_rerank_bench_smoke` timing flake).
2. `cargo clippy -p spur-acp -p spur-core -p spur-tui -- -D warnings` clean on the diff.
3. Render-golden snapshots reviewed manually (no unintended layout regressions).
4. Manual smoke (recommended): launch spur with codex agent, observe status bar shows `gpt-5-codex · <effort> · ctx <pct>%`. Launch with claude-code-acp, observe status bar shows model name only.

## Out-of-scope reminders

- `/model` writeback to status bar — M10.2.
- Legacy raw stream-json usage parity — M10.3.
- Cost recompute on model change — M11.
- Auth `_meta` UX — separate spec.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Wave 0 emit-site change cascades into many test fixtures | Use serde defaults for new `caps` field — `Option<Arc<>>` default-`None` keeps deserialization compatible. Search-and-replace in tests should be ≤10 sites. |
| `AgentKind` parameter cascades into `SpurAgentCaps::new` callers | Single new param; rg + ed in 3-5 sites. Worker scripts include the rg command in red-test runs. |
| Render-golden snapshots churn | Expected. Commit snapshot diff in Wave E; reviewer gates on visual diff. |
| Truncation helper edge cases (multi-byte chars, e.g. `…`) | Use `chars().count()` not `len()`; helper has standalone unit tests covering ascii / utf-8 / surrogate cases. |
| Wave D status_bar render breaks when `usage_supported=true && context_used.is_some() && context_size.is_none()` | Render `<used>tk` (raw token count) instead of `--%` — distinct fallback documented in spec §5.4. Test covers. |

## Estimated effort

- Wave 0: 30 LOC + 1 integration test (~30 min)
- Wave A: 30 LOC + 1 unit test (~20 min)
- Wave B: 25 LOC + table test (~15 min)
- Wave C: 80 LOC + 5 unit tests (~45 min)
- Wave D: 120 LOC + 2 render-golden + helper unit tests (~90 min)
- Wave E: 30 LOC + 3 width-variant snapshots (~30 min)

**Total ~315 LOC, ~4h focused worker time** (single worker, serial waves).
