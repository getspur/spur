# Brain Delegation Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-agent capability descriptors, a structured `delegation_plan` MCP tool parameter, and a rewritten brain prompt that teaches the brain how to decompose, route, and shape delegation calls.

**Architecture:** Three independently-shippable phases. Phase 1 ships the data model + validator (invisible to brain). Phase 2 ships MCP tool enrichment (additive — brain ignores → prior behavior). Phase 3 ships the rewritten brain prompt behind a build-aware feature flag (replacement — user-observable).

**Tech Stack:** Rust 1.80+, tokio, serde, `toml` crate (already in workspace), `tracing`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-15-brain-delegation-framework-design.md`

---

## Prerequisites

Verify before starting:

- [ ] `brain-worker-refinement-design.md` **Change 1** (`brain_session_id` threading through `DelegationRequest`) is merged to the target branch. This plan assumes `DelegationRequest.brain_session_id: SessionId` already exists and is stamped by all 8 MCP handlers in `spur-mcp/src/server.rs`.

  Check with: `grep -n "brain_session_id" crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs`

  Expected: the field exists on `DelegationRequest` and is set in every `DelegationRequest { ... }` literal.

  If not merged: stop. Resume after that lands.

---

## File Structure

### New files

| Path | Responsibility |
|---|---|
| `crates/spur-acp/src/agents/mod.rs` | Module root re-exporting defaults + public API |
| `crates/spur-acp/src/agents/defaults.toml` | Bundled built-in delegation descriptors for 4 known agents |
| `crates/spur-acp/src/agents/defaults.rs` | Load + merge + synthesize + validate; `DelegationDescriptor`, `Tier`, lint APIs |
| `docs/spur/agent-config.md` | User-facing `[agents.entries.delegation]` reference |
| `docs/spur/contributing-agent-defaults.md` | How to tune `defaults.toml` |

### Modified files

| Path | What changes |
|---|---|
| `crates/spur-acp/src/lib.rs` | `pub mod agents;` + re-exports |
| `crates/spur-acp/src/config/mod.rs` | Add `DelegationDescriptor` field to `AgentConfig`; call `apply_builtin_defaults` + `validate_delegation_config` from config load |
| `crates/spur-acp/src/domain/delegation.rs` | New `DelegationPlan`, `PlanCandidate`, `PlanSubtask` structs |
| `crates/spur-acp/src/domain/events.rs` | `ReviewPayload` gains `delegation_plan` + `chosen_matches_dispatched`; `DelegationRequested` event body gains `delegation_plan` |
| `crates/spur-mcp/src/server.rs` | Extend `WorkerInfo` with tier/description/good_for/avoid_for/output_shape/cost_tier |
| `crates/spur-mcp/src/tools.rs` | Upgrade 3 tool descriptions; add `delegation_plan` input schema; add field to `DelegationRequest`; stamp in 8 handlers |
| `crates/spur-core/src/orchestrator.rs` | Prompt rewrite via helper fns + feature-flag gate + prompt logging + `build_worker_info` call at 3 spawn sites + mismatch detection + `normalize_agent_name` + `ReviewPayload` population |
| `.spur/config.toml.example` | Commented `[delegation]` block per agent |

---

## Phase 1 — Data Model + Defaults + Validator

Ships invisibly. No brain behavior change. Pre-existing configs continue to parse and run identically.

### Task 1: Add `Tier` enum and `DelegationDescriptor` struct

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (add struct after existing `AgentConfig`)
- Test: same file (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

At the bottom of `crates/spur-acp/src/config/mod.rs`, add:

```rust
#[cfg(test)]
mod delegation_descriptor_tests {
    use super::*;

    #[test]
    fn descriptor_deserializes_from_partial_toml() {
        let toml = r#"
            description = "test agent"
            tier = "specialist"
            good_for = ["a", "b"]
        "#;
        let d: DelegationDescriptor = toml::from_str(toml).unwrap();
        assert_eq!(d.description.as_deref(), Some("test agent"));
        assert!(matches!(d.tier, Some(Tier::Specialist)));
        assert_eq!(d.good_for, vec!["a".to_string(), "b".to_string()]);
        assert!(d.avoid_for.is_empty());
        assert!(d.inherit_defaults); // default true
    }

    #[test]
    fn descriptor_default_is_empty_and_inherits() {
        let d = DelegationDescriptor::default();
        assert!(d.description.is_none());
        assert!(d.good_for.is_empty());
        assert!(d.inherit_defaults);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-acp --lib delegation_descriptor_tests`
Expected: FAIL — `DelegationDescriptor` / `Tier` undefined.

- [ ] **Step 3: Implement the types**

In `crates/spur-acp/src/config/mod.rs`, add just above `pub struct AgentConfig`:

```rust
/// Per-agent task-routing descriptor. Feeds both the brain prompt and
/// `list_available_workers` tool response. See design spec section A.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DelegationDescriptor {
    /// One-line human summary. Rendered into the workers-block of the
    /// brain prompt when non-empty.
    pub description:        Option<String>,
    /// Routing preference signal. Specialists are preferred when their
    /// good_for matches; generalists are fallback.
    pub tier:               Option<Tier>,
    /// Positive task patterns. Used by the brain to route.
    pub good_for:           Vec<String>,
    /// Negative task patterns. Soft signal; brain MAY override with
    /// stated rationale when no better agent exists.
    pub avoid_for:          Vec<String>,
    /// Held back from workers-block; injected into per-dispatch task
    /// prompt only.
    pub strengths:          Vec<String>,
    /// Held back from workers-block; injected into per-dispatch task
    /// prompt only.
    pub limitations:        Vec<String>,
    /// Held back from routing; shown to brain when dispatching so it
    /// can shape CONTEXT appropriately.
    pub input_expectations: Option<String>,
    /// Routing-relevant via `list_available_workers`. Brain uses for
    /// EXPECTED_OUTPUT section of dispatched task prompt.
    pub output_shape:       Option<String>,
    /// Default true. When false, user fields are used verbatim
    /// (including empty vecs — no built-in merge).
    #[serde(default = "default_inherit_defaults")]
    pub inherit_defaults:   bool,
}

impl Default for DelegationDescriptor {
    fn default() -> Self {
        Self {
            description: None, tier: None,
            good_for: Vec::new(), avoid_for: Vec::new(),
            strengths: Vec::new(), limitations: Vec::new(),
            input_expectations: None, output_shape: None,
            inherit_defaults: true,
        }
    }
}

fn default_inherit_defaults() -> bool { true }

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier { Specialist, Generalist }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-acp --lib delegation_descriptor_tests`
Expected: PASS (2/2).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "feat(spur-acp): add DelegationDescriptor + Tier types"
```

### Task 2: Wire `DelegationDescriptor` into `AgentConfig`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs:21+` (inside `AgentConfig` struct)

- [ ] **Step 1: Write the failing test**

Append to the `delegation_descriptor_tests` module:

```rust
    #[test]
    fn agent_config_parses_delegation_sub_table() {
        let toml = r#"
            name = "claude-x"
            command = "claude"
            transport = "acp"

            [delegation]
            description = "custom claude variant"
            good_for = ["one-offs"]
        "#;
        let cfg: AgentConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delegation.description.as_deref(), Some("custom claude variant"));
        assert_eq!(cfg.delegation.good_for, vec!["one-offs".to_string()]);
    }

    #[test]
    fn agent_config_without_delegation_block_uses_defaults() {
        let toml = r#"
            name = "bare"
            command = "bare"
            transport = "acp"
        "#;
        let cfg: AgentConfig = toml::from_str(toml).unwrap();
        assert!(cfg.delegation.description.is_none());
        assert!(cfg.delegation.inherit_defaults);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-acp --lib delegation_descriptor_tests`
Expected: FAIL — `AgentConfig` missing `delegation` field.

- [ ] **Step 3: Add the field**

In `crates/spur-acp/src/config/mod.rs`, locate `pub struct AgentConfig { ... }` and add BEFORE the closing brace:

```rust
    /// Task-routing descriptor for delegation decisions. See
    /// `docs/spur/agent-config.md` and the delegation-framework spec.
    #[serde(default)]
    pub delegation: DelegationDescriptor,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-acp --lib delegation_descriptor_tests`
Expected: PASS (4/4).

- [ ] **Step 5: Verify no other tests break**

Run: `cargo test -p spur-acp`
Expected: all pre-existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "feat(spur-acp): add delegation descriptor field to AgentConfig"
```

### Task 3: Create `agents/` module + `defaults.toml`

**Files:**
- Create: `crates/spur-acp/src/agents/mod.rs`
- Create: `crates/spur-acp/src/agents/defaults.toml`
- Create: `crates/spur-acp/src/agents/defaults.rs`
- Modify: `crates/spur-acp/src/lib.rs` (add `pub mod agents`)

- [ ] **Step 1: Create `defaults.toml` with all 4 built-in descriptors**

Create `crates/spur-acp/src/agents/defaults.toml`:

```toml
# Built-in delegation descriptors. Coarse, imperative, stable.
# Tune carefully — every bump ships to all users on next release.
# See docs/spur/contributing-agent-defaults.md for guidelines.

[claude-code-acp]
description = "Generalist coding agent; strong at greenfield + refactors."
tier = "generalist"
good_for = [
  "multi-file refactors",
  "writing new modules from spec",
  "test authoring",
  "code review with rationale",
]
avoid_for = ["kiro vendor-ext command invocation"]
strengths = ["long-context reasoning", "diff-shaped output"]
limitations = ["no network access beyond allowlisted tools"]
input_expectations = "Provide acceptance criteria + file allowlist when scope > 3 files."
output_shape = "Unified diff + summary paragraph + test plan bullets."

[kiro]
description = "Specialist agent for Kiro spec-driven workflows and vendor commands."
tier = "specialist"
good_for = [
  "/spec-init, /spec-plan, /spec-execute tasks",
  "work requiring Kiro's internal spec schema",
]
avoid_for = [
  "tasks outside Kiro's spec/command workflow",
  "large refactors with no spec artifact",
]
strengths = ["structured spec output", "vendor-ext command integration"]
limitations = []
input_expectations = ""
output_shape = "Spec artifact + next-step suggestions."

[codex]
description = "Low-cost generalist; strong at narrowly-scoped edits."
tier = "generalist"
good_for = [
  "single-file edits",
  "syntactic refactors",
  "translating between language idioms",
]
avoid_for = [
  "multi-file coordination",
  "architectural decisions",
]
strengths = ["mechanical precision"]
limitations = ["limited broad-codebase reasoning"]
input_expectations = ""
output_shape = "Unified diff + one-sentence rationale."

[gemini]
description = "Generalist agent with strong multi-modal support."
tier = "generalist"
good_for = [
  "tasks involving images or diagrams",
  "exploratory analysis where context is ambiguous",
]
avoid_for = []
strengths = ["multi-modal input handling"]
limitations = []
input_expectations = ""
output_shape = "Narrative analysis + action items."
```

- [ ] **Step 2: Write the failing test for the loader**

Create `crates/spur-acp/src/agents/defaults.rs` with:

```rust
//! Built-in delegation descriptors.
//!
//! Loaded once from bundled `defaults.toml`. See design spec section A
//! and `docs/spur/contributing-agent-defaults.md`.

use crate::config::DelegationDescriptor;
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULTS_TOML: &str = include_str!("defaults.toml");

static DEFAULTS: OnceLock<HashMap<String, DelegationDescriptor>> = OnceLock::new();

fn defaults() -> &'static HashMap<String, DelegationDescriptor> {
    DEFAULTS.get_or_init(|| {
        toml::from_str::<HashMap<String, DelegationDescriptor>>(DEFAULTS_TOML)
            .expect("bundled defaults.toml must parse")
    })
}

/// Look up the built-in descriptor for a known agent name.
/// Returns `None` for unknown agents. `claude-code` aliases to
/// `claude-code-acp` because the stream-json variant has the same
/// semantics for delegation.
pub fn builtin_descriptor(agent_name: &str) -> Option<DelegationDescriptor> {
    let key = match agent_name {
        "claude-code" => "claude-code-acp",
        other => other,
    };
    defaults().get(key).cloned()
}

/// Names of agents with built-in descriptors, for testing and
/// documentation generation.
pub fn known_agents() -> &'static [&'static str] {
    &["claude-code-acp", "claude-code", "kiro", "codex", "gemini"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_defaults_toml_parses() {
        // Force initialization; panic if TOML is malformed.
        let _ = defaults();
    }

    #[test]
    fn every_known_agent_resolves_to_a_descriptor() {
        for name in known_agents() {
            let d = builtin_descriptor(name);
            assert!(d.is_some(), "no descriptor for known agent: {}", name);
            let d = d.unwrap();
            assert!(d.description.is_some(), "{}: missing description", name);
            assert!(d.tier.is_some(), "{}: missing tier", name);
            assert!(!d.good_for.is_empty(), "{}: empty good_for", name);
            assert!(d.output_shape.is_some(), "{}: missing output_shape", name);
        }
    }

    #[test]
    fn unknown_agent_returns_none() {
        assert!(builtin_descriptor("not-a-real-agent").is_none());
    }

    #[test]
    fn claude_code_aliases_to_claude_code_acp() {
        let a = builtin_descriptor("claude-code").unwrap();
        let b = builtin_descriptor("claude-code-acp").unwrap();
        assert_eq!(a.description, b.description);
    }
}
```

- [ ] **Step 3: Create the module root**

Create `crates/spur-acp/src/agents/mod.rs`:

```rust
//! Built-in agent descriptors and delegation-config validation.

pub mod defaults;

pub use defaults::{builtin_descriptor, known_agents};
```

- [ ] **Step 4: Register the module in `lib.rs`**

In `crates/spur-acp/src/lib.rs`, add next to the other `pub mod` lines:

```rust
pub mod agents;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-acp --lib agents::defaults::tests`
Expected: PASS (4/4).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/agents/ crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): add built-in delegation defaults registry"
```

### Task 4: Implement `apply_builtin_defaults` with merge semantics

**Files:**
- Modify: `crates/spur-acp/src/agents/defaults.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `defaults.rs`:

```rust
    use crate::config::{AgentConfig, Tier};

    fn minimal_agent(name: &str) -> AgentConfig {
        // Constructs a minimum-shape AgentConfig; relies on serde for
        // default values we don't care about here.
        let toml = format!(r#"name = "{}"
command = "x"
transport = "acp""#, name);
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn merge_fills_in_missing_from_default() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.description.is_some());
        assert!(!cfg.delegation.good_for.is_empty());
        assert!(cfg.delegation.tier.is_some());
    }

    #[test]
    fn merge_preserves_user_overrides() {
        let mut cfg = minimal_agent("claude-code-acp");
        cfg.delegation.description = Some("MY OVERRIDE".into());
        cfg.delegation.good_for = vec!["custom".into()];
        apply_builtin_defaults(&mut cfg);
        assert_eq!(cfg.delegation.description.as_deref(), Some("MY OVERRIDE"));
        assert_eq!(cfg.delegation.good_for, vec!["custom".to_string()]);
    }

    #[test]
    fn merge_empty_vec_treated_as_inherit() {
        let mut cfg = minimal_agent("claude-code-acp");
        // good_for starts empty by default; should get populated
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.good_for.len() >= 3);
    }

    #[test]
    fn merge_inherit_defaults_false_keeps_empty() {
        let mut cfg = minimal_agent("claude-code-acp");
        cfg.delegation.inherit_defaults = false;
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.description.is_none());
        assert!(cfg.delegation.good_for.is_empty());
    }

    #[test]
    fn merge_unknown_agent_synthesizes_thin() {
        let mut cfg = minimal_agent("my-custom-agent");
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.description.is_some());
        assert!(cfg.delegation.description.as_ref().unwrap().contains("my-custom-agent"));
        assert!(cfg.delegation.good_for.is_empty());
        assert!(matches!(cfg.delegation.tier, Some(Tier::Generalist)));
    }

    #[test]
    fn merge_is_idempotent() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        let after_first = cfg.delegation.clone();
        apply_builtin_defaults(&mut cfg);
        assert_eq!(after_first.description, cfg.delegation.description);
        assert_eq!(after_first.good_for, cfg.delegation.good_for);
    }
```

Note: `DelegationDescriptor` needs `PartialEq` for some of these. If compile-error shows missing trait, derive it. If not: check via individual fields. Use individual-field comparison to avoid deriving PartialEq if it causes trouble. The test above uses individual comparisons, so no derive needed.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-acp --lib agents::defaults::tests`
Expected: FAIL — `apply_builtin_defaults` not defined.

- [ ] **Step 3: Implement the merge function**

In `crates/spur-acp/src/agents/defaults.rs`, before the `#[cfg(test)]` block, add:

```rust
use crate::config::{AgentConfig, Tier};

/// Merge built-in descriptor into an `AgentConfig`'s delegation field.
/// Per-field override semantics: user values win; missing fields and
/// empty vecs inherit from the default. When `inherit_defaults = false`,
/// user values are used verbatim (no merge).
///
/// Idempotent.
pub fn apply_builtin_defaults(cfg: &mut AgentConfig) {
    if !cfg.delegation.inherit_defaults {
        return;
    }
    match builtin_descriptor(&cfg.name) {
        Some(default) => {
            let user = &mut cfg.delegation;
            if user.description.is_none() { user.description = default.description; }
            if user.tier.is_none() { user.tier = default.tier; }
            if user.good_for.is_empty() { user.good_for = default.good_for; }
            if user.avoid_for.is_empty() { user.avoid_for = default.avoid_for; }
            if user.strengths.is_empty() { user.strengths = default.strengths; }
            if user.limitations.is_empty() { user.limitations = default.limitations; }
            if user.input_expectations.is_none() {
                user.input_expectations = default.input_expectations;
            }
            if user.output_shape.is_none() {
                user.output_shape = default.output_shape;
            }
            tracing::info!(agent = %cfg.name, "applied built-in delegation descriptor");
        }
        None => {
            // No built-in default. Thin-synthesize only if user config
            // is fully empty — otherwise leave user's partial config
            // alone.
            let user = &cfg.delegation;
            let is_empty = user.description.is_none()
                && user.tier.is_none()
                && user.good_for.is_empty()
                && user.avoid_for.is_empty();
            if is_empty {
                cfg.delegation.description = Some(
                    format!("{} agent (no descriptor configured)", cfg.name)
                );
                cfg.delegation.tier = Some(Tier::Generalist);
                tracing::info!(agent = %cfg.name, "synthesized thin delegation descriptor");
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p spur-acp --lib agents::defaults::tests`
Expected: PASS (10/10 including existing tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/agents/defaults.rs
git commit -m "feat(spur-acp): implement apply_builtin_defaults merge logic"
```

### Task 5: Implement `validate_delegation_config` lints

**Files:**
- Modify: `crates/spur-acp/src/agents/defaults.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `defaults.rs`:

```rust
    #[test]
    fn lint_flags_oversized_good_for_entry() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.good_for = vec![
            "a".repeat(90),  // over 80 chars
            "ok short entry".into(),
        ];
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("exceeds 80")));
    }

    #[test]
    fn lint_flags_oversized_avoid_for_entry() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.avoid_for = vec!["a".repeat(81)];
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("avoid_for")));
    }

    #[test]
    fn lint_flags_worker_without_description() {
        // my-agent has no built-in default; no user description; worker role
        let mut cfg = minimal_agent("my-agent");
        // Note: default role is `Both` which is worker-capable
        cfg.delegation.description = None;
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("description")));
    }

    #[test]
    fn lint_flags_worker_without_good_for() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.description = Some("something".into());
        cfg.delegation.good_for = vec![];
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("good_for")));
    }

    #[test]
    fn lint_flags_capability_mismatch() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.good_for = vec!["plan mode refactors".into()];
        // capabilities stays empty by default
        let msgs = validate_delegation_config(&[cfg]);
        assert!(
            msgs.iter().any(|m| m.message.contains("plan_mode")),
            "expected plan_mode mismatch warning, got: {:?}",
            msgs.iter().map(|m| &m.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn lint_clean_config_produces_no_warnings() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.is_empty(), "expected no warnings, got: {:?}",
                msgs.iter().map(|m| &m.message).collect::<Vec<_>>());
    }

    #[test]
    fn lint_counts_chars_not_bytes() {
        // Non-ASCII: each char is multi-byte but counts as 1 char.
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.good_for = vec!["日".repeat(50)];  // 50 chars, 150 bytes
        let msgs = validate_delegation_config(&[cfg]);
        // Should NOT flag — 50 chars is under 80.
        assert!(!msgs.iter().any(|m| m.message.contains("exceeds 80")),
                "should not flag 50-char entry even though it's 150 bytes");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-acp --lib agents::defaults::tests`
Expected: FAIL — `validate_delegation_config` and `LintMessage` not defined.

- [ ] **Step 3: Implement the validator**

In `crates/spur-acp/src/agents/defaults.rs`, append (before `#[cfg(test)]`):

```rust
/// Keyword table for lint #4 (capability/descriptor cross-check).
///
/// MAINTENANCE NOTE: when a new token is added to `AgentConfig::capabilities`,
/// add the corresponding trigger keywords here so the lint flags
/// good_for entries that reference the capability without declaring it.
const CAPABILITY_KEYWORDS: &[(&str, &[&str])] = &[
    ("plan_mode",      &["plan mode", "plan-mode", "planning"]),
    ("usage",          &["usage tracking", "token counting"]),
    ("load_session",   &["session resume", "load_session"]),
    ("list_sessions",  &["list_sessions"]),
    ("session_resume", &["session_resume"]),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintLevel { Warn, Error }

#[derive(Debug, Clone)]
pub struct LintMessage {
    pub level:   LintLevel,
    pub agent:   String,
    pub message: String,
}

/// Run all delegation-config lints over the given AgentConfigs.
/// Call AFTER `apply_builtin_defaults` so inherited values are visible.
/// All v1 lints emit `Warn` level; user sees them but startup continues.
pub fn validate_delegation_config(cfgs: &[AgentConfig]) -> Vec<LintMessage> {
    let mut msgs = Vec::new();
    for cfg in cfgs {
        lint_length(cfg, &mut msgs);
        lint_worker_without_description(cfg, &mut msgs);
        lint_worker_without_good_for(cfg, &mut msgs);
        lint_capability_mismatch(cfg, &mut msgs);
    }
    msgs
}

fn lint_length(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    for (i, entry) in cfg.delegation.good_for.iter().enumerate() {
        if entry.chars().count() > 80 {
            out.push(LintMessage {
                level:   LintLevel::Warn,
                agent:   cfg.name.clone(),
                message: format!(
                    "good_for[{}] exceeds 80 chars; use a short task pattern, not a sentence",
                    i
                ),
            });
        }
    }
    for (i, entry) in cfg.delegation.avoid_for.iter().enumerate() {
        if entry.chars().count() > 80 {
            out.push(LintMessage {
                level:   LintLevel::Warn,
                agent:   cfg.name.clone(),
                message: format!(
                    "avoid_for[{}] exceeds 80 chars; use a short task pattern, not a sentence",
                    i
                ),
            });
        }
    }
}

fn lint_worker_without_description(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    if cfg.role.is_worker_capable() && cfg.delegation.description.is_none() {
        out.push(LintMessage {
            level:   LintLevel::Warn,
            agent:   cfg.name.clone(),
            message: "worker-capable but has no delegation.description — routing will be weak".into(),
        });
    }
}

fn lint_worker_without_good_for(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    if cfg.role.is_worker_capable() && cfg.delegation.good_for.is_empty() {
        out.push(LintMessage {
            level:   LintLevel::Warn,
            agent:   cfg.name.clone(),
            message: "worker-capable but no delegation.good_for entries — brain has no positive routing signal".into(),
        });
    }
}

fn lint_capability_mismatch(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    let joined = cfg.delegation.good_for.join(" ").to_lowercase();
    for (token, keywords) in CAPABILITY_KEYWORDS {
        for kw in keywords.iter() {
            if joined.contains(&kw.to_lowercase())
                && !cfg.capabilities.iter().any(|c| c == token) {
                out.push(LintMessage {
                    level:   LintLevel::Warn,
                    agent:   cfg.name.clone(),
                    message: format!(
                        "delegation.good_for references {} but capabilities does not declare {}",
                        kw, token
                    ),
                });
                break; // one message per token
            }
        }
    }
}
```

- [ ] **Step 4: Add the `is_worker_capable()` method on `AgentRole` if missing**

Check: `grep -n "is_worker_capable\|worker_capable" crates/spur-acp/src/types.rs crates/spur-acp/src/config/`

If missing, add to `crates/spur-acp/src/types.rs` on the `AgentRole` enum:

```rust
impl AgentRole {
    /// True if this role can receive delegation tasks.
    pub fn is_worker_capable(&self) -> bool {
        matches!(self, AgentRole::Worker | AgentRole::Both)
    }
}
```

(Verify enum variant names match the actual codebase — may be `Brain | Worker | Both` or similar.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-acp --lib agents::defaults::tests`
Expected: PASS (17/17 including existing).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/agents/defaults.rs crates/spur-acp/src/types.rs
git commit -m "feat(spur-acp): add delegation config lints"
```

### Task 6: Wire merge + validate into config load path

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (or wherever the toplevel config-loading function lives)

- [ ] **Step 1: Locate the config-load entry point**

Run: `grep -rn "pub fn load\|pub fn from_file\|pub fn parse" crates/spur-acp/src/config/ crates/spur-acp/src/registry.rs`

Expected: finds the function that reads `.spur/config.toml` and returns the parsed config structure. In spur this is likely in `registry.rs` or `config/mod.rs`.

- [ ] **Step 2: Write the failing integration test**

Add to `crates/spur-acp/src/agents/defaults.rs` tests:

```rust
    #[test]
    fn end_to_end_config_load_applies_defaults_and_lints() {
        use crate::agents::defaults::apply_builtin_defaults;
        let toml = r#"
            name = "claude-code-acp"
            command = "npx"
            args = ["--yes", "@agentclientprotocol/claude-agent-acp"]
            transport = "acp"
        "#;
        let mut cfg: AgentConfig = toml::from_str(toml).unwrap();
        apply_builtin_defaults(&mut cfg);
        // Descriptor filled from defaults.toml:
        assert!(cfg.delegation.description.is_some());
        assert!(!cfg.delegation.good_for.is_empty());
        // And the clean config should produce no lint warnings:
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.is_empty());
    }
```

- [ ] **Step 3: Run to verify it passes from the in-crate wiring**

Run: `cargo test -p spur-acp --lib end_to_end_config_load_applies_defaults_and_lints`
Expected: PASS (the wiring in this test calls the API directly; existing spur-acp load still needs updating in next step).

- [ ] **Step 4: Hook `apply_builtin_defaults` + `validate_delegation_config` into the toplevel load path**

In the load function found in Step 1, after parsing the config and iterating `agents.entries`, add:

```rust
use crate::agents::defaults::{apply_builtin_defaults, validate_delegation_config, LintLevel};

// After parse, before downstream consumers see the AgentConfig:
for cfg in &mut agents_entries {
    apply_builtin_defaults(cfg);
}

// Lint and surface warnings via tracing.
let msgs = validate_delegation_config(&agents_entries);
for m in &msgs {
    match m.level {
        LintLevel::Warn => {
            tracing::warn!(agent = %m.agent, "{}", m.message);
        }
        LintLevel::Error => {
            tracing::error!(agent = %m.agent, "{}", m.message);
        }
    }
}
```

Adjust field names to match actual code structure (`agents_entries` may be named `entries` or similar in the real code).

- [ ] **Step 5: Run the full spur-acp test suite**

Run: `cargo test -p spur-acp`
Expected: all pass; no pre-existing test broken.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/
git commit -m "feat(spur-acp): wire delegation defaults merge + lints into config load"
```

---

## Phase 2 — MCP Tool Surface + Events

Ships the `delegation_plan` tool parameter, enriched `WorkerInfo`, review-payload extension, mismatch detection, and tool-description upgrades. Additive; brain behavior unchanged by default.

### Task 7: Create `DelegationPlan` domain types

**Files:**
- Modify: `crates/spur-acp/src/domain/delegation.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs` (add re-exports)
- Modify: `crates/spur-acp/src/lib.rs` (if these types should be top-level exports)

- [ ] **Step 1: Write the failing test**

In `crates/spur-acp/src/domain/delegation.rs`, find the existing `#[cfg(test)]` block (around line 90+) and add:

```rust
    #[test]
    fn delegation_plan_deserializes_from_full_json() {
        let json = r#"{
            "candidates": [
                {"agent": "claude", "rationale": "default fit"},
                {"agent": "codex", "rationale": "cheaper alternative"}
            ],
            "decomposition": [
                {"subtask": "refactor auth", "parallelizable_with": ["refactor tests"]}
            ],
            "chosen": "claude",
            "rationale": "Scope > 3 files; claude is generalist."
        }"#;
        let plan: DelegationPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.chosen.as_deref(), Some("claude"));
        assert_eq!(plan.candidates.len(), 2);
        assert_eq!(plan.decomposition.len(), 1);
        assert!(plan.rationale.is_some());
    }

    #[test]
    fn delegation_plan_deserializes_from_minimal_json() {
        let json = r#"{"chosen": "kiro", "rationale": "spec work"}"#;
        let plan: DelegationPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.chosen.as_deref(), Some("kiro"));
        assert!(plan.candidates.is_empty());
        assert!(plan.decomposition.is_empty());
    }

    #[test]
    fn delegation_plan_deserializes_from_empty_json() {
        let json = r#"{}"#;
        let plan: DelegationPlan = serde_json::from_str(json).unwrap();
        assert!(plan.chosen.is_none());
        assert!(plan.candidates.is_empty());
    }
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-acp --lib delegation::`
Expected: FAIL — `DelegationPlan` not defined.

- [ ] **Step 3: Add the types**

In `crates/spur-acp/src/domain/delegation.rs`, after the existing `DelegationResult` definition:

```rust
/// Structured reasoning trace the brain passes alongside each
/// `delegate_to_worker` / `delegate_parallel` call. All fields optional;
/// permissive schema. See design spec section C.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DelegationPlan {
    /// Candidate agents the brain considered.
    #[serde(default)]
    pub candidates:    Vec<PlanCandidate>,
    /// Subtask breakdown for multi-task dispatches.
    #[serde(default)]
    pub decomposition: Vec<PlanSubtask>,
    /// The agent the brain committed to (or "self"/"parallel").
    pub chosen:        Option<String>,
    /// Short justification surfaced to the review gate.
    pub rationale:     Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PlanCandidate {
    pub agent:     Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PlanSubtask {
    pub subtask:             Option<String>,
    #[serde(default)]
    pub parallelizable_with: Vec<String>,
}
```

- [ ] **Step 4: Re-export from `domain/mod.rs`**

Modify `crates/spur-acp/src/domain/mod.rs`, update the line starting with `pub use delegation::...`:

```rust
pub use delegation::{
    DelegationPlan, DelegationResult, DelegationStatus,
    PlanCandidate, PlanSubtask, TimeoutFallback,
};
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-acp --lib delegation::`
Expected: PASS (3 new + existing).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/delegation.rs crates/spur-acp/src/domain/mod.rs
git commit -m "feat(spur-acp): add DelegationPlan domain types"
```

### Task 8: Extend `WorkerInfo` and add `build_worker_info` helper

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (WorkerInfo struct at line 86)
- Create: `crates/spur-acp/src/agents/worker_info.rs` (or add to defaults.rs)

- [ ] **Step 1: Write the failing test**

In `crates/spur-acp/src/agents/defaults.rs` tests module, append:

```rust
    #[test]
    fn build_worker_info_populates_all_fields() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        let info = build_worker_info(&cfg);
        assert_eq!(info.name, "claude-code-acp");
        assert!(info.description.is_some());
        assert!(info.tier.is_some());
        assert!(!info.good_for.is_empty());
        assert!(info.output_shape.is_some());
    }

    #[test]
    fn build_worker_info_handles_empty_descriptor() {
        let cfg = minimal_agent("unknown-agent");
        // without apply_builtin_defaults, all fields stay empty
        let info = build_worker_info(&cfg);
        assert_eq!(info.name, "unknown-agent");
        assert!(info.description.is_none());
        assert!(info.good_for.is_empty());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-acp --lib agents::defaults::tests::build_worker_info`
Expected: FAIL — `build_worker_info` and `WorkerInfo` not defined in scope.

- [ ] **Step 3: Extend `WorkerInfo` in spur-mcp**

In `crates/spur-mcp/src/server.rs`, find the existing `WorkerInfo` struct (around line 86) and REPLACE it with:

```rust
/// Descriptor for a worker-capable agent, returned by the
/// `list_available_workers` MCP tool.
///
/// Populated by `spur_acp::agents::defaults::build_worker_info`
/// from a merged `AgentConfig`. See design spec section C.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub name:         String,
    pub tier:         Option<String>,
    pub description:  Option<String>,
    #[serde(default)]
    pub good_for:     Vec<String>,
    #[serde(default)]
    pub avoid_for:    Vec<String>,
    pub output_shape: Option<String>,
    pub cost_tier:    Option<String>,
}
```

- [ ] **Step 4: Add `build_worker_info` helper**

Append to `crates/spur-acp/src/agents/defaults.rs` (public API section):

```rust
use spur_mcp::WorkerInfo;

/// Build the public `WorkerInfo` from a merged `AgentConfig`.
/// Call AFTER `apply_builtin_defaults` to see inherited values.
pub fn build_worker_info(cfg: &AgentConfig) -> WorkerInfo {
    WorkerInfo {
        name:         cfg.name.clone(),
        tier:         cfg.delegation.tier.map(|t| match t {
            Tier::Specialist => "specialist".into(),
            Tier::Generalist => "generalist".into(),
        }),
        description:  cfg.delegation.description.clone(),
        good_for:     cfg.delegation.good_for.clone(),
        avoid_for:    cfg.delegation.avoid_for.clone(),
        output_shape: cfg.delegation.output_shape.clone(),
        cost_tier:    Some(format!("{:?}", cfg.cost_tier).to_lowercase()),
    }
}
```

Note: this introduces a new dependency from `spur-acp` on `spur-mcp`. Check if that creates a cycle — both already link to `spur-acp`'s domain types, so `spur-mcp → spur-acp` is the existing direction. We need the reverse. Check:

Run: `grep -n '^spur-mcp' crates/spur-acp/Cargo.toml`

If the dep doesn't exist AND creates a cycle, instead DEFINE `build_worker_info` in `spur-mcp/src/server.rs` (next to `WorkerInfo`) accepting a `&AgentConfig` import from `spur_acp::config`. That's the correct dependency direction.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-acp --lib agents::defaults::tests::build_worker_info`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-acp/src/agents/defaults.rs
git commit -m "feat: extend WorkerInfo and add build_worker_info helper"
```

### Task 9: Wire `build_worker_info` into orchestrator spawn sites

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` at lines ~292, ~1183, ~1336

- [ ] **Step 1: Locate the three spawn sites**

Run: `grep -n "worker_capable()" crates/spur-core/src/orchestrator.rs`

Expected: three hits populating `WorkerInfo { name: ... }` with name-only entries.

- [ ] **Step 2: Replace each site's population logic**

At each of the three sites, replace the `let workers: Vec<WorkerInfo> = self.agents.worker_capable().map(|a| WorkerInfo { name: a.name.clone() }).collect();` pattern with a call to the shared helper. Example (exact diff depends on current code):

```rust
use spur_acp::agents::defaults::build_worker_info;

let workers: Vec<WorkerInfo> = self
    .agents
    .worker_capable()
    .map(|a| build_worker_info(a))
    .collect();
```

(Or whatever iterator method exists on the agents registry — preserve the current call shape.)

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-core`
Expected: compiles clean.

- [ ] **Step 4: Verify tests still pass**

Run: `cargo test -p spur-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): wire build_worker_info into spawn sites"
```

### Task 10: Add `delegation_plan` to `DelegationRequest` and stamp in MCP handlers

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` (DelegationRequest struct)
- Modify: `crates/spur-mcp/src/server.rs` (8 handlers that construct DelegationRequest)

- [ ] **Step 1: Add field to DelegationRequest**

In `crates/spur-mcp/src/tools.rs`, locate `pub struct DelegationRequest { ... }` and add after the `brain_session_id` field:

```rust
    /// Structured reasoning trace the brain passed with this call.
    /// None when brain omitted the parameter. Orchestrator uses this
    /// for reviewer-visibility and mismatch detection. See design
    /// spec section C.
    pub delegation_plan: Option<spur_acp::DelegationPlan>,
```

- [ ] **Step 2: Update the `delegation_plan` input schema in `delegate_to_worker_def`**

Modify `crates/spur-mcp/src/tools.rs:46` `delegate_to_worker_def()`. Replace its `input_schema` with:

```rust
input_schema: json!({
    "type": "object",
    "properties": {
        "agent": {
            "type": "string",
            "description": "Name of the worker agent to delegate to"
        },
        "task": {
            "type": "string",
            "description": "Task description for the worker. Structure as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT."
        },
        "context_files": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional supplementary file paths. Prefer inlining relevant excerpts in the task field's CONTEXT section."
        },
        "delegation_plan": {
            "type": "object",
            "description": "Structured reasoning for this delegation. At minimum pass {chosen, rationale}. For 2+ subtasks or >3 files, include candidates and decomposition.",
            "properties": {
                "candidates": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent":     { "type": "string" },
                            "rationale": { "type": "string" }
                        }
                    }
                },
                "decomposition": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "subtask":             { "type": "string" },
                            "parallelizable_with": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                },
                "chosen":    { "type": "string" },
                "rationale": { "type": "string" }
            }
        }
    },
    "required": ["agent", "task"]
}),
```

Also update the tool description:

```rust
description: "Delegate a task to a worker agent. Blocks until the worker completes. Pass a `delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT. Use `list_available_workers` when routing is ambiguous.".into(),
```

- [ ] **Step 3: Update `delegate_parallel_def` similarly**

Replace the `input_schema` for `delegate_parallel_def()` at `tools.rs:72`. The schema gains a top-level `delegation_plan` (one plan for the whole parallel dispatch):

```rust
input_schema: json!({
    "type": "object",
    "properties": {
        "tasks": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Worker agent name" },
                    "task":  { "type": "string", "description": "Task description" }
                },
                "required": ["agent", "task"]
            },
            "description": "List of tasks to delegate in parallel"
        },
        "delegation_plan": {
            "type": "object",
            "description": "Structured reasoning for the parallel dispatch. The `decomposition` section MUST demonstrate subtasks are independent.",
            "properties": {
                "candidates":    { "type": "array" },
                "decomposition": { "type": "array" },
                "chosen":        { "type": "string" },
                "rationale":     { "type": "string" }
            }
        }
    },
    "required": ["tasks"]
}),
```

And description:

```rust
description: "Delegate multiple tasks in parallel. Blocks until all complete. The `delegation_plan.decomposition` field MUST demonstrate subtasks are independent — no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker` serially.".into(),
```

- [ ] **Step 4: Update `list_available_workers_def` description**

```rust
description: "Returns tier, description, good_for, avoid_for, output_shape, and cost_tier for each worker. Call when the system-prompt one-liner is insufficient.".into(),
```

- [ ] **Step 5: Stamp `delegation_plan` in the 8 handlers**

In `crates/spur-mcp/src/server.rs`, find each `DelegationRequest { ... }` literal (8 sites per the spec). For handlers that come from tool calls that could carry `delegation_plan` (both `delegate_to_worker` and per-task entries of `delegate_parallel`), parse the field from the incoming JSON; for other tools (`get_issue`, `create_pr`, `report_progress`, etc.) set `delegation_plan: None`.

Pattern for `delegate_to_worker` handler:

```rust
let delegation_plan: Option<spur_acp::DelegationPlan> = params
    .get("delegation_plan")
    .and_then(|v| serde_json::from_value(v.clone()).ok());

let request = DelegationRequest {
    id: request_id,
    agent,
    task,
    context_files,
    respond_to: response_tx,
    brain_session_id: self.brain_session_id.clone(),
    delegation_plan,
};
```

Pattern for `delegate_parallel`: extract the top-level `delegation_plan` once; attach the SAME plan to every per-task `DelegationRequest`:

```rust
let shared_plan: Option<spur_acp::DelegationPlan> = params
    .get("delegation_plan")
    .and_then(|v| serde_json::from_value(v.clone()).ok());

for task_spec in tasks_array {
    let request = DelegationRequest {
        // ... per-task fields
        delegation_plan: shared_plan.clone(),
    };
    // ... send
}
```

For non-delegation handlers, just add `delegation_plan: None`.

- [ ] **Step 6: Run compile + tests**

Run: `cargo test -p spur-mcp && cargo test -p spur-core`
Expected: all pass. If the orchestrator destructures `DelegationRequest` in `handle_delegations`, also add `delegation_plan` to the destructure pattern (see orchestrator.rs:1911-1918 block).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/
git commit -m "feat(spur-mcp): add delegation_plan tool param + thread to DelegationRequest"
```

### Task 11: Add `normalize_agent_name` helper + mismatch detection

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Write the failing test**

In `crates/spur-core/src/orchestrator.rs` (or wherever orchestrator tests live), add:

```rust
#[cfg(test)]
mod normalize_tests {
    use super::normalize_agent_name;

    #[test]
    fn strips_acp_suffix() {
        assert_eq!(normalize_agent_name("claude-code-acp"), "claude-code");
        assert_eq!(normalize_agent_name("kiro-acp"), "kiro");
    }

    #[test]
    fn strips_cli_suffix() {
        assert_eq!(normalize_agent_name("gemini-cli"), "gemini");
    }

    #[test]
    fn lowercases() {
        assert_eq!(normalize_agent_name("CLAUDE"), "claude");
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(normalize_agent_name("  kiro  "), "kiro");
    }

    #[test]
    fn same_agent_matches_across_variants() {
        assert_eq!(
            normalize_agent_name("Claude-Code-ACP"),
            normalize_agent_name("claude-code")
        );
    }

    #[test]
    fn distinct_agents_do_not_collide() {
        assert_ne!(
            normalize_agent_name("our-claude"),
            normalize_agent_name("claude"),
        );
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-core --lib normalize_tests`
Expected: FAIL — `normalize_agent_name` not defined.

- [ ] **Step 3: Implement the helper**

In `crates/spur-core/src/orchestrator.rs`, add (near the top-level, before the main struct):

```rust
/// Normalize an agent name for equality comparison.
/// - Lowercases
/// - Trims surrounding whitespace
/// - Strips `-acp`, `_acp`, `-cli`, `_cli` suffixes
///
/// Used to compare `DelegationPlan.chosen` (possibly a short name
/// the brain chose) against the dispatched `agent` (possibly a
/// fully-qualified registered name like `claude-code-acp`).
pub fn normalize_agent_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    for suffix in ["-acp", "_acp", "-cli", "_cli"].iter() {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    lower
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p spur-core --lib normalize_tests`
Expected: PASS (6/6).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): add normalize_agent_name helper"
```

### Task 12: Add `delegation_plan` and `chosen_matches_dispatched` to `ReviewPayload`

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (ReviewPayload at line 29)

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-acp/src/domain/events.rs` (or its test module):

```rust
#[cfg(test)]
mod review_payload_tests {
    use super::*;
    use crate::domain::DelegationPlan;

    #[test]
    fn review_payload_default_has_none_plan() {
        let p = ReviewPayload {
            summary: "s".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
        };
        assert!(p.delegation_plan.is_none());
        assert!(p.chosen_matches_dispatched.is_none());
    }

    #[test]
    fn review_payload_round_trips_with_plan() {
        let plan = DelegationPlan {
            chosen: Some("kiro".into()),
            rationale: Some("because".into()),
            ..Default::default()
        };
        let p = ReviewPayload {
            summary: "".into(), diff_summary: None, pr_url: None, error: None,
            delegation_plan: Some(plan),
            chosen_matches_dispatched: Some(true),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ReviewPayload = serde_json::from_str(&json).unwrap();
        assert!(back.delegation_plan.is_some());
        assert_eq!(back.chosen_matches_dispatched, Some(true));
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-acp --lib review_payload_tests`
Expected: FAIL — fields missing.

- [ ] **Step 3: Add the fields**

In `crates/spur-acp/src/domain/events.rs:29`, modify `ReviewPayload`:

```rust
/// Payload carried with a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
    /// Structured delegation reasoning the brain emitted for this call.
    /// See design spec section C.5.
    #[serde(default)]
    pub delegation_plan: Option<crate::domain::DelegationPlan>,
    /// `Some(false)` when `delegation_plan.chosen` doesn't match the
    /// dispatched agent (after `normalize_agent_name`). Never blocks
    /// dispatch; exposed for reviewer visibility.
    #[serde(default)]
    pub chosen_matches_dispatched: Option<bool>,
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p spur-acp --lib review_payload_tests`
Expected: PASS (2/2).

- [ ] **Step 5: Verify no pre-existing tests break**

Run: `cargo test`
Expected: all pass. If any existing `ReviewPayload { ... }` literal breaks because the new fields don't have `#[serde(default)]`-equivalent struct defaults, add the two new fields to those literals (passing `None`, `None`).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-acp): add delegation_plan + chosen_matches_dispatched to ReviewPayload"
```

### Task 13: Add `delegation_plan` to `DelegationRequested` event body

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (DelegationRequested variant at line 179)

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-acp/src/domain/events.rs` tests:

```rust
    #[test]
    fn delegation_requested_event_carries_optional_plan() {
        use crate::domain::DelegationPlan;
        // Construct the event — exact field names depend on actual variant
        // layout; adjust per codebase.
        let plan = DelegationPlan {
            chosen: Some("claude".into()),
            ..Default::default()
        };
        // Sanity: can construct + serialize, and plan round-trips.
        let body = SpurEventBody::DelegationRequested {
            // ... other existing fields set to default/empty values
            delegation_plan: Some(plan.clone()),
            // ... other fields
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"delegation_plan\""));
    }
```

(This test stub is illustrative — the actual `DelegationRequested` variant has other required fields; fill them with realistic or default values from the existing code. The assertion that matters is that `delegation_plan` serializes.)

- [ ] **Step 2: Add the field to the variant**

Find the `DelegationRequested { ... }` variant in `SpurEventBody` (around line 179), and add a new field:

```rust
    DelegationRequested {
        // ... existing fields ...
        /// Optional structured plan the brain passed alongside the
        /// delegate_* call. See design spec section C.7.
        #[serde(default)]
        delegation_plan: Option<crate::domain::DelegationPlan>,
    },
```

- [ ] **Step 3: Update all emit sites**

Find every place that constructs `DelegationRequested { ... }` (orchestrator.rs, possibly review_sink.rs, etc.):

Run: `grep -rn "DelegationRequested {" crates/ --include='*.rs'`

At each site, add `delegation_plan: request.delegation_plan.clone()` (or `None` if the context doesn't have the DelegationRequest).

- [ ] **Step 4: Run the tests + full suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-acp): add delegation_plan to DelegationRequested event body"
```

### Task 14: Wire mismatch detection + ReviewPayload population

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (execute_delegation function)

- [ ] **Step 1: Locate the review-gate call site**

Run: `grep -n "ReviewPayload\s*{" crates/spur-core/src/orchestrator.rs`

Expected: finds one or more sites where ReviewPayload is constructed before handing off to review-gate.

- [ ] **Step 2: Compute `chosen_matches_dispatched` and populate**

At each ReviewPayload construction site inside `execute_delegation` (there may be only one primary site), compute the match:

```rust
let plan = request.delegation_plan.as_ref();
let chosen_matches_dispatched = plan
    .and_then(|p| p.chosen.as_ref())
    .map(|c| {
        normalize_agent_name(c) == normalize_agent_name(&request.agent)
    });

if chosen_matches_dispatched == Some(false) {
    tracing::warn!(
        session = %request.brain_session_id,
        chosen = %plan.and_then(|p| p.chosen.as_deref()).unwrap_or(""),
        dispatched = %request.agent,
        "delegation_plan.chosen does not match dispatched agent",
    );
}

let review_payload = ReviewPayload {
    // ... existing fields (summary, diff_summary, pr_url, error)
    delegation_plan: request.delegation_plan.clone(),
    chosen_matches_dispatched,
};
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test -p spur-core`
Expected: all pass.

- [ ] **Step 4: Write a new integration test for mismatch detection**

Add a test (in whatever test module spur-core uses for orchestrator-level tests) that:

1. Constructs a `DelegationRequest` with `agent = "kiro"` and `delegation_plan.chosen = "claude"`.
2. Synthesizes a minimal review-gate flow (may require a MockBrain; if test infrastructure doesn't support that yet, note as a TODO in the test and verify manually via TUI smoke).
3. Asserts the resulting `ReviewPayload.chosen_matches_dispatched == Some(false)` and that a warn log was emitted.

If the test harness doesn't support observing review payload at test time yet, write the lowest-level test possible: construct two strings, normalize both, compare. Add this as a unit test on the normalize+compare pattern:

```rust
#[test]
fn mismatch_detection_chosen_vs_dispatched_strings() {
    let dispatched = "kiro";
    let chosen = "claude";
    let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
    assert_eq!(matched, false);

    let dispatched = "claude-code-acp";
    let chosen = "claude";
    let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
    // claude-code-acp normalizes to "claude-code", so "claude" != "claude-code"
    assert_eq!(matched, false);

    let dispatched = "claude-code-acp";
    let chosen = "claude-code-acp";
    let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
    assert_eq!(matched, true);
}
```

Note the subtle edge: `claude` vs `claude-code-acp` normalizes to `claude` vs `claude-code` — they don't match. If that's surprising, consider enhancing `normalize_agent_name` to also strip `-code` suffix, OR accept that it's the user's responsibility to use consistent naming in descriptors.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): wire delegation_plan mismatch detection + ReviewPayload population"
```

---

## Phase 3 — Brain Prompt Rewrite + Feature Flag

Ships the observable behavior change. Guarded by build-aware feature flag (`"v1"` in dev builds, `"legacy"` in release builds at v1 ship).

### Task 15: Add `[brain.delegation]` feature flag config

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (or the `BrainConfig` struct home)

- [ ] **Step 1: Locate the `BrainConfig` / `[brain]` struct**

Run: `grep -rn "pub struct BrainConfig\|\\[brain\\]" crates/spur-acp/src/config/`

- [ ] **Step 2: Write the failing test**

Add to the config tests:

```rust
    #[test]
    fn brain_delegation_framework_defaults_per_build() {
        // Empty [brain.delegation] block → build-aware default.
        let toml = r#"
            [brain]
            default = "claude-code-acp"
        "#;
        let cfg: BrainConfig = toml::from_str(toml).unwrap();
        let expected = if cfg!(debug_assertions) { "v1" } else { "legacy" };
        assert_eq!(cfg.delegation.framework, expected);
    }

    #[test]
    fn brain_delegation_framework_explicit_v1() {
        let toml = r#"
            [brain]
            default = "claude-code-acp"
            [brain.delegation]
            framework = "v1"
        "#;
        let cfg: BrainConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delegation.framework, "v1");
    }
```

- [ ] **Step 3: Add the config struct**

Add to the config module:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BrainDelegationConfig {
    /// Which delegation framework version to use in the brain prompt.
    /// `"v1"` uses the rewritten prompt (workers block, dispatch
    /// procedure, delegation_plan guidance). `"legacy"` uses the
    /// pre-framework 5-line prose prompt. Build-aware default:
    /// debug builds default to `"v1"`; release builds default to
    /// `"legacy"` at v1 ship, flipping to `"v1"` at v2, removed at v3.
    pub framework: String,
}

impl Default for BrainDelegationConfig {
    fn default() -> Self {
        Self {
            framework: if cfg!(debug_assertions) { "v1".into() } else { "legacy".into() },
        }
    }
}
```

And add the field to the existing `BrainConfig`:

```rust
    #[serde(default)]
    pub delegation: BrainDelegationConfig,
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p spur-acp --lib brain_delegation`
Expected: PASS (2/2).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "feat(spur-acp): add [brain.delegation] framework feature flag"
```

### Task 16: Refactor `build_brain_prompt` into composable helpers

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (build_brain_prompt around line 1831)

- [ ] **Step 1: Write failing tests on the parts the helpers render directly**

The full prompt assembly threads through orchestrator state (agents registry, config). Rather than reconstructing a full orchestrator in unit tests, exercise the four pure pieces individually: the three static constants and `render_workers_block` via a stub over `worker_capable()`.

Add to `crates/spur-core/src/orchestrator.rs` (or a new `mod prompt_v1_tests` inside it):

```rust
#[cfg(test)]
mod prompt_v1_tests {
    use super::*;

    // --- Static-constant tests: no fixture needed ---

    #[test]
    fn dispatch_procedure_contains_required_keywords() {
        assert!(DISPATCH_PROCEDURE.contains("When to delegate vs. do it yourself"));
        assert!(DISPATCH_PROCEDURE.contains("Do it yourself when:"));
        assert!(DISPATCH_PROCEDURE.contains("Delegate when:"));
        assert!(DISPATCH_PROCEDURE.contains("specialist"));
        assert!(DISPATCH_PROCEDURE.contains("avoid_for is a SOFT signal"));
    }

    #[test]
    fn plan_requirement_shows_full_and_minimal_shapes() {
        assert!(PLAN_REQUIREMENT.contains("delegation_plan"));
        assert!(PLAN_REQUIREMENT.contains("candidates"));
        assert!(PLAN_REQUIREMENT.contains("decomposition"));
        assert!(PLAN_REQUIREMENT.contains("minimum shape"));
        assert!(PLAN_REQUIREMENT.contains(">=2 subtasks OR >3 files"));
    }

    #[test]
    fn task_structure_contains_four_sections() {
        assert!(TASK_STRUCTURE.contains("CONTEXT:"));
        assert!(TASK_STRUCTURE.contains("GOAL:"));
        assert!(TASK_STRUCTURE.contains("CONSTRAINTS:"));
        assert!(TASK_STRUCTURE.contains("EXPECTED OUTPUT"));
    }

    #[test]
    fn canonical_example_is_syntactically_present() {
        assert!(CANONICAL_EXAMPLE.contains("Canonical example"));
        assert!(CANONICAL_EXAMPLE.contains("delegate_to_worker"));
        assert!(CANONICAL_EXAMPLE.contains("delegation_plan"));
    }

    // --- Workers-block rendering: build minimal fixtures from AgentConfig ---

    use spur_acp::config::{AgentConfig, Tier};
    use spur_acp::agents::defaults::apply_builtin_defaults;

    fn cfg_with_good_for(name: &str, good_for: Vec<String>) -> AgentConfig {
        // minimal_agent + override good_for; avoid building full orchestrator.
        let toml = format!(r#"name = "{}"
command = "x"
transport = "acp""#, name);
        let mut cfg: AgentConfig = toml::from_str(&toml).unwrap();
        cfg.delegation.good_for = good_for;
        cfg.delegation.description = Some(format!("{} test descriptor", name));
        cfg.delegation.tier = Some(Tier::Generalist);
        cfg
    }

    /// Render the workers block over an explicit agent slice, bypassing
    /// orchestrator self. Mirrors the logic of `render_workers_block`.
    fn render_workers_block_over(agents: &[AgentConfig]) -> String {
        let mut out = String::from("## Available worker agents\n\n");
        let mut any = false;
        for agent in agents {
            if agent.delegation.good_for.is_empty() { continue; }
            any = true;
            let tier = agent.delegation.tier
                .map(|t| match t {
                    Tier::Specialist => "specialist",
                    Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let desc = agent.delegation.description.as_deref().unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: medium)\n{}\n\n",
                agent.name, tier, desc,
            ));
        }
        if !any { out.push_str("(no worker-capable agents with descriptors configured)\n\n"); }
        out
    }

    #[test]
    fn workers_block_lists_agents_with_non_empty_good_for() {
        let agents = vec![
            cfg_with_good_for("claude-x", vec!["refactors".into()]),
            cfg_with_good_for("kiro-x", vec!["specs".into()]),
        ];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("claude-x"));
        assert!(block.contains("kiro-x"));
    }

    #[test]
    fn workers_block_excludes_empty_good_for_agents() {
        let agents = vec![
            cfg_with_good_for("has-good-for", vec!["real".into()]),
            cfg_with_good_for("bare", vec![]),  // will be excluded
        ];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("has-good-for"));
        assert!(!block.contains("bare"));
    }

    #[test]
    fn workers_block_says_none_when_all_excluded() {
        let agents = vec![cfg_with_good_for("bare", vec![])];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("(no worker-capable agents with descriptors configured)"));
    }

    #[test]
    fn workers_block_is_deterministic_for_same_input() {
        let agents = vec![cfg_with_good_for("a", vec!["x".into()])];
        let a = render_workers_block_over(&agents);
        let b = render_workers_block_over(&agents);
        assert_eq!(a, b);
    }
}
```

These tests exercise every block of the new prompt via pure functions without needing orchestrator construction. Full end-to-end prompt snapshots are covered by the manual TUI smoke check in the verification section.

- [ ] **Step 2: Run to verify tests fail**

Run: `cargo test -p spur-core --lib prompt_snapshot_tests`
Expected: FAIL — v1 prompt content not present.

- [ ] **Step 3: Refactor `build_brain_prompt` into helpers**

In `crates/spur-core/src/orchestrator.rs:1831`, REPLACE the body of `build_brain_prompt` with:

```rust
fn build_brain_prompt(&self, task: &str, issue: Option<&Issue>) -> String {
    if self.config.brain.delegation.framework == "v1" {
        self.build_brain_prompt_v1(task, issue)
    } else {
        self.build_brain_prompt_legacy(task, issue)
    }
}

fn build_brain_prompt_legacy(&self, task: &str, issue: Option<&Issue>) -> String {
    // Preserve original 5-line guidance for legacy flag users.
    let mut prompt = String::new();
    prompt.push_str(
        "You are coordinating a coding task. You have two kinds of tools:\n\
         \n\
         1. Your own tools (filesystem, bash, git) — use these to investigate and code directly.\n\
         2. SPUR delegation tools — use these to hand work to specialized worker agents.\n\
         \n\
         When to delegate vs do it yourself:\n\
         - Delegate when subtasks are INDEPENDENT and can run in parallel\n\
         - Delegate to match agent strengths\n\
         - Do it yourself for quick tasks or when you need tight iterative control\n\
         - Always review worker output before approving\n\n",
    );
    self.append_issue_and_task(&mut prompt, task, issue);
    prompt
}

fn build_brain_prompt_v1(&self, task: &str, issue: Option<&Issue>) -> String {
    let mut prompt = String::new();
    prompt.push_str(&self.render_header());
    prompt.push_str(&self.render_workers_block());
    prompt.push_str(DISPATCH_PROCEDURE);
    prompt.push_str(PLAN_REQUIREMENT);
    prompt.push_str(TASK_STRUCTURE);
    prompt.push_str(CANONICAL_EXAMPLE);
    self.append_issue_and_task(&mut prompt, task, issue);
    self.log_prompt_once(&prompt);
    prompt
}

fn append_issue_and_task(&self, prompt: &mut String, task: &str, issue: Option<&Issue>) {
    if let Some(issue) = issue {
        prompt.push_str(&format!(
            "## Issue #{}: {}\n\n{}\n\nLabels: {}\nStatus: {}\n\n",
            issue.id, issue.title, issue.body,
            issue.labels.join(", "), issue.status,
        ));
    }
    if let Some(ref append) = self.config.brain.prompt.append {
        prompt.push_str(&format!("## Project Context\n\n{}\n\n", append));
    }
    prompt.push_str(&format!("## Task\n\n{}\n", task));
}
```

- [ ] **Step 4: Add the block-renderer helpers and static constants**

Also in orchestrator.rs (near `build_brain_prompt`):

```rust
fn render_header(&self) -> String {
    "You are a brain coordinating a coding task. You have two kinds of tools:\n\
     \n\
     1. Your own tools (filesystem, bash, git) — for investigation and direct edits.\n\
     2. SPUR delegation tools (delegate_to_worker, delegate_parallel, list_available_workers) — for handing work to worker agents that run in isolated worktrees.\n\n".into()
}

fn render_workers_block(&self) -> String {
    let mut out = String::from("## Available worker agents\n\n");
    let any_listed = {
        let mut found = false;
        for agent in self.agents.worker_capable() {
            // Exclude agents whose good_for is empty (thin-synthesized
            // or inherit_defaults=false with empty vec).
            if agent.delegation.good_for.is_empty() {
                continue;
            }
            found = true;
            let tier = agent.delegation.tier
                .map(|t| match t {
                    spur_acp::config::Tier::Specialist => "specialist",
                    spur_acp::config::Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let cost = format!("{:?}", agent.cost_tier).to_lowercase();
            let desc = agent.delegation.description.as_deref().unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: {})\n{}\n\n",
                agent.name, tier, cost, desc,
            ));
        }
        found
    };
    if !any_listed {
        out.push_str("(no worker-capable agents with descriptors configured)\n\n");
    }
    out
}

fn log_prompt_once(&self, prompt: &str) {
    // Write the full assembled prompt to .spur/logs/brain-prompts/{session_id}.md
    // for forensic review. Best-effort; never blocks or errors the session.
    let dir = self.repo_root.join(".spur/logs/brain-prompts");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::debug!(error = %e, "could not create brain-prompts log dir");
        return;
    }
    let path = dir.join(format!("{}.md", self.current_session_id()));
    if let Err(e) = std::fs::write(&path, prompt) {
        tracing::debug!(error = %e, path = %path.display(), "could not write prompt log");
    }
    // LRU eviction: if dir size > 50 MB, delete oldest files first.
    enforce_log_cap(&dir, 50 * 1024 * 1024);
}

fn enforce_log_cap(dir: &std::path::Path, cap: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, _, s)| s).sum();
    if total <= cap { return; }
    files.sort_by_key(|(_, mtime, _)| *mtime);  // oldest first
    let mut to_free = total - cap;
    for (path, _, size) in files {
        if to_free == 0 { break; }
        let _ = std::fs::remove_file(&path);
        to_free = to_free.saturating_sub(size);
    }
}
```

And static constants:

```rust
const DISPATCH_PROCEDURE: &str = "\
## When to delegate vs. do it yourself

Do it yourself when:
  - The task is <15min of work.
  - You need tight iterative control (probe, edit, probe).
  - The task requires your accumulated session context.
  - No worker's good_for meaningfully matches.

Delegate when:
  - Subtasks are independent and parallelizable (use delegate_parallel).
  - A worker's good_for directly matches the task shape.
  - Scope (LoC, files, or duration) exceeds what you want to spend your
    context window on.
  - You need fresh context isolation.

Routing rule: prefer specialist tier when good_for matches exactly;
fall back to generalist tier otherwise. avoid_for is a SOFT signal —
you MAY override it with a stated rationale when no better agent exists.
Prefer lower-cost_tier agents for mechanical tasks; reserve higher-cost
agents for tasks requiring integration, judgment, or architectural
decisions.

Your <delegation_plan> replaces, does not supplement, other planning
artifacts you would emit FOR DELEGATION DECISIONS. Native planning
tools (Todo, plan mode, etc.) remain for intra-task work.

";

const PLAN_REQUIREMENT: &str = "\
## Required: delegation_plan parameter

Every delegate_to_worker and delegate_parallel call should include a
`delegation_plan` argument. Content scales with complexity:

For >=2 subtasks OR >3 files touched — pass the full shape:
  {
    \"candidates\":    [{\"agent\": \"...\", \"rationale\": \"...\"}, ...],
    \"decomposition\": [{\"subtask\": \"...\", \"parallelizable_with\": [\"...\"]}],
    \"chosen\":        \"agent-name-or-self-or-parallel\",
    \"rationale\":     \"Why this choice beats the alternatives. If
                      violating any agent's avoid_for, state why.\"
  }

For trivial single-step delegations — minimum shape:
  { \"chosen\": \"agent-name\", \"rationale\": \"short justification\" }

All fields are advisory; the orchestrator accepts the tool call even
with minimal or missing content. Your rationale is surfaced to the
review gate so reviewers can see what you decided and why.

If you have access to a sequential-thinking MCP tool, use it to
generate the candidates and decomposition before committing to the
delegate_* call.

";

const TASK_STRUCTURE: &str = "\
## Task prompt structure (what to send workers)

Structure the `task` field of delegate_to_worker as:

  CONTEXT: {scope, constraints from this session, relevant file paths
           and short excerpts — prefer inlining over passing paths via
           context_files so the worker doesn't spend turns re-reading}
  GOAL:    {one-sentence success criterion}
  CONSTRAINTS: {what the worker must NOT do}
  EXPECTED OUTPUT: {populated from the chosen agent's output_shape
                   when declared}

For agents with declared output_shape, EXPECTED OUTPUT must restate it.
For agents with declared input_expectations, CONTEXT must satisfy those
expectations before dispatch.

";

const CANONICAL_EXAMPLE: &str = "\
## Canonical example

Task: 'Refactor the auth module to use the new SessionId format across
all callers (4 files).'

Reasoning out loud (brain's narrative text):
  This is a multi-file refactor matching claude-code-acp's good_for.
  The changes are coupled (can't parallelize across callers).

delegate_to_worker(
  agent = \"claude-code-acp\",
  task = \"CONTEXT: Refactor the auth module. Affected files: src/auth/mod.rs, \
          src/auth/session.rs, src/api/handlers.rs, src/tests/auth.rs. \
          The new SessionId format is: [snippet]. \
          GOAL: All callers use the new format; all tests pass. \
          CONSTRAINTS: Don't touch src/api/v2/; don't modify the database schema. \
          EXPECTED OUTPUT: Unified diff + summary paragraph + test plan bullets.\",
  delegation_plan = {
    \"candidates\": [
      {\"agent\": \"claude-code-acp\", \"rationale\": \"multi-file refactor matches good_for\"},
      {\"agent\": \"codex\", \"rationale\": \"cheaper but avoid_for = multi-file coordination\"}
    ],
    \"decomposition\": [
      {\"subtask\": \"refactor auth + callers\", \"parallelizable_with\": []}
    ],
    \"chosen\": \"claude-code-acp\",
    \"rationale\": \"multi-file refactor + coupled callers; codex's avoid_for excludes it.\"
  }
)

";
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-core --lib prompt_snapshot_tests`
Expected: PASS (7/7).

- [ ] **Step 6: Verify full suite stays green**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): rewrite brain prompt with framework blocks behind feature flag"
```

---

## Phase 4 — Documentation + Example Config

### Task 17: Write `docs/spur/agent-config.md` delegation section

**Files:**
- Create or modify: `docs/spur/agent-config.md`

- [ ] **Step 1: Check if the file exists**

Run: `ls docs/spur/agent-config.md`

- [ ] **Step 2: Add the delegation section**

If the file exists, append a new section. If not, create it with just the delegation section (pre-existing sections can be added in later docs PRs).

```markdown
## Delegation descriptor — `[agents.entries.delegation]`

Each worker-capable agent can declare a delegation descriptor that tells the brain what the agent is good at, when to avoid it, and how to shape task prompts for it. Descriptors feed both the brain's system prompt (as a one-liner per agent) and the `list_available_workers` MCP tool (with the full shape).

All fields are optional. Built-in defaults ship for `claude-code-acp`, `kiro`, `codex`, and `gemini`; user values override per-field.

### Example

    [[agents.entries]]
    name = "my-claude"
    command = "claude"
    transport = "acp"

    [agents.entries.delegation]
    description = "Custom claude variant for our auth-flow work."
    tier        = "generalist"             # "specialist" | "generalist"
    good_for    = [
      "auth module refactors",
      "session-state migrations",
    ]
    avoid_for   = ["database schema work"]
    strengths   = ["long-context", "diff-shaped output"]
    limitations = ["no network"]
    input_expectations = "Provide session-state migration doc link in CONTEXT."
    output_shape       = "Unified diff + migration notes."
    inherit_defaults   = true              # default true; false = use user values verbatim

### Field reference

| Field | Role | Where used |
|---|---|---|
| `description` | One-line summary | Workers block in brain prompt, `list_available_workers` |
| `tier` | Specialist/generalist routing hint | Both |
| `good_for` | Positive task patterns | `list_available_workers`; brain routes on |
| `avoid_for` | Soft negative patterns | `list_available_workers`; brain may override with rationale |
| `strengths` | Free-form descriptors | Per-dispatch task prompt only |
| `limitations` | Known failure modes | Per-dispatch task prompt only |
| `input_expectations` | What the brain must supply in CONTEXT | Per-dispatch task prompt only |
| `output_shape` | Shape the worker produces | Brain's EXPECTED_OUTPUT section + `list_available_workers` |
| `inherit_defaults` | Merge with built-in default (default true) | Loader |

### Merge semantics

- **Per-field override:** users replace any subset without restating others.
- **Empty vec inherits (when `inherit_defaults = true`).** Setting `good_for = []` at v1 means "use the built-in default's `good_for`".
- **`inherit_defaults = false`:** user values are used verbatim, including empty vecs. Use when the built-in is genuinely wrong for your setup.

### Validation

spur warns at startup for:
- `good_for`/`avoid_for` entries over 80 chars
- Worker-capable agents with no `description`
- Worker-capable agents with empty `good_for`
- `good_for` entries mentioning a capability (e.g., "plan mode") that isn't declared in `[agents.entries.capabilities]`

Warnings don't block startup.

### Feature flag

The brain-delegation framework is gated by:

    [brain.delegation]
    framework = "v1"    # "v1" | "legacy"

Defaults: `"v1"` in dev builds (debug_assertions=true); `"legacy"` in release builds at v1 ship. Flag will flip to `"v1"` in release builds at v2, and be removed at v3.
```

- [ ] **Step 3: Commit**

```bash
git add docs/spur/agent-config.md
git commit -m "docs(spur): document [agents.entries.delegation] sub-table"
```

### Task 18: Add `.spur/config.toml.example` commented delegation block

**Files:**
- Modify: `.spur/config.toml.example` (may live at workspace root; verify)

- [ ] **Step 1: Verify path**

Run: `find . -name 'config.toml.example' -not -path './target/*' 2>/dev/null`

- [ ] **Step 2: Add commented delegation block to each agent entry**

For each `[[agents.entries]]` in the example file, add a commented-out `[agents.entries.delegation]` block:

```toml
# [agents.entries.delegation]
# # Uncomment to override the built-in descriptor.
# # Fields omitted here inherit from the built-in default.
# # description = "..."
# # good_for = ["..."]
# # avoid_for = ["..."]
# # output_shape = "..."
```

Plus a global block at the top showing the feature flag:

```toml
# [brain.delegation]
# # "v1" enables the rewritten brain prompt (workers block, dispatch
# # procedure, delegation_plan). "legacy" uses the pre-framework
# # 5-line prompt. Default is build-aware: dev builds use "v1";
# # release builds default to "legacy" at v1 ship. See
# # docs/spur/agent-config.md for the lifecycle.
# framework = "v1"
```

- [ ] **Step 3: Commit**

```bash
git add .spur/config.toml.example
git commit -m "docs(spur): add commented delegation blocks to config.toml.example"
```

### Task 19: Write `docs/spur/contributing-agent-defaults.md`

**Files:**
- Create: `docs/spur/contributing-agent-defaults.md`

- [ ] **Step 1: Create the doc**

```markdown
# Contributing Agent Defaults

Built-in delegation descriptors for known agents live in `crates/spur-acp/src/agents/defaults.toml`. Edit carefully — every change ships to all users on the next release.

## Guidelines

1. **Coarse and stable.** Descriptors should age over 6-12 months without breaking routing. Avoid version numbers, benchmark scores, or workflow-specific details.
2. **Imperative, short.** `good_for` entries are task patterns, not sentences. Target under 60 chars each. The lint fires at 80 but prefer tighter.
3. **Negative space matters.** `avoid_for` is as important as `good_for` for routing — it's often the tiebreaker when multiple agents match.
4. **Output shape is the brain's signal for task-prompt shaping.** Be specific about what the worker actually produces (diff? spec artifact? narrative?).

## Process for adding a new agent

1. Add a section to `defaults.toml`.
2. Add the agent name to `known_agents()` in `defaults.rs`.
3. Add the test case in `every_known_agent_resolves_to_a_descriptor`.
4. Update `docs/spur/agent-config.md` if this agent needs any special config notes.
5. Run `cargo test -p spur-acp --lib agents::`.

## Process for tuning an existing descriptor

1. Open an issue describing the observed misrouting.
2. Edit `defaults.toml`.
3. Add a regression test if the misrouting is reproducible with a fixture config.
4. Tag the release notes: this changes brain behavior.

## Maintenance item: capability keyword table

In `defaults.rs`, `CAPABILITY_KEYWORDS` maps capability tokens (`plan_mode`, `usage`, etc.) to trigger keywords the lint scans for in `good_for` strings. When a new token is added to `AgentConfig::capabilities`, add its trigger keywords to this table so the lint keeps working.
```

- [ ] **Step 2: Commit**

```bash
git add docs/spur/contributing-agent-defaults.md
git commit -m "docs(spur): add contributing-agent-defaults guide"
```

---

## Final Task: Full-suite verification + changelog

### Task 20: Run full test suite and changelog entry

- [ ] **Step 1: Run the complete test suite**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 2: Run a release build to verify no debug-only code slipped in**

Run: `cargo build --release`
Expected: succeeds; no warnings about debug_assertions misuse.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets`
Expected: no new warnings (or only warnings pre-existing on main).

- [ ] **Step 4: Add changelog entry**

Modify the project's changelog (likely `CHANGELOG.md` at repo root or `docs/CHANGELOG.md`):

```markdown
## Unreleased

### Added
- **Brain delegation framework.** Per-agent descriptors in `[agents.entries.delegation]` (routing signals), structured `delegation_plan` MCP tool parameter on `delegate_to_worker`/`delegate_parallel` (reasoning trace), rewritten brain prompt behind `[brain.delegation] framework` flag. Dev builds default to `"v1"`; release builds default to `"legacy"` at v1 ship. See `docs/superpowers/specs/2026-04-15-brain-delegation-framework-design.md`.
- Built-in delegation descriptors for `claude-code-acp`, `kiro`, `codex`, `gemini` in `crates/spur-acp/src/agents/defaults.toml`.
- `ReviewPayload` gains `delegation_plan` + `chosen_matches_dispatched` for reviewer visibility.
- `DelegationRequested` event carries `delegation_plan` for TUI timeline.
- `list_available_workers` MCP tool returns enriched descriptors (tier, description, good_for, avoid_for, output_shape, cost_tier).
- Config lint warnings for oversized `good_for` entries, worker-capable agents without descriptors, capability/descriptor mismatches.

### Changed
- `delegate_to_worker` / `delegate_parallel` / `list_available_workers` MCP tool descriptions expanded with framework guidance.
- `WorkerInfo` struct extended with routing fields (additive; backward-compatible).
```

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: changelog entry for brain delegation framework"
```

- [ ] **Step 6: Summary check**

Run: `git log --oneline --since="start of this plan"`
Expected: ~20 commits, one per task, all green.

Run: `cargo test --workspace --release`
Expected: release build's tests also pass (catches debug-only assertions that shouldn't exist).

---

## What's out of scope (explicit)

These are documented as non-goals in the spec; do NOT attempt during this plan:
- Learned routing / per-(task-pattern, agent) success tracking
- Cost-budgeted dispatch
- `expected_output` as a first-class MCP tool field
- Nested-delegation lineage
- Model-specific prompt flavors
- Hard procedural enforcement on `delegation_plan` content
- Blocking-semantics changes (streaming delegation)
- Async / non-blocking delegation
- Prometheus/metrics emission
- TUI UI for editing descriptors
- Runtime config hot-reload
- **Skill integration as soft upper layer.** Belongs to the follow-up spec `2026-04-16-spur-skill-integration-design.md`. This plan's artifacts are designed to accommodate that layer without rework.

## Rollout phases (for reference during execution)

- **Phase 1** = Tasks 1-6 (A + D). Ship first; no behavior change.
- **Phase 2** = Tasks 7-14 (C). Ship second; additive, tool descriptions + schema.
- **Phase 3** = Tasks 15-16 (B). Ship third, gated by feature flag.
- **Phase 4** = Tasks 17-20 (docs + verification). Land with Phase 3 or shortly after.

Each phase is independently revertable. Phase 3 is the observable switch; Phases 1-2 are safely dead code until Phase 3's flag defaults to `"v1"` in release builds (planned for v2 release).
