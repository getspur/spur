# Per-agent skip-permissions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a declarative `skip_permissions` lever to each `AgentConfig` so operators can run agents in bypass mode — equivalent to `claude --dangerously-skip-permissions` and `kiro-cli chat -a` — via one config flip per agent, covering three mechanisms: spawn-time CLI args, ACP session mode, and spur-side ACP auto-approve.

**Architecture:** Three new fields on `AgentConfig` (`skip_permissions`, `skip_permissions_args`, `skip_permissions_session_mode`). A `cfg.effective_args()` method in spur-acp composes spawn args. A `new_session_with_bypass(conn, cfg, cwd, mcp)` helper in spur-core wraps the new-session call with an optional `set_session_mode`. Existing `auto_approve` in `native.rs` gets a defensive improvement to prefer `AllowAlways`/`AllowOnce` kinds. The `create_connection` helper and the worker spawn match both consult `effective_args` and `skip_permissions` to decide whether to pass `permission_tx: None`.

**Tech Stack:** Rust, `agent-client-protocol` 0.10.x, `tokio`, `serde`, `toml`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-14-spur-acp-skip-permissions-design.md`

---

## Preconditions

- `cargo build --all-targets -p spur-acp -p spur-core` is green on `main` before starting.
- The probe binary `crates/spur-acp/examples/skip_perm_spike.rs` already exists (landed during brainstorming). Do not delete it.

## File Structure

| File | Role | Action |
|---|---|---|
| `crates/spur-acp/src/config.rs` | `AgentConfig` struct + field defaults | **Modify** — add 3 fields + `effective_args()` method |
| `crates/spur-acp/tests/skip_permissions_config.rs` | Serde + `effective_args` tests | **Create** |
| `crates/spur-acp/src/connection/native.rs` | `auto_approve` helper | **Modify** — defensive option selection |
| `crates/spur-acp/tests/auto_approve_defensive.rs` | Unit test for defensive `auto_approve` | **Create** |
| `crates/spur-core/src/orchestrator.rs` | `create_connection`, worker spawn, `init_agents` seed table, 5 `new_session` call sites | **Modify** |
| `crates/spur-core/src/skip_perm.rs` | `new_session_with_bypass` helper | **Create** |
| `crates/spur-core/src/lib.rs` | Module declaration | **Modify** — add `pub mod skip_perm;` |
| `crates/spur-core/tests/skip_perm_helper.rs` | Helper test against a mock `AgentConnection` | **Create** |

---

## Task 1: Three new fields on `AgentConfig`

**Files:**
- Modify: `crates/spur-acp/src/config.rs:8-37`
- Create: `crates/spur-acp/tests/skip_permissions_config.rs`

**Alternatives considered** (one-round evaluation):

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Three flat `#[serde(default)]` fields** | Matches existing `AgentConfig` ergonomics (flat); defaults trivial; TOML stays one level | Three fields instead of one | **Chosen** |
| B. Nested struct `skip_permissions: Option<SkipPermissionsConfig>` | Grouped logically | Extra `[agents.entries.skip_permissions]` nesting per agent; operator has to remember it | Rejected — ergonomics cost > logical grouping |
| C. Enum `skip_permissions: SkipMode { Off, Args(...), SessionMode(...), Both{...} }` | Type-prevents contradictions | TOML serde for tagged enums is verbose; breaks flat-field symmetry | Rejected — over-engineered for 2 actual states |

The flat fields form `Option<String>` + `Vec<String>` + `bool` are independently defaultable, so omitting any field yields today's behavior exactly.

### Steps

- [ ] **Step 1: Write the failing serde tests**

Create `crates/spur-acp/tests/skip_permissions_config.rs`:

```rust
//! Serde round-trip tests for the three `skip_permissions*` fields on
//! `AgentConfig`. Guards against silent regressions in default values and
//! field names.

use spur_acp::config::AgentConfig;

#[test]
fn skip_permissions_defaults_when_absent() {
    let toml_src = r#"
name = "kiro"
command = "kiro-cli"
transport = "acp"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.skip_permissions, false);
    assert!(cfg.skip_permissions_args.is_empty());
    assert!(cfg.skip_permissions_session_mode.is_none());
}

#[test]
fn skip_permissions_reads_explicit_values() {
    let toml_src = r#"
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
skip_permissions = true
skip_permissions_args = ["--trust-all-tools"]
skip_permissions_session_mode = "bypassPermissions"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(cfg.skip_permissions);
    assert_eq!(cfg.skip_permissions_args, vec!["--trust-all-tools".to_string()]);
    assert_eq!(
        cfg.skip_permissions_session_mode.as_deref(),
        Some("bypassPermissions")
    );
}

#[test]
fn skip_permissions_round_trips_through_toml() {
    let original = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        skip_permissions: true,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
    };
    let encoded = toml::to_string(&original).expect("serialize");
    let decoded: AgentConfig = toml::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded.skip_permissions, true);
    assert_eq!(decoded.skip_permissions_args, vec!["--trust-all-tools".to_string()]);
    assert!(decoded.skip_permissions_session_mode.is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p spur-acp --test skip_permissions_config
```

Expected: compile error (fields don't exist on `AgentConfig`).

- [ ] **Step 3: Add the three fields to `AgentConfig`**

Edit `crates/spur-acp/src/config.rs`, extending the struct `AgentConfig` (lines 8-37) with three fields **after** the existing `pub review: AgentReviewPolicy,`:

```rust
    /// Per-agent human-review policy.
    #[serde(default)]
    pub review: AgentReviewPolicy,

    /// When true, SPUR runs this agent in bypass mode. Activates (up to)
    /// three lanes, each conditional on this flag and its corresponding
    /// declared value:
    ///   - L1a: `skip_permissions_args` are appended to `args` at spawn.
    ///   - L1b: `skip_permissions_session_mode` is applied via
    ///          `set_session_mode` immediately after `new_session`.
    ///   - L2:  spur-acp passes `permission_tx = None` into the transport,
    ///          which auto-approves every ACP `request_permission` call.
    /// Default: false.
    #[serde(default)]
    pub skip_permissions: bool,

    /// Spawn-time CLI args appended to `args` when `skip_permissions = true`.
    /// Use for agents whose bypass is a command-line flag
    /// (e.g. `["--trust-all-tools"]` for kiro-cli,
    /// `["--dangerously-skip-permissions"]` for claude direct).
    /// Default: empty.
    #[serde(default)]
    pub skip_permissions_args: Vec<String>,

    /// ACP session mode to set via `set_session_mode` right after
    /// `new_session`, when `skip_permissions = true`. Use for agents that
    /// expose bypass as an ACP session mode (claude-code-acp →
    /// `"bypassPermissions"`). Non-fatal if the agent rejects the mode:
    /// L2 auto-approve still catches any permission calls.
    /// Default: None.
    #[serde(default)]
    pub skip_permissions_session_mode: Option<String>,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p spur-acp --test skip_permissions_config
```

Expected: 3 passed.

- [ ] **Step 5: Verify no existing tests regressed**

```bash
cargo test -p spur-acp
```

Expected: all existing tests pass (including `agent_review_policy`).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/config.rs crates/spur-acp/tests/skip_permissions_config.rs
git commit -m "feat(spur-acp): add skip_permissions fields to AgentConfig

Three new #[serde(default)] fields on AgentConfig: skip_permissions (bool),
skip_permissions_args (Vec<String>), skip_permissions_session_mode (Option<String>).
Defaults preserve today's behavior exactly. Serde round-trip tests guard
the schema.

Refs: docs/superpowers/specs/2026-04-14-spur-acp-skip-permissions-design.md"
```

---

## Task 2: `AgentConfig::effective_args()` method

**Files:**
- Modify: `crates/spur-acp/src/config.rs` (append `impl AgentConfig`)
- Modify: `crates/spur-acp/tests/skip_permissions_config.rs` (append test)

**Alternatives considered:**

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Method on `AgentConfig`** | Natural discovery (method-level autocomplete); one call site reads "cfg.effective_args()" | None significant | **Chosen** |
| B. Free function `effective_args(&AgentConfig) -> Vec<String>` | Also fine | Less discoverable; no reason to prefer free function | Rejected |
| C. Inline the concat at every caller | Zero abstraction | Duplicated in `create_connection` and the worker-spawn match; change once = change twice | Rejected |

The method allocates a fresh `Vec` on every call. For connection setup (rare event, seconds apart) this is noise-free. If it ever shows up in a hot path, change to `Cow<[String]>` — not worth preempting.

### Steps

- [ ] **Step 1: Write the failing test** (append to `crates/spur-acp/tests/skip_permissions_config.rs`)

```rust
#[test]
fn effective_args_returns_plain_args_when_disabled() {
    let cfg = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        skip_permissions: false,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
    };
    assert_eq!(cfg.effective_args(), vec!["acp".to_string()]);
}

#[test]
fn effective_args_appends_skip_args_when_enabled() {
    let cfg = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        skip_permissions: true,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
    };
    assert_eq!(
        cfg.effective_args(),
        vec!["acp".to_string(), "--trust-all-tools".to_string()]
    );
}

#[test]
fn effective_args_returns_plain_args_when_enabled_but_no_skip_args() {
    // claude-code-acp case: skip_permissions = true, bypass via session
    // mode not spawn args. effective_args should be unchanged.
    let cfg = AgentConfig {
        name: "claude-code-acp".into(),
        command: "npx".into(),
        args: vec!["--yes".into(), "@agentclientprotocol/claude-agent-acp@0.26.0".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        skip_permissions: true,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: Some("bypassPermissions".into()),
    };
    assert_eq!(
        cfg.effective_args(),
        vec![
            "--yes".to_string(),
            "@agentclientprotocol/claude-agent-acp@0.26.0".to_string()
        ]
    );
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p spur-acp --test skip_permissions_config
```

Expected: 3 new tests fail with "no method named `effective_args`".

- [ ] **Step 3: Add the method**

Append to `crates/spur-acp/src/config.rs` (directly after the `AgentConfig` struct definition, before `fn default_role()`):

```rust
impl AgentConfig {
    /// Args to pass when spawning this agent. Concatenates `args` with
    /// `skip_permissions_args` iff `skip_permissions` is true. This is the
    /// single source of truth used by `spur_core`'s spawn paths — do not
    /// read `self.args` directly when spawning.
    pub fn effective_args(&self) -> Vec<String> {
        let mut out = self.args.clone();
        if self.skip_permissions {
            out.extend(self.skip_permissions_args.iter().cloned());
        }
        out
    }
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p spur-acp --test skip_permissions_config
```

Expected: 6 passed (3 from Task 1 + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config.rs crates/spur-acp/tests/skip_permissions_config.rs
git commit -m "feat(spur-acp): AgentConfig::effective_args() composes spawn args

Single source of truth for final spawn args: args + skip_permissions_args
when skip_permissions is on. Callers in spur-core will use this instead
of cfg.args.clone() directly."
```

---

## Task 3: Defensive `auto_approve` in `native.rs`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:1419-1430` (function `auto_approve`)
- Create: `crates/spur-acp/tests/auto_approve_defensive.rs`

**Alternatives considered:**

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Prefer `AllowAlways`/`AllowOnce` kinds, fall back to `options.first()`** | Survives an agent that puts `RejectOnce` first; current behavior preserved for today's agents | None — it's strictly more correct than today | **Chosen** |
| B. Prefer `AllowAlways` only, fall back to first | Slightly more paranoid | Both Claude and Kiro emit `AllowOnce` options; picking them is fine | Rejected — over-restrictive |
| C. Keep blind `options.first()` | One fewer change | Empirically safe today but fragile for future agents | Rejected — costs nothing to harden |

`PermissionOptionKind` is `#[non_exhaustive]` (verified in SDK source at `agent-client-protocol-schema-0.11.4/src/client.rs:648-660`), so the match must include a `_` arm.

### Steps

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/auto_approve_defensive.rs`:

```rust
//! Defensive selection: `auto_approve` must prefer an allow-class option
//! even when it is not the first entry in the list.
//!
//! This test exercises the public surface only — it constructs a
//! `RequestPermissionRequest` and asserts the chosen `option_id`. The
//! function under test is not `pub`, so we drive it indirectly via the
//! `permission_tx = None` path in `NativeAcpConnection`. However, since
//! spawning a real agent just to test this helper is overkill, the helper
//! is made `pub(crate)` and exercised via a sibling integration test
//! that depends on `spur_acp` test-only surface.
//!
//! To keep the test simple and free of agent spawn, we add a thin
//! re-export `pub use connection::native::__test_auto_approve` behind
//! `cfg(test)` visibility… (see Step 3 below.)

use agent_client_protocol::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SelectedPermissionOutcome, SessionId, ToolCallId,
    ToolCallUpdateFields,
};

fn mk_request(options: Vec<PermissionOption>) -> RequestPermissionRequest {
    // Build a minimal RequestPermissionRequest. The only field we care
    // about is `options` — the rest are set to empty / synthetic values.
    let tool_call = agent_client_protocol::ToolCallUpdate {
        id: ToolCallId::new("t"),
        fields: ToolCallUpdateFields::default(),
        meta: None,
    };
    RequestPermissionRequest {
        options,
        session_id: SessionId::new("s".to_string()),
        tool_call,
        meta: None,
    }
}

#[test]
fn auto_approve_prefers_allow_always_when_not_first() {
    let opts = vec![
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_always"),
            "Allow Always",
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_once"),
            "Allow Once",
            PermissionOptionKind::AllowOnce,
        ),
    ];
    let req = mk_request(opts);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "allow_always");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn auto_approve_prefers_allow_once_when_no_allow_always() {
    let opts = vec![
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("allow_once"),
            "Allow Once",
            PermissionOptionKind::AllowOnce,
        ),
    ];
    let req = mk_request(opts);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "allow_once");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn auto_approve_falls_back_to_first_when_no_allow_kind() {
    // Degenerate case: only reject-class options. Preserve today's "pick
    // options.first()" behavior — caller sees an auto-reject, which is
    // still defensible as a fail-safe.
    let opts = vec![
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("reject_always"),
            "Reject Always",
            PermissionOptionKind::RejectAlways,
        ),
    ];
    let req = mk_request(opts);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "reject_once");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}

#[test]
fn auto_approve_empty_options_uses_allow_default() {
    let req = mk_request(vec![]);
    let resp = spur_acp::connection::native::__test_auto_approve(&req).expect("ok");
    match resp.outcome {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            assert_eq!(option_id.0.as_ref(), "allow");
        }
        other => panic!("expected Selected, got {other:?}"),
    }
}
```

> **Note on SDK types:** the exact field names/constructors for `ToolCallUpdate`, `PermissionOption`, and `RequestPermissionRequest` may need small adjustment when running this test — check against `/Users/<user>/.cargo/registry/src/.../agent-client-protocol-schema-0.11.4/src/client.rs` if a compile error occurs. Use `PermissionOption::new(option_id, name, kind)` (confirmed signature at `.../rpc_tests.rs:575`).

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p spur-acp --test auto_approve_defensive
```

Expected: compile error — `__test_auto_approve` not exposed.

- [ ] **Step 3: Modify `auto_approve` in `crates/spur-acp/src/connection/native.rs`**

Replace the existing function (lines 1419-1430) with:

```rust
// ─── Permission helpers ─────────────────────────────────────────────────────

fn auto_approve(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    // Prefer an explicitly allow-class option. Falls back to the first
    // option (historical behavior) if no allow-class is present, and to
    // a hardcoded "allow" id if the options list is empty.
    //
    // `PermissionOptionKind` is `#[non_exhaustive]`, so the match below
    // uses a `_` arm to stay forward-compatible with future variants.
    let option_id = args
        .options
        .iter()
        .find(|o| matches!(
            o.kind,
            agent_client_protocol::PermissionOptionKind::AllowAlways
                | agent_client_protocol::PermissionOptionKind::AllowOnce
        ))
        .map(|o| o.option_id.clone())
        .or_else(|| args.options.first().map(|o| o.option_id.clone()))
        .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("allow"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}

/// Test-only re-export of the private `auto_approve` helper so
/// integration tests under `tests/` can exercise its selection logic
/// without spawning an agent. Hidden from rustdoc; not a stability
/// surface.
#[doc(hidden)]
pub fn __test_auto_approve(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    auto_approve(args)
}

fn auto_deny(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let option_id = args
        .options
        .last()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("deny"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}
```

(No change needed in `connection/mod.rs` — `native` is already `pub mod native`, so `spur_acp::connection::native::__test_auto_approve` is directly reachable from integration tests as long as the function is `pub`.)

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p spur-acp --test auto_approve_defensive
```

Expected: 4 passed.

- [ ] **Step 5: Verify existing tests untouched**

```bash
cargo test -p spur-acp
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs \
        crates/spur-acp/src/connection/mod.rs \
        crates/spur-acp/tests/auto_approve_defensive.rs
git commit -m "feat(spur-acp): auto_approve prefers AllowAlways/AllowOnce kinds

Previously picked options.first() blindly. Both Claude and Kiro put an
allow-class option first today, so behavior is unchanged in practice,
but this hedges against a future agent whose first option is rejection.
Falls back to options.first(), then to a synthetic 'allow' id, mirroring
the original fallback chain."
```

---

## Task 4: Wire `effective_args` + skip_permissions into spawn paths

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:1400-1428` (`create_connection`)
- Modify: `crates/spur-core/src/orchestrator.rs:2299-2321` (worker spawn match)

**Alternatives considered:**

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Update both call sites in place** | Minimally invasive; each change is 2 lines | Mild duplication (two transport matches) | **Chosen** |
| B. Refactor worker spawn to call `create_connection` | Removes duplication | Out of scope; `create_connection` is a `&self` method, worker spawn is a free fn — signature change ripples | Rejected for this plan; may revisit as follow-up |
| C. Inline args concat at both sites without `effective_args`  | Fewer abstractions | Duplicates the concat logic; Task 2's method exists for a reason | Rejected |

The L2 override (`permission_tx = None` when `skip_permissions`) happens inside `create_connection`. The worker spawn path already passes `None` unconditionally (line 2304), so L2 is implicitly always on for workers — no change needed there.

### Steps

- [ ] **Step 1: Modify `create_connection`** (`crates/spur-core/src/orchestrator.rs`)

Replace the function body (lines 1400-1428) with:

```rust
    fn create_connection(
        &self,
        config: &spur_acp::config::AgentConfig,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Box<dyn AgentConnection> {
        // L1a: effective_args folds skip_permissions_args into the spawn
        // args when bypass is on.
        let args = config.effective_args();
        // L2: when bypass is on, short-circuit permission requests by
        // passing None, which activates spur-acp's auto_approve fast-path.
        // Only meaningful for transports that surface ACP permission
        // callbacks (ACP native); other transports ignore the value.
        let perm_tx = if config.skip_permissions { None } else { permission_tx };

        match config.transport {
            TransportKind::Acp => Box::new(NativeAcpConnection::new(
                config.name.clone(),
                config.command.clone(),
                args,
                perm_tx,
            )),
            TransportKind::Stdio => Box::new(StdioAdapter::new(
                config.name.clone(),
                config.command.clone(),
                args,
            )),
            TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
                config.name.clone(),
                config.command.clone(),
                args,
            )),
            TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
                config.name.clone(),
                config.command.clone(),
                args,
            )),
        }
    }
```

- [ ] **Step 2: Modify the worker-spawn transport match** (`crates/spur-core/src/orchestrator.rs`)

Replace lines 2299-2321 with:

```rust
    // 2. Spawn worker agent in worktree via AgentConnection.
    // Workers never receive a permission_tx, so L2 auto-approve is
    // implicitly always on for them. skip_permissions still has effect
    // via L1a (spawn args).
    let spawn_args = agent_config.effective_args();
    let mut connection: Box<dyn AgentConnection> = match agent_config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args,
            None,
        )),
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args,
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args,
        )),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args,
        )),
    };
```

Note that `spawn_args` is moved into one of the `::new` calls — we build it fresh per call because the match arms consume it. Since the value is small (a handful of strings), this is fine. If you prefer to avoid the per-arm clone, change to `.clone()` inside the arms and let the initial binding be reused — either shape passes tests.

*Actually the cleaner form:* build `spawn_args` once, clone per arm explicitly:

```rust
    let spawn_args = agent_config.effective_args();
    let mut connection: Box<dyn AgentConnection> = match agent_config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args.clone(),
            None,
        )),
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args.clone(),
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args.clone(),
        )),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
            agent_config.name.clone(),
            agent_config.command.clone(),
            spawn_args,
        )),
    };
```

The final arm drops the clone since the binding isn't used after.

- [ ] **Step 3: Build-check**

```bash
cargo build -p spur-core
```

Expected: clean build, no warnings related to `skip_permissions`.

- [ ] **Step 4: Run existing spur-core tests**

```bash
cargo test -p spur-core
```

Expected: all tests pass. (No new tests yet; this task is plumbing.)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): wire skip_permissions into connection spawn

create_connection() now calls cfg.effective_args() (L1a) and nulls out
permission_tx when skip_permissions is set (L2). Worker spawn match
picks up effective_args via the same method; workers already pass
permission_tx=None so L2 is implicit.

ACP session-mode lane (L1b) still pending — comes in Task 6."
```

---

## Task 5: `new_session_with_bypass` helper + mock test

**Files:**
- Create: `crates/spur-core/src/skip_perm.rs`
- Modify: `crates/spur-core/src/lib.rs` (add `pub mod skip_perm;`)
- Create: `crates/spur-core/tests/skip_perm_helper.rs`

**Alternatives considered:**

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Free function `new_session_with_bypass(conn, cfg, cwd, mcp)` in a new `skip_perm` module** | Testable in isolation against a mock `AgentConnection`; no `Orchestrator` coupling | Adds one small module | **Chosen** |
| B. Method on `Orchestrator` | Reuses existing type | Ties the helper to `&mut Orchestrator` state it doesn't need | Rejected |
| C. Extension trait on `AgentConnection` | Discoverable via `conn.new_session_with_bypass(...)` | `AgentConnection` lives in `spur-acp`; extension trait for `spur-core` behavior crosses crate boundaries awkwardly | Rejected |
| D. Inline the logic at each of the 5 call sites | Zero abstraction | 5 × ~8 lines of duplicated wiring + error handling | Rejected |

A fifth option would have been to push `new_session_with_bypass` into `spur-acp` itself, but that forces `AgentConfig`-level knowledge into the transport crate, violating "spur-acp stays agent-agnostic."

### Steps

- [ ] **Step 1: Write the failing test**

Create `crates/spur-core/tests/skip_perm_helper.rs`:

```rust
//! Tests for `new_session_with_bypass` — the helper that wraps
//! `AgentConnection::new_session` with an optional post-session
//! `set_session_mode` call driven by the agent's config.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, McpServer, NewSessionResponse,
    PromptRequest, SessionId, SessionNotification, SetSessionModeRequest, SetSessionModeResponse,
};
use async_trait::async_trait;
use futures::Stream;
use spur_acp::config::AgentConfig;
use spur_acp::connection::AgentConnection;
use spur_acp::types::{AgentHealth, AgentRole, CostTier, TransportKind};
use spur_core::skip_perm::new_session_with_bypass;

#[derive(Default)]
struct MockConn {
    /// Records every method call in order, as
    /// `("new_session", "<cwd>")` or `("set_session_mode", "<mode>")`.
    calls: Arc<Mutex<Vec<(String, String)>>>,
    /// If set, `set_session_mode` returns this error instead of Ok.
    fail_set_session_mode: bool,
}

#[async_trait]
impl AgentConnection for MockConn {
    async fn initialize(
        &mut self,
        _r: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        unimplemented!()
    }

    async fn new_session(
        &mut self,
        cwd: PathBuf,
        _mcp: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        self.calls
            .lock()
            .unwrap()
            .push(("new_session".into(), cwd.display().to_string()));
        Ok(NewSessionResponse {
            session_id: SessionId::new("mock-session".to_string()),
            modes: None,
            meta: None,
        })
    }

    async fn prompt(
        &mut self,
        _r: PromptRequest,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn Stream<Item = SessionNotification> + Send>>>
    {
        unimplemented!()
    }

    async fn cancel(&mut self, _s: &str) -> anyhow::Result<()> { unimplemented!() }
    async fn shutdown(&mut self) -> anyhow::Result<()> { Ok(()) }
    fn health(&self) -> AgentHealth { AgentHealth::Ready }

    async fn set_session_mode(
        &mut self,
        req: SetSessionModeRequest,
    ) -> anyhow::Result<SetSessionModeResponse> {
        self.calls
            .lock()
            .unwrap()
            .push(("set_session_mode".into(), req.mode_id.0.to_string()));
        if self.fail_set_session_mode {
            Err(anyhow::anyhow!("mock rejects mode"))
        } else {
            Ok(SetSessionModeResponse { meta: None })
        }
    }
}

fn cfg(
    skip: bool,
    mode: Option<&str>,
) -> AgentConfig {
    AgentConfig {
        name: "mock".into(),
        command: "mock".into(),
        args: vec![],
        transport: TransportKind::Acp,
        role: AgentRole::Both,
        capabilities: vec![],
        cost_tier: CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        skip_permissions: skip,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: mode.map(String::from),
    }
}

#[tokio::test]
async fn skips_set_session_mode_when_flag_off() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(false, Some("bypassPermissions"));
    new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![("new_session".into(), "/cwd".into())]);
}

#[tokio::test]
async fn skips_set_session_mode_when_mode_absent() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(true, None);
    new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![("new_session".into(), "/cwd".into())]);
}

#[tokio::test]
async fn calls_set_session_mode_when_bypass_and_mode_present() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(true, Some("bypassPermissions"));
    new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            ("new_session".into(), "/cwd".into()),
            ("set_session_mode".into(), "bypassPermissions".into()),
        ]
    );
}

#[tokio::test]
async fn set_session_mode_error_is_non_fatal() {
    let mut conn = MockConn {
        fail_set_session_mode: true,
        ..Default::default()
    };
    let cfg = cfg(true, Some("bypassPermissions"));
    // Must succeed even though set_session_mode fails — L2 auto-approve
    // is the fallback.
    let resp = new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok despite mode failure");
    assert_eq!(resp.session_id.0.as_ref(), "mock-session");
}
```

> **Note on `NewSessionResponse` / `SetSessionModeResponse`:** the exact field set (e.g. `modes`, `meta`) may differ slightly from the form shown. If compile fails, check `agent-client-protocol-schema-0.11.4/src/client.rs` for the canonical struct definitions and adjust literals.

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p spur-core --test skip_perm_helper
```

Expected: compile error — `skip_perm` module doesn't exist.

- [ ] **Step 3: Create the helper module**

Create `crates/spur-core/src/skip_perm.rs`:

```rust
//! `new_session_with_bypass` — wraps `AgentConnection::new_session` with
//! the optional `set_session_mode("bypassPermissions")` call that L1b of
//! the skip-permissions design requires.
//!
//! Keeps `AgentConfig`-aware logic out of `spur-acp` (which must stay
//! agent-agnostic). Callers in `orchestrator.rs` use this instead of
//! `conn.new_session(...)` whenever they have an `AgentConfig` in scope.

use std::path::PathBuf;

use agent_client_protocol::{McpServer, NewSessionResponse, SetSessionModeRequest};
use spur_acp::config::AgentConfig;
use spur_acp::connection::AgentConnection;

/// Call `conn.new_session(cwd, mcp)`. If `cfg.skip_permissions` is true
/// and `cfg.skip_permissions_session_mode` is set, then additionally
/// invoke `conn.set_session_mode(...)` with that mode on the freshly
/// created session id.
///
/// Errors from `new_session` propagate. Errors from `set_session_mode`
/// are logged at `warn!` and swallowed — L2 auto-approve is the
/// fallback, so a non-honoring agent still bypasses permissions.
pub async fn new_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> anyhow::Result<NewSessionResponse> {
    let resp = conn.new_session(cwd, mcp_servers).await?;

    if cfg.skip_permissions {
        if let Some(mode) = cfg.skip_permissions_session_mode.as_deref() {
            let req = SetSessionModeRequest::new(resp.session_id.clone(), mode);
            if let Err(e) = conn.set_session_mode(req).await {
                tracing::warn!(
                    agent = %cfg.name,
                    mode = %mode,
                    error = %e,
                    "skip_permissions: set_session_mode failed; \
                     relying on L2 auto-approve"
                );
            } else {
                tracing::debug!(
                    agent = %cfg.name,
                    mode = %mode,
                    "skip_permissions: set_session_mode applied"
                );
            }
        }
    }

    Ok(resp)
}
```

- [ ] **Step 4: Register the module**

In `crates/spur-core/src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod skip_perm;
```

- [ ] **Step 5: Run to verify pass**

```bash
cargo test -p spur-core --test skip_perm_helper
```

Expected: 4 passed.

- [ ] **Step 6: Run full spur-core suite**

```bash
cargo test -p spur-core
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/skip_perm.rs \
        crates/spur-core/src/lib.rs \
        crates/spur-core/tests/skip_perm_helper.rs
git commit -m "feat(spur-core): add new_session_with_bypass helper for L1b

Wraps AgentConnection::new_session with an optional
set_session_mode(bypassPermissions) call driven by AgentConfig. Errors
from set_session_mode are logged and swallowed because L2 auto-approve
is the belt-and-suspenders fallback. Tested against a mock
AgentConnection covering all four flag/mode combinations."
```

---

## Task 6: Wire the helper into all 5 `new_session` call sites

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:277-280` (`run_adhoc`)
- Modify: `crates/spur-core/src/orchestrator.rs:828-831` (`exec_direct`)
- Modify: `crates/spur-core/src/orchestrator.rs:1068-1071` (`create_brain_session`)
- Modify: `crates/spur-core/src/orchestrator.rs:1196-1199` (`load_brain_session` fallback)
- Modify: `crates/spur-core/src/orchestrator.rs:2344-2346` (`run_one_worker_attempt`)

**Alternatives considered:**

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Replace all 5 call sites with the helper, looking up config where needed** | Every site covered; single source of truth for L1b | `create_brain_session` / `load_brain_session` don't have `AgentConfig` in scope; we must look it up via `self.registry.get(&brain_name)` | **Chosen** |
| B. Only wire sites that already have `AgentConfig` in scope; leave the others | Smaller diff | Brain-session paths (1068, 1196) silently skip L1b — bypasses don't work for brain agents that use session-mode bypass (i.e. claude-code-acp, the preferred Claude transport). Unacceptable. | Rejected |
| C. Thread `&AgentConfig` through `create_brain_session` / `load_brain_session` signatures | Keeps the helper's interface minimal | Signature changes ripple to callers; introduces more churn than the registry lookup | Rejected — registry lookup is cheap and local |

At sites 1068 and 1196, `brain_name` is already in scope. We look up via `self.registry.get(&brain_name).ok_or_else(...)?.clone()` → gives us the `AgentConfig` without plumbing. `cloned()` is fine; `AgentConfig` is `Clone` and the hot path is once per brain session.

### Steps

- [ ] **Step 1: Modify site 1 — `run_adhoc` (around line 277)**

The function has `brain_config` in scope (line 254: `self.create_connection(&brain_config, None)`). Replace:

```rust
        let session_response = connection
            .new_session(self.repo_root.clone(), mcp_servers)
            .await
            .context("Failed to create brain session")?;
```

with:

```rust
        let session_response = spur_core::skip_perm::new_session_with_bypass(
            &mut *connection,
            &brain_config,
            self.repo_root.clone(),
            mcp_servers,
        )
        .await
        .context("Failed to create brain session")?;
```

Note `connection` at this site is a `Box<dyn AgentConnection>`. `&mut *connection` dereferences the `Box` (via `DerefMut`) and reborrows as `&mut dyn AgentConnection`, matching the helper signature. If the local is already `&mut dyn AgentConnection`, write `connection` without the reborrow.

- [ ] **Step 2: Modify site 2 — `exec_direct` (around line 828)**

Scope: `agent_config` is available (line 820). Replace:

```rust
        let session_response = connection
            .new_session(self.repo_root.clone(), vec![])
            .await
            .context("Failed to create agent session")?;
```

with:

```rust
        let session_response = spur_core::skip_perm::new_session_with_bypass(
            &mut *connection,
            &agent_config,
            self.repo_root.clone(),
            vec![],
        )
        .await
        .context("Failed to create agent session")?;
```

- [ ] **Step 3: Modify site 3 — `create_brain_session` (around line 1068)**

Scope: only `brain_name: String` is available, not `AgentConfig`. Add a registry lookup before the call. Replace:

```rust
        let session_response = connection
            .new_session(self.repo_root.clone(), mcp_servers)
            .await
            .context("Failed to create brain session")?;
```

with:

```rust
        let brain_cfg = self
            .registry
            .get(&brain_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(
                "brain agent '{}' not in registry during create_brain_session",
                brain_name
            ))?;
        let session_response = spur_core::skip_perm::new_session_with_bypass(
            &mut *connection,
            &brain_cfg,
            self.repo_root.clone(),
            mcp_servers,
        )
        .await
        .context("Failed to create brain session")?;
```

- [ ] **Step 4: Modify site 4 — `load_brain_session` fallback (around line 1196)**

This site is inside an `Err(e) => { … }` match arm that falls back to `new_session` after `load_session` fails. Same registry-lookup pattern applies.

Replace the arm body:

```rust
            Err(e) => {
                warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
                let session_response = connection
                    .new_session(self.repo_root.clone(), mcp_servers)
                    .await
                    .context("Failed to create fallback session after load_session failure")?;
                (session_response.session_id.to_string(), None, false)
            }
```

with:

```rust
            Err(e) => {
                warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
                let brain_cfg = self
                    .registry
                    .get(&brain_name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!(
                        "brain agent '{}' not in registry during load_brain_session fallback",
                        brain_name
                    ))?;
                let session_response = spur_core::skip_perm::new_session_with_bypass(
                    &mut *connection,
                    &brain_cfg,
                    self.repo_root.clone(),
                    mcp_servers,
                )
                .await
                .context("Failed to create fallback session after load_session failure")?;
                (session_response.session_id.to_string(), None, false)
            }
```

- [ ] **Step 5: Modify site 5 — `run_one_worker_attempt` (around line 2344)**

This is a free function, not an `Orchestrator` method, but `agent_config` is in scope (used at line 2300). Replace:

```rust
    let session_response = match connection
        .new_session(worktree_info.path.clone(), vec![])
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = connection.shutdown().await;
            let _ = worktrees.remove_worktree(&worker_session).await;
```

with:

```rust
    let session_response = match spur_core::skip_perm::new_session_with_bypass(
        &mut *connection,
        agent_config,
        worktree_info.path.clone(),
        vec![],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = connection.shutdown().await;
            let _ = worktrees.remove_worktree(&worker_session).await;
```

> **Check on `agent_config`:** this free function takes `agent_config: &AgentConfig` as a parameter. Confirm the binding is by-reference; if it's owned, remove the leading `&` above.

- [ ] **Step 6: Build-check**

```bash
cargo build -p spur-core
```

Expected: clean. If any site has a different `connection` variable type (e.g. `&mut Box<dyn …>` vs owned `Box<dyn …>`), adjust the reborrow accordingly — `&mut *connection` works for an owned `Box`, while an already-borrowed `&mut Box<dyn …>` needs `&mut **connection`.

- [ ] **Step 7: Run the full suite**

```bash
cargo test -p spur-acp -p spur-core
```

Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): wire new_session_with_bypass into all 5 sites

Brain (create + load fallback + adhoc), direct exec, and worker spawn
all now go through the helper so L1b (set_session_mode) fires when the
agent declares a bypass session mode.

Brain-session sites look up AgentConfig via self.registry.get(&brain_name)
since the surrounding functions only carry brain_name: String."
```

---

## Task 7: Extend `init_agents` seed table with bypass defaults

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:873-923` (`init_agents`)

**Alternatives considered:**

| Shape | Pros | Cons | Verdict |
|---|---|---|---|
| A. Extend the existing 4-tuple to a 6-tuple | Smallest diff | Tuples with 6 mixed-type positional fields are hard to read | Rejected |
| **B. Replace tuples with a `SeedAgent` struct** | Named fields; extensible | ~10 more LoC than tuples | **Chosen** |
| C. Load seeds from an embedded TOML via `include_str!` | Text-editable seeds | `serde(default)` on `AgentConfig` means TOML defaults just work; loading from a file introduces a runtime parse that can fail | Rejected — premature |

`skip_permissions` itself stays `false` in the seed — the operator opts in by flipping the flag in their config. What the seed provides is the **mechanism declaration** per agent (`skip_permissions_args`, `skip_permissions_session_mode`), so flipping the flag later in the operator's TOML Just Works.

### Steps

- [ ] **Step 1: Replace the seed table**

In `crates/spur-core/src/orchestrator.rs`, replace lines 873-923 (the `init_agents` body) with:

```rust
    /// Initialize: scan $PATH for known agents, populate registry.
    pub async fn init_agents(&mut self) -> Result<Vec<String>> {
        struct SeedAgent {
            name: &'static str,
            command: &'static str,
            args: Vec<&'static str>,
            transport: TransportKind,
            /// L1a mechanism: CLI args appended when skip_permissions is on.
            /// Empty means this agent's bypass is not a CLI flag.
            skip_permissions_args: Vec<&'static str>,
            /// L1b mechanism: ACP session mode set after new_session when
            /// skip_permissions is on. None means this agent's bypass is
            /// not an ACP session mode.
            skip_permissions_session_mode: Option<&'static str>,
        }

        let known_agents = [
            SeedAgent {
                name: "kiro",
                command: "kiro-cli",
                args: vec!["acp"],
                transport: TransportKind::Acp,
                skip_permissions_args: vec!["--trust-all-tools"],
                skip_permissions_session_mode: None,
            },
            SeedAgent {
                name: "claude-code",
                command: "claude",
                args: vec![
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--permission-mode",
                    "acceptEdits",
                ],
                transport: TransportKind::StreamJson,
                skip_permissions_args: vec!["--dangerously-skip-permissions"],
                skip_permissions_session_mode: None,
            },
            SeedAgent {
                name: "claude-code-acp",
                command: "npx",
                args: vec!["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"],
                transport: TransportKind::Acp,
                // The npx wrapper takes no CLI flags — bypass is via
                // ACP session mode (verified in acp-agent.js source
                // and probed live, see design doc).
                skip_permissions_args: vec![],
                skip_permissions_session_mode: Some("bypassPermissions"),
            },
            SeedAgent {
                name: "codex",
                command: "codex",
                args: vec!["--acp"],
                transport: TransportKind::Acp,
                // Unknown bypass mechanism; operator can set
                // skip_permissions=true and get L2-only (every ACP
                // permission request silently auto-approved).
                skip_permissions_args: vec![],
                skip_permissions_session_mode: None,
            },
            SeedAgent {
                name: "gemini",
                command: "gemini",
                args: vec![],
                transport: TransportKind::CliWrap,
                skip_permissions_args: vec![],
                skip_permissions_session_mode: None,
            },
        ];

        let mut found = Vec::new();

        for seed in &known_agents {
            let which = tokio::process::Command::new("which")
                .arg(seed.command)
                .output()
                .await;

            if let Ok(output) = which {
                if output.status.success() {
                    let config = spur_acp::config::AgentConfig {
                        name: seed.name.to_string(),
                        command: seed.command.to_string(),
                        args: seed.args.iter().map(|s| s.to_string()).collect(),
                        transport: seed.transport,
                        role: AgentRole::Both,
                        capabilities: vec![],
                        cost_tier: CostTier::Medium,
                        rate_limit_window: None,
                        review: Default::default(),
                        skip_permissions: false,
                        skip_permissions_args: seed
                            .skip_permissions_args
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        skip_permissions_session_mode: seed
                            .skip_permissions_session_mode
                            .map(String::from),
                    };
                    self.registry.register(config);
                    found.push(seed.name.to_string());
                    info!(agent = %seed.name, command = %seed.command, "Found agent");
                }
            }
        }

        Ok(found)
    }
```

Note: the new `claude-code-acp` seed entry is ADDITIVE — it wasn't in the original table. This matches the production `.spur/config.toml` which already has it as a manually declared entry; the seed now self-populates the same fields so freshly initialized repos get both transports out of the box.

- [ ] **Step 2: Build-check**

```bash
cargo build -p spur-core
```

Expected: clean.

- [ ] **Step 3: Run tests**

```bash
cargo test -p spur-core
```

Expected: all pass. (No new tests for the seed table — it's configuration data and is exercised by any integration test that calls `init_agents`.)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): seed table declares skip_permissions mechanism per agent

Replaces the 4-tuple with a SeedAgent struct. Adds per-agent bypass
declarations:
  - kiro                → --trust-all-tools (L1a)
  - claude-code         → --dangerously-skip-permissions (L1a)
  - claude-code-acp     → session mode bypassPermissions (L1b)
  - codex, gemini       → unknown; operator gets L2-only if they opt in

skip_permissions itself stays false in the seed; operator flips it per
agent in ~/.spur/config.toml. Adds claude-code-acp to auto-discovery so
fresh repos get both Claude transports."
```

---

## Task 8: End-to-end smoke + probe rerun

**Files:** none modified.

**Alternatives considered:**

| Approach | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Rerun the existing probe matrix + build/test/clippy** | Reuses the simulation infrastructure already validated in brainstorming; directly confirms runtime behavior | Requires live agents (`kiro-cli`, `npx`) on PATH | **Chosen** |
| B. Write a new in-crate integration test that spawns claude-code-acp | Automatable in CI | Slow (≥ 15 s per run), flaky (npx cold start), adds test-time external deps | Rejected — the probe binary already does this in a diagnostic context, no need to duplicate |
| C. Skip smoke; trust unit tests | Fastest | Unit tests don't cover `create_connection → effective_args → NativeAcpConnection` end-to-end | Rejected |

### Steps

- [ ] **Step 1: Full build**

```bash
cargo build --all-targets -p spur-acp -p spur-core
```

Expected: clean.

- [ ] **Step 2: Full test suite**

```bash
cargo test -p spur-acp -p spur-core
```

Expected: all pass.

- [ ] **Step 3: Clippy**

```bash
cargo clippy -p spur-acp -p spur-core --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 4: Rerun the four-row probe matrix**

```bash
rm -rf /tmp/spur-probe-out && mkdir /tmp/spur-probe-out

target/debug/examples/skip_perm_spike claude-code-acp off /tmp/spur-skip-probe-cwd \
    > /tmp/spur-probe-out/1.stdout 2> /tmp/spur-probe-out/1.stderr
target/debug/examples/skip_perm_spike claude-code-acp session /tmp/spur-skip-probe-cwd \
    > /tmp/spur-probe-out/2.stdout 2> /tmp/spur-probe-out/2.stderr
target/debug/examples/skip_perm_spike kiro off /tmp/spur-skip-probe-cwd \
    > /tmp/spur-probe-out/3.stdout 2> /tmp/spur-probe-out/3.stderr
target/debug/examples/skip_perm_spike kiro args /tmp/spur-skip-probe-cwd \
    > /tmp/spur-probe-out/4.stdout 2> /tmp/spur-probe-out/4.stderr

cat /tmp/spur-probe-out/*.stdout
```

Expected rows (from design doc's validated matrix):

```
agent=claude-code-acp mode=Off     permission_calls=1 notifs=7 took=<~20000>ms outcome=ok
agent=claude-code-acp mode=Session permission_calls=0 notifs=7 took=<~17000>ms outcome=ok
agent=kiro            mode=Off     permission_calls=1 notifs=3 took=<~28000>ms outcome=ok
agent=kiro            mode=Args    permission_calls=0 notifs=3 took=<~18000>ms outcome=ok
```

The probe hits `NativeAcpConnection` directly (not the `create_connection` path), so the matrix validates that the underlying mechanisms still work after any incidental changes. Permission counts MUST match the design doc. Durations are illustrative only.

- [ ] **Step 5: Manual smoke via `spur run`** (optional but recommended)

With a local `.spur/config.toml` that sets `skip_permissions = true` on one agent (e.g. kiro), invoke `spur run` on a task that triggers a tool call and confirm:

- No permission prompts appear in the TUI.
- The tool executes successfully.
- `.spur/logs/<agent>-*.log` contains no `request_permission` entries for that session.

If the UI does prompt, check:
1. Is the config TOML being loaded? (try `spur status` or similar)
2. Is `skip_permissions` spelled correctly in the TOML?
3. Does `tracing::debug!` in `new_session_with_bypass` log "skip_permissions: set_session_mode applied" (or the failure warn!)?

This step is optional in the sense that the unit + probe coverage is already decisive; it's the final sanity check for a human-in-the-loop run.

- [ ] **Step 6: Commit any incidental fixes (if needed)**

If steps 1-4 revealed any issue, commit the fix with a descriptive message. Otherwise, no commit.

---

## Self-Review

**Spec coverage (checking each spec section):**

- Spec §1 (Config schema — three new fields) → Task 1. ✓
- Spec §2 L1a (spawn args) → Task 2 (`effective_args`) + Task 4 (wire into `create_connection` and worker spawn). ✓
- Spec §2 L1b (session mode) → Task 5 (`new_session_with_bypass`) + Task 6 (wire 5 call sites). ✓
- Spec §2 L2 (auto-approve + defensive) → Task 3 (defensive `auto_approve`) + Task 4 (nulling `permission_tx`). ✓
- Spec §3 (touchpoints: seed table, spawn paths) → Task 4, Task 6, Task 7. ✓
- Spec §4 Out of scope → honored: no CLI override, no CLAUDE_CONFIG_DIR, L2-only fallback for unknown agents. ✓
- Spec "Testing plan" → Serde tests (Task 1), `auto_approve` defensive (Task 3), helper mock tests (Task 5), manual probe matrix (Task 8). ✓

**Placeholder scan:** No "TBD"/"TODO"/"add validation"/"similar to Task N" patterns. Each step has complete code or exact commands.

**Type consistency:**
- `effective_args()` signature: `&self -> Vec<String>`. Used identically in Task 4 and referenced in Task 2's test. ✓
- `new_session_with_bypass` signature: `&mut dyn AgentConnection, &AgentConfig, PathBuf, Vec<McpServer>`. Used identically in Task 5's mock test and all 5 Task 6 sites. ✓
- `SeedAgent` fields: `name, command, args, transport, skip_permissions_args, skip_permissions_session_mode`. Same names used in the consuming loop. ✓
- `skip_permissions_session_mode: Option<String>` in `AgentConfig`; seeds use `Option<&'static str>` and convert via `.map(String::from)`. ✓

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-14-spur-acp-skip-permissions.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
