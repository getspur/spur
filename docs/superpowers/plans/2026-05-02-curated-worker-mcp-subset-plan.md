# Curated Worker MCP Subset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a dedicated `WorkerMcpServer` exposing a curated 8-tool subset to workers via per-delegation HMAC bearer token, lazily started per `BrainSession`, opt-in via `enable_worker_mcp=true`.

**Architecture:** Workers connect to a new HTTP/JSON-RPC server (separate from brain `McpCallbackServer`). Token-based auth via middleware extracts `WorkerCallContext` from URL token; freestanding handler functions in a new `handlers.rs` module are shared between both servers via a unified `Result<Value, McpHandlerError>` contract. Lifecycle is bound to active-delegation count, not request idle time. Audit trail: synchronous on writes, aggregated per-delegation on reads with detached-task flusher (no async Drop).

**Tech Stack:** Rust, tokio, dashmap, hmac+sha2, base64ct, serde_json, anyhow, thiserror, tokio_util::sync::CancellationToken.

**Spec:** `docs/superpowers/specs/2026-05-02-curated-worker-mcp-subset-design.md` (commit `98d80fe0`).

**Beads:** bd-14cq (parent brainstorm); follow-up to bd-wjvs.

---

## File Structure

| Path | Purpose | Status |
|---|---|---|
| `crates/spur-mcp/src/token.rs` | HMAC-SHA256 token gen/validation, `WorkerToken` payload struct | NEW |
| `crates/spur-mcp/src/handlers.rs` | Freestanding tool handlers, `McpHandlerError`, `WorkerCallContext` | NEW |
| `crates/spur-mcp/src/worker_server.rs` | `WorkerMcpServer` struct, HTTP listener, token middleware, dispatch, audit, lifecycle | NEW |
| `crates/spur-mcp/src/plan/audit_sentinel.rs` | Add `WorkerMcp` variant + `WorkerMcpSubkind` enum | MODIFY |
| `crates/spur-mcp/src/tools.rs` | Add `worker_tools_list()` returning the 8-tool subset | MODIFY |
| `crates/spur-mcp/src/tool_schemas.rs` | Add `enable_worker_mcp`, `enable_worker_progress` fields | MODIFY |
| `crates/spur-mcp/src/server.rs` | Refactor 5 method handlers to thin wrappers over `handlers.rs` freestanding fns | MODIFY |
| `crates/spur-mcp/src/lib.rs` | Register new modules | MODIFY |
| `crates/spur-acp/src/domain/delegation.rs` | Add `DelegationDispatchError::WorkerMcpUnavailable` variant | MODIFY |
| `crates/spur-acp/src/domain/events.rs` | Add `SpurEventBody::WorkerMcpDelegationSummary` variant | MODIFY |
| `crates/spur-core/src/orchestrator.rs` | `worker_mcp_servers` field, lazy start, conditional `mcp_servers` injection at `:6571-6577`, `flush_delegation` exit hook, `retire_brain_session` shutdown | MODIFY |
| `crates/spur-mcp/tests/worker_mcp_*.rs` | Integration + security tests (one file per test category) | NEW |
| `.github/workflows/spur-ci.yml` (or equivalent) | SDK matrix gated job | MODIFY |

---

## Phase 1 — Type Foundations

These tasks add new enum variants and types that everything else depends on. They have no dependencies on each other and could be parallelized but are ordered for clarity.

### Task 1: AuditSentinelKind::WorkerMcp variant

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs:71` (enum), `:200` (encode_comment helper)
- Test: `crates/spur-mcp/src/plan/audit_sentinel.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
// In #[cfg(test)] mod tests inside audit_sentinel.rs
#[test]
fn worker_mcp_sentinel_round_trip() {
    let kind = AuditSentinelKind::WorkerMcp {
        delegation_id: "abc-123".into(),
        subkind: WorkerMcpSubkind::Call,
        tool_name: Some("update_issue".into()),
        target_issue_id: Some("bd-1234".into()),
        error: None,
    };
    let encoded = kind.encode_comment();
    let decoded = AuditSentinelKind::decode_from_comment(&encoded)
        .expect("must round-trip");
    assert_eq!(decoded, kind);

    // kebab-case in serialized form
    assert!(encoded.contains("\"call\""));
    assert!(!encoded.contains("\"Call\""));
}

#[test]
fn worker_mcp_subkind_kebab_case_serialization() {
    let cases = [
        (WorkerMcpSubkind::Call, "call"),
        (WorkerMcpSubkind::AuthDenied, "auth-denied"),
        (WorkerMcpSubkind::ScopeViolation, "scope-violation"),
        (WorkerMcpSubkind::UpstreamFailure, "upstream-failure"),
        (WorkerMcpSubkind::FlushFailed, "flush-failed"),
        (WorkerMcpSubkind::PmDegraded, "pm-degraded"),
    ];
    for (sub, expected) in cases {
        let json = serde_json::to_string(&sub).unwrap();
        assert_eq!(json, format!("\"{expected}\""));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --lib plan::audit_sentinel::tests::worker_mcp -- --nocapture
```
Expected: FAIL with `unresolved variant or struct WorkerMcp` and `unresolved type WorkerMcpSubkind`.

- [ ] **Step 3: Add the variant and subkind enum**

```rust
// crates/spur-mcp/src/plan/audit_sentinel.rs
// Add to AuditSentinelKind enum (around line 71, after existing variants):
WorkerMcp {
    delegation_id: String,
    subkind: WorkerMcpSubkind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_issue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
},

// Add new enum (place after AuditSentinelKind):
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerMcpSubkind {
    Call,
    AuthDenied,
    ScopeViolation,
    UpstreamFailure,
    FlushFailed,
    PmDegraded,
}
```

If `kind_str` (the helper that returns kebab-case tag for each variant) exists around line 174, add `Self::WorkerMcp { .. } => "worker-mcp"`.

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --lib plan::audit_sentinel::tests::worker_mcp
```
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/audit_sentinel.rs
git commit -m "spur-mcp: add WorkerMcp audit sentinel variant + subkind enum"
```

---

### Task 2: SpurEventBody::WorkerMcpDelegationSummary variant

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Test: same file, inline tests

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn worker_mcp_delegation_summary_round_trip() {
    let event = SpurEventBody::WorkerMcpDelegationSummary {
        delegation_id: "abc-123".into(),
        calls_total: 42,
        calls_by_tool: vec![
            ("get_issue".into(), 30),
            ("update_issue".into(), 12),
        ].into_iter().collect(),
        p99_latency_ms: 87,
        errors: 2,
    };
    let json = serde_json::to_string(&event).unwrap();
    let decoded: SpurEventBody = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, event);
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-acp --lib domain::events::tests::worker_mcp_delegation_summary
```
Expected: FAIL with `no variant named WorkerMcpDelegationSummary`.

- [ ] **Step 3: Add the variant**

```rust
// crates/spur-acp/src/domain/events.rs
// Add to SpurEventBody enum (find #[serde(tag = "type", rename_all = "snake_case")] enum SpurEventBody { ... }):
WorkerMcpDelegationSummary {
    delegation_id: String,
    calls_total: u64,
    calls_by_tool: std::collections::BTreeMap<String, u64>,
    p99_latency_ms: u64,
    errors: u64,
},
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-acp --lib domain::events::tests::worker_mcp_delegation_summary
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "spur-acp: add WorkerMcpDelegationSummary event variant"
```

---

### Task 3: Schema fields on DelegateToWorkerInput / DelegateParallelTaskInput

**Files:**
- Modify: `crates/spur-mcp/src/tool_schemas.rs:13` (`DelegateToWorkerInput`), `:33` (`DelegateParallelTaskInput`)
- Test: same file, inline tests

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn delegate_input_default_enable_flags_false() {
    let json = r#"{"agent": "kimi", "task": "do work"}"#;
    let input: DelegateToWorkerInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.enable_worker_mcp, None);
    assert_eq!(input.enable_worker_progress, None);
}

#[test]
fn delegate_input_explicit_enable_worker_mcp() {
    let json = r#"{"agent": "kimi", "task": "do work", "enable_worker_mcp": true}"#;
    let input: DelegateToWorkerInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.enable_worker_mcp, Some(true));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --lib tool_schemas::tests::delegate_input
```
Expected: FAIL with `no field enable_worker_mcp`.

- [ ] **Step 3: Add the fields**

```rust
// crates/spur-mcp/src/tool_schemas.rs
pub struct DelegateToWorkerInput {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_mcp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_progress: Option<bool>,
}

// Same fields added to DelegateParallelTaskInput.
```

Update the JSON schema string returned by the tool definitions in `tools.rs` (`delegate_to_worker_def`, `delegate_parallel_def`) to document the new optional booleans.

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --lib tool_schemas::tests::delegate_input
```
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/tool_schemas.rs crates/spur-mcp/src/tools.rs
git commit -m "spur-mcp: add enable_worker_mcp/enable_worker_progress schema fields (default false)"
```

---

### Task 4: DelegationDispatchError::WorkerMcpUnavailable variant

**Files:**
- Modify: wherever `DelegationDispatchError` is defined. Per spec §9.2 it's currently in `crates/spur-mcp/src/server.rs:235`. If subsequent refactor moved it, update accordingly. Use `grep -n 'enum DelegationDispatchError' crates/`.
- Test: same file, inline tests

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn worker_mcp_unavailable_jsonrpc_code_is_minus_32002() {
    let err = DelegationDispatchError::WorkerMcpUnavailable {
        reason: "port exhausted".into(),
    };
    assert_eq!(err.json_rpc_code(), -32002);
    assert!(err.to_string().contains("port exhausted"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --lib server::tests::worker_mcp_unavailable
```
Expected: FAIL.

- [ ] **Step 3: Add variant + code mapping**

```rust
// In the DelegationDispatchError enum:
#[error("worker MCP server unavailable: {reason}")]
WorkerMcpUnavailable { reason: String },

// In the json_rpc_code (or equivalent) method:
Self::WorkerMcpUnavailable { .. } => -32002,
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --lib server::tests::worker_mcp_unavailable
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add <modified file>
git commit -m "spur-mcp: add DelegationDispatchError::WorkerMcpUnavailable (-32002)"
```

---

### Task 5: token.rs — HMAC token gen + validation

**Files:**
- Create: `crates/spur-mcp/src/token.rs`
- Modify: `crates/spur-mcp/src/lib.rs` (add `pub mod token;`)
- Modify: `crates/spur-mcp/Cargo.toml` (add `hmac = "0.12"`, `sha2 = "0.10"`, `base64ct = { version = "1", features = ["alloc"] }` if not present)
- Test: `crates/spur-mcp/src/token.rs` (inline tests)

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/src/token.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn key() -> [u8; 32] { [42; 32] }

    fn now_unix() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn round_trip_valid_token() {
        let payload = WorkerTokenPayload {
            d: "abc-123".into(),
            b: "session-99".into(),
            e: now_unix() + 3600,
        };
        let token = encode_token(&key(), &payload).unwrap();
        let decoded = validate_token(&key(), &token, /*skew_tolerance=*/30).unwrap();
        assert_eq!(decoded.d, "abc-123");
        assert_eq!(decoded.b, "session-99");
    }

    #[test]
    fn rejects_expired_token() {
        let payload = WorkerTokenPayload {
            d: "abc-123".into(),
            b: "session-99".into(),
            e: now_unix() - 100,  // 100s in the past
        };
        let token = encode_token(&key(), &payload).unwrap();
        let err = validate_token(&key(), &token, 30).unwrap_err();
        assert!(matches!(err, TokenError::Expired));
    }

    #[test]
    fn accepts_token_within_skew_tolerance() {
        let payload = WorkerTokenPayload {
            d: "abc".into(),
            b: "s".into(),
            e: now_unix() - 10,  // 10s past
        };
        let token = encode_token(&key(), &payload).unwrap();
        validate_token(&key(), &token, 30).expect("within tolerance");
    }

    #[test]
    fn rejects_bad_signature() {
        let payload = WorkerTokenPayload {
            d: "abc".into(),
            b: "s".into(),
            e: now_unix() + 60,
        };
        let token = encode_token(&key(), &payload).unwrap();
        let other_key = [99u8; 32];
        let err = validate_token(&other_key, &token, 30).unwrap_err();
        assert!(matches!(err, TokenError::BadSignature));
    }

    #[test]
    fn rejects_malformed_token() {
        let err = validate_token(&key(), "not.a.token", 30).unwrap_err();
        assert!(matches!(err, TokenError::Malformed));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --lib token::tests
```
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement token.rs**

```rust
// crates/spur-mcp/src/token.rs
use base64ct::{Base64UrlUnpadded, Encoding};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerTokenPayload {
    pub d: String,  // delegation_id
    pub b: String,  // brain_session_id
    pub e: u64,     // expiry (unix seconds)
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token is malformed")]
    Malformed,
    #[error("token signature does not verify")]
    BadSignature,
    #[error("token expired")]
    Expired,
}

pub fn encode_token(key: &[u8; 32], payload: &WorkerTokenPayload) -> anyhow::Result<String> {
    let payload_json = serde_json::to_vec(payload)?;
    let payload_b64 = Base64UrlUnpadded::encode_string(&payload_json);
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| anyhow::anyhow!(e))?;
    mac.update(payload_b64.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = Base64UrlUnpadded::encode_string(&sig);
    Ok(format!("{payload_b64}.{sig_b64}"))
}

pub fn validate_token(
    key: &[u8; 32],
    token: &str,
    skew_tolerance_secs: u64,
) -> Result<WorkerTokenPayload, TokenError> {
    let (payload_b64, sig_b64) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let sig = Base64UrlUnpadded::decode_vec(sig_b64).map_err(|_| TokenError::Malformed)?;

    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| TokenError::BadSignature)?;
    mac.update(payload_b64.as_bytes());
    mac.verify_slice(&sig).map_err(|_| TokenError::BadSignature)?;

    let payload_bytes = Base64UrlUnpadded::decode_vec(payload_b64).map_err(|_| TokenError::Malformed)?;
    let payload: WorkerTokenPayload = serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::Malformed)?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    if payload.e + skew_tolerance_secs < now {
        return Err(TokenError::Expired);
    }
    Ok(payload)
}
```

Add `pub mod token;` to `crates/spur-mcp/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --lib token::tests
```
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/token.rs crates/spur-mcp/src/lib.rs crates/spur-mcp/Cargo.toml
git commit -m "spur-mcp: add HMAC-SHA256 worker token encode/validate"
```

---

## Phase 2 — Handlers Module (Refactor)

This phase extracts the existing handler bodies in `server.rs` into freestanding async functions in a new `handlers.rs` module, unifying their return types via `McpHandlerError`. The existing brain `McpCallbackServer` becomes a thin wrapper.

### Task 6: handlers.rs scaffold — McpHandlerError + WorkerCallContext

**Files:**
- Create: `crates/spur-mcp/src/handlers.rs`
- Modify: `crates/spur-mcp/src/lib.rs` (add `pub mod handlers;`)
- Test: inline

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_call_context_construction() {
        let ctx = WorkerCallContext {
            delegation_id: "d-1".into(),
            brain_session_id: "b-1".into(),
        };
        assert_eq!(ctx.delegation_id, "d-1");
    }

    #[test]
    fn handler_error_to_jsonrpc_response() {
        let err = McpHandlerError::InvalidParams("missing field 'id'".into());
        let resp = err.to_jsonrpc_response(serde_json::json!(7));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("-32602"));
        assert!(s.contains("missing field"));
    }

    #[test]
    fn upstream_pm_failure_maps_to_internal_error() {
        let err = McpHandlerError::UpstreamPm("503 service unavailable".into());
        let resp = err.to_jsonrpc_response(serde_json::json!(1));
        assert!(serde_json::to_string(&resp).unwrap().contains("-32603"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --lib handlers::tests
```
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement scaffold**

```rust
// crates/spur-mcp/src/handlers.rs
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct WorkerCallContext {
    pub delegation_id: String,
    pub brain_session_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum McpHandlerError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("upstream PM failure: {0}")]
    UpstreamPm(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpHandlerError {
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => -32602,
            Self::NotFound(_) => -32004,
            Self::Unauthorized(_) => -32001,
            Self::UpstreamPm(_) | Self::Internal(_) => -32603,
        }
    }

    pub fn to_jsonrpc_response(&self, id: Value) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": self.json_rpc_code(),
                "message": self.to_string(),
            }
        })
    }
}
```

Add `pub mod handlers;` to `crates/spur-mcp/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --lib handlers::tests
```
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/handlers.rs crates/spur-mcp/src/lib.rs
git commit -m "spur-mcp: add handlers module scaffold (McpHandlerError + WorkerCallContext)"
```

---

### Task 7: Extract get_issue handler

**Files:**
- Modify: `crates/spur-mcp/src/handlers.rs` (add freestanding `get_issue`)
- Modify: `crates/spur-mcp/src/server.rs` (replace method body with delegation to freestanding fn)
- Test: `crates/spur-mcp/tests/handlers_get_issue.rs` (NEW integration test file)

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/tests/handlers_get_issue.rs
use spur_mcp::handlers::{get_issue, WorkerCallContext, McpHandlerError};
use spur_acp::pm::{PmService, /* test fixture imports */};
use std::sync::Arc;

#[tokio::test]
async fn get_issue_returns_issue_via_pm_service() {
    let pm = test_pm_service_with_issue("bd-100", "test issue body");
    let ctx = WorkerCallContext {
        delegation_id: "d-1".into(),
        brain_session_id: "b-1".into(),
    };
    let args = serde_json::json!({"id": "bd-100"});
    let result = get_issue(&pm, &ctx, args).await.unwrap();
    assert_eq!(result["id"], "bd-100");
    assert_eq!(result["body"], "test issue body");
}

#[tokio::test]
async fn get_issue_missing_id_param_returns_invalid_params() {
    let pm = test_pm_service_empty();
    let ctx = WorkerCallContext {
        delegation_id: "d-1".into(),
        brain_session_id: "b-1".into(),
    };
    let args = serde_json::json!({});  // no id
    let err = get_issue(&pm, &ctx, args).await.unwrap_err();
    assert!(matches!(err, McpHandlerError::InvalidParams(_)));
}

// test_pm_service_with_issue / test_pm_service_empty implemented here as fixtures
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --test handlers_get_issue
```
Expected: FAIL — `get_issue` not found in handlers module.

- [ ] **Step 3: Add freestanding get_issue and rewire server.rs**

```rust
// crates/spur-mcp/src/handlers.rs (append)
use spur_acp::pm::PmService;

pub async fn get_issue(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let id = args.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| McpHandlerError::InvalidParams("missing 'id'".into()))?;
    let issue = pm.get_issue(id).await
        .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?;
    serde_json::to_value(issue).map_err(|e| McpHandlerError::Internal(e.to_string()))
}
```

In `crates/spur-mcp/src/server.rs`, find the existing `handle_get_issue` method (~line in the 2400-2500 range; locate via grep) and rewrite it to delegate:

```rust
async fn handle_get_issue(&self, params: serde_json::Value, id: serde_json::Value) -> JsonRpcResponse {
    let ctx = WorkerCallContext {
        delegation_id: String::new(),  // brain calls have no delegation_id
        brain_session_id: self.brain_session_id.0.clone(),
    };
    match crate::handlers::get_issue(&self.pm_service, &ctx, params).await {
        Ok(value) => JsonRpcResponse::success(id, value),
        Err(err) => JsonRpcResponse::from_value(err.to_jsonrpc_response(id)),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --test handlers_get_issue
cargo build -p spur-mcp  # must still compile
```
Expected: PASS + clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/handlers.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/handlers_get_issue.rs
git commit -m "spur-mcp: extract get_issue into freestanding handler"
```

---

### Task 8: Extract update_issue handler

**Files:**
- Modify: `crates/spur-mcp/src/handlers.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Test: `crates/spur-mcp/tests/handlers_update_issue.rs` (NEW)

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/tests/handlers_update_issue.rs
use spur_mcp::handlers::{update_issue, WorkerCallContext, McpHandlerError};

#[tokio::test]
async fn update_issue_writes_comment_via_pm() {
    let pm = test_pm_service_empty();
    let ctx = WorkerCallContext { delegation_id: "d".into(), brain_session_id: "b".into() };
    let args = serde_json::json!({
        "id": "bd-100",
        "comment": "hello from worker"
    });
    let result = update_issue(&pm, &ctx, args).await.unwrap();
    assert_eq!(result["ok"], true);

    let issue = pm.get_issue("bd-100").await.unwrap();
    assert!(issue.body_or_comments_contain("hello from worker"));
}

#[tokio::test]
async fn update_issue_missing_id_invalid_params() {
    let pm = test_pm_service_empty();
    let ctx = WorkerCallContext { delegation_id: "d".into(), brain_session_id: "b".into() };
    let err = update_issue(&pm, &ctx, serde_json::json!({"comment": "x"})).await.unwrap_err();
    assert!(matches!(err, McpHandlerError::InvalidParams(_)));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --test handlers_update_issue
```
Expected: FAIL.

- [ ] **Step 3: Add freestanding update_issue and rewire server.rs**

```rust
// crates/spur-mcp/src/handlers.rs (append)
pub async fn update_issue(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let id = args.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| McpHandlerError::InvalidParams("missing 'id'".into()))?;
    let comment = args.get("comment").and_then(|v| v.as_str()).map(String::from);
    let add_labels = args.get("add_labels").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();
    let remove_labels = args.get("remove_labels").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();
    let status = args.get("status").and_then(|v| v.as_str()).map(String::from);

    pm.update_issue(id, comment.as_deref(), &add_labels, &remove_labels, status.as_deref())
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?;
    Ok(serde_json::json!({"ok": true}))
}
```

Rewire `server.rs::handle_update_issue` to delegate (mirror the pattern from Task 7).

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --test handlers_update_issue && cargo build -p spur-mcp
```
Expected: PASS + clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/handlers.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/handlers_update_issue.rs
git commit -m "spur-mcp: extract update_issue into freestanding handler"
```

---

### Task 9: Extract get_plan_status handler

**Files:**
- Modify: `crates/spur-mcp/src/handlers.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Test: `crates/spur-mcp/tests/handlers_get_plan_status.rs` (NEW)

Follow the exact same TDD pattern as Tasks 7-8. The freestanding signature:

```rust
pub async fn get_plan_status(
    plan_resolver: &dyn PlanResolver,  // trait abstracting load_or_project_plan
    reconciler_outcomes: &ReconcilerOutcomes,  // existing type
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError>;
```

You will need to introduce a `PlanResolver` trait (in `handlers.rs` or a sibling module) that abstracts `McpCallbackServer::load_or_project_plan`. Implement the trait for `McpCallbackServer` so the existing `handle_get_plan_status` keeps working as a wrapper.

- [ ] **Step 1: Write the failing test** (assert plan-status JSON shape via mock `PlanResolver`)
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement the freestanding handler + trait + impl**
- [ ] **Step 4: Run, pass + build**
- [ ] **Step 5: Commit** as `spur-mcp: extract get_plan_status + introduce PlanResolver trait`

---

### Task 10: Extract fetch_outcome_artifact handler

**Files:**
- Modify: `crates/spur-mcp/src/handlers.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Test: `crates/spur-mcp/tests/handlers_fetch_outcome_artifact.rs` (NEW)

Same TDD pattern. Critical detail: this handler uses `OutcomeKey` which already scopes by `brain_session_id` (`server.rs:2997-3001`). The freestanding version must construct the key from `ctx.brain_session_id`, NOT from any parameter the caller supplied — this is what enforces cross-session isolation.

```rust
pub async fn fetch_outcome_artifact(
    materializer: &Materializer,
    outcome_store: &OutcomeStore,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError>;
```

Test must include: a worker with `ctx.brain_session_id = "session-A"` cannot fetch an artifact stored under `brain_session_id = "session-B"` — handler returns `McpHandlerError::Unauthorized`.

- [ ] **Step 1: Write the failing test (include cross-session denial assertion)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: extract fetch_outcome_artifact (preserves brain_session scoping)`

---

### Task 11: Extract get_task_diff handler (heaviest)

**Files:**
- Modify: `crates/spur-mcp/src/handlers.rs`
- Modify: `crates/spur-mcp/src/server.rs:4313-4524` (the existing 210-line method)
- Test: `crates/spur-mcp/tests/handlers_get_task_diff.rs` (NEW)

This is the biggest extraction. The current method touches `self.pm_service`, `self.feature_gate`, `self.repo_root`, and `self.load_or_project_plan()`. Pass each as a parameter; reuse the `PlanResolver` trait introduced in Task 9.

```rust
pub async fn get_task_diff(
    pm: &PmService,
    feature_gate: &FeatureGate,
    repo_root: &std::path::Path,
    plan_resolver: &dyn PlanResolver,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError>;
```

Move the body verbatim from `server.rs:4313-4524`, replacing every `self.X` access with the corresponding parameter. The existing brain method becomes a one-liner that delegates.

- [ ] **Step 1: Write the failing test (basic shape assertion + feature-gate denial path)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement (be patient — this step has the largest code block in the plan; mechanically transcribe the body)**
- [ ] **Step 4: Run, pass + `cargo build -p spur-mcp` clean**
- [ ] **Step 5: Commit** as `spur-mcp: extract get_task_diff into freestanding handler (largest refactor)`

---

### Task 12: Re-align report_signal handler

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:1198` (already freestanding) — change return type to `Result<serde_json::Value, McpHandlerError>` instead of `anyhow::Result<Value>`. Move into `crates/spur-mcp/src/handlers.rs`.
- Modify: server-side dispatch call sites to handle the new return type.
- Test: existing `report_signal` tests should still pass; add one that asserts `WorkerCallContext` is passed through.

```rust
// New signature in handlers.rs
pub async fn report_signal(
    pm: &PmService,
    feature_gate: &FeatureGate,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError>;
```

- [ ] **Step 1: Write the failing test (asserts context is threaded)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Move + re-align signature + adjust callers**
- [ ] **Step 4: Run all `cargo test -p spur-mcp report_signal`, pass**
- [ ] **Step 5: Commit** as `spur-mcp: align report_signal to McpHandlerError contract`

---

### Task 13: Implement report_progress handler (NEW)

**Files:**
- Modify: `crates/spur-mcp/src/handlers.rs`
- Test: `crates/spur-mcp/tests/handlers_report_progress.rs` (NEW)

This handler does NOT exist yet in the codebase. It is a new fire-and-forget handler that emits a `SpurEventBody::WorkerReportProgress` event via `FunnelHandle` (you may need to add this event variant; if it overlaps with an existing one, reuse it).

```rust
pub async fn report_progress(
    funnel: &FunnelHandle,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let message = args.get("message").and_then(|v| v.as_str())
        .ok_or_else(|| McpHandlerError::InvalidParams("missing 'message'".into()))?
        .to_string();
    let percent = args.get("percent").and_then(|v| v.as_f64());

    funnel.emit(SpurEventBody::WorkerReportProgress {
        delegation_id: ctx.delegation_id.clone(),
        message,
        percent,
    });
    Ok(serde_json::json!({"ok": true}))
}
```

If `WorkerReportProgress` doesn't exist in `SpurEventBody`, add it as a sub-step of this task (mirror Task 2).

- [ ] **Step 1: Write the failing test (emits one event with correct payload)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement (add event variant if needed)**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: add report_progress handler (fire-and-forget event)`

---

## Phase 3 — Worker Tools List

### Task 14: tools.rs::worker_tools_list()

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs:816` (add new fn next to existing `tools_list()`)
- Test: same file

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn worker_tools_list_contains_exactly_8_curated_tools() {
    let tools = worker_tools_list();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names.len(), 8);
    let expected = [
        "get_issue", "list_issues", "get_task_diff", "get_plan_status",
        "fetch_outcome_artifact", "update_issue", "report_signal", "report_progress",
    ];
    for tool in &expected {
        assert!(names.contains(tool), "missing tool: {tool}");
    }
}

#[test]
fn worker_tools_list_excludes_brain_only_tools() {
    let tools = worker_tools_list();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let forbidden = [
        "delegate_to_worker", "delegate_parallel", "delegate_async",
        "submit_plan", "merge_plan", "execute_epic", "review_task",
        "create_pr", "create_issue", "add_dependency", "cancel_delegation",
        "wait_delegation", "check_delegation_status", "list_available_workers",
        "get_session_cost", "get_reconciler_status",
    ];
    for tool in &forbidden {
        assert!(!names.contains(tool), "leaked brain-only tool: {tool}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p spur-mcp --lib tools::tests::worker_tools_list
```
Expected: FAIL.

- [ ] **Step 3: Implement worker_tools_list**

```rust
// In tools.rs, near the existing tools_list()
pub fn worker_tools_list() -> Vec<ToolDefinition> {
    vec![
        get_issue_def(),
        list_issues_def(),
        get_task_diff_def(),
        get_plan_status_def(),
        fetch_outcome_artifact_def(),
        update_issue_def(),
        report_signal_def(),
        report_progress_def(),  // add this if not present
    ]
}
```

If `report_progress_def()` doesn't exist, add it next to `report_signal_def()` with an appropriate JSON schema (`message: string`, `percent: number | null`).

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p spur-mcp --lib tools::tests::worker_tools_list
```
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/tools.rs
git commit -m "spur-mcp: add worker_tools_list() with curated 8-tool subset"
```

---

## Phase 4 — Worker Server (worker_server.rs)

This is the largest phase. The server is built up incrementally: skeleton → token middleware → JSON-RPC dispatcher → audit → lifecycle. Each task adds one capability with its own test.

### Task 15: WorkerMcpServer skeleton + start()

**Files:**
- Create: `crates/spur-mcp/src/worker_server.rs`
- Modify: `crates/spur-mcp/src/lib.rs`
- Test: `crates/spur-mcp/tests/worker_server_lifecycle.rs` (NEW)

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/tests/worker_server_lifecycle.rs
use spur_mcp::worker_server::WorkerMcpServer;
use std::sync::Arc;

#[tokio::test]
async fn start_binds_listener_and_returns_url() {
    let pm = test_pm_service_empty();
    let feature_gate = test_feature_gate();
    let funnel = test_funnel();
    let server = WorkerMcpServer::start(
        "session-1".into(), pm, feature_gate, funnel,
    ).await.expect("start must succeed");
    let url = server.url();
    assert!(url.starts_with("http://127.0.0.1:"));
    assert!(url.contains("/mcp"));

    // Reachable
    let resp = reqwest::Client::new().get(&url).send().await.unwrap();
    // Without a valid token we expect 401 or 400, but not connection refused
    assert!(resp.status().is_client_error() || resp.status() == 200);

    server.shutdown().await;
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement skeleton**

```rust
// crates/spur-mcp/src/worker_server.rs
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub struct WorkerMcpServer {
    addr: SocketAddr,
    hmac_key: [u8; 32],
    brain_session_id: String,
    shutdown: CancellationToken,
    accept_loop_handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    // pm_service, feature_gate, funnel held internally; details in later tasks
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("failed to bind listener: {0}")]
    Bind(std::io::Error),
}

impl WorkerMcpServer {
    pub async fn start(
        brain_session_id: String,
        pm_service: Arc<dyn /* trait */>,
        feature_gate: Arc<FeatureGate>,
        funnel: FunnelHandle,
    ) -> Result<Arc<Self>, BindError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(BindError::Bind)?;
        let addr = listener.local_addr().map_err(BindError::Bind)?;
        let mut hmac_key = [0u8; 32];
        rand::Rng::fill(&mut rand::thread_rng(), &mut hmac_key);
        let shutdown = CancellationToken::new();
        let server = Arc::new(Self {
            addr, hmac_key, brain_session_id,
            shutdown: shutdown.clone(),
            accept_loop_handle: tokio::sync::Mutex::new(None),
        });
        let server_for_loop = Arc::clone(&server);
        let handle = tokio::spawn(async move {
            server_for_loop.accept_loop(listener).await;
        });
        *server.accept_loop_handle.lock().await = Some(handle);
        Ok(server)
    }

    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub async fn shutdown(self: Arc<Self>) {
        self.shutdown.cancel();
        if let Some(handle) = self.accept_loop_handle.lock().await.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
    }

    async fn accept_loop(self: Arc<Self>, listener: TcpListener) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                accept = listener.accept() => match accept {
                    Ok((stream, _)) => {
                        let server = Arc::clone(&self);
                        tokio::spawn(async move {
                            server.handle_connection(stream).await;
                        });
                    }
                    Err(_) => continue,
                }
            }
        }
    }

    async fn handle_connection(self: Arc<Self>, _stream: tokio::net::TcpStream) {
        // Stub. Token middleware + JSON-RPC dispatch added in later tasks.
        // For now, write a minimal HTTP response so the test's reachability check passes.
    }
}
```

Add `pub mod worker_server;` to lib.rs. Add `rand = "0.8"` to Cargo.toml if not present.

- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: scaffold WorkerMcpServer (start/url/shutdown)`

---

### Task 16: Token middleware

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: extend `crates/spur-mcp/tests/worker_server_lifecycle.rs`

Add `issue_token(delegation_id, ttl)` method and HTTP middleware that extracts `?token=` from request URL, validates via `crate::token::validate_token`, and constructs `WorkerCallContext`. Reject with HTTP 401 + JSON-RPC `-32600` if invalid. Audit `WorkerMcp{subkind: AuthDenied}` (the audit emit infrastructure is added later but stub it as a `tracing::warn!` for now and update in Task 19).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn invalid_token_returns_401() {
    let server = test_server().await;
    let url = format!("{}?token=garbage", server.url());
    let resp = reqwest::Client::new().post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 401);
    server.shutdown().await;
}

#[tokio::test]
async fn valid_token_passes_middleware() {
    let server = test_server().await;
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let url = format!("{}?token={}", server.url(), token);
    let resp = reqwest::Client::new().post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send().await.unwrap();
    // Even though tools/list isn't implemented yet, middleware should pass it through
    assert_ne!(resp.status(), 401);
    server.shutdown().await;
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement middleware (extend `handle_connection` to parse the HTTP request line, extract query string, validate token; respond 401 on failure; otherwise build `WorkerCallContext` and pass to a `dispatch()` stub)**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: add token middleware to WorkerMcpServer (HTTP 401 on invalid)`

---

### Task 17: JSON-RPC dispatcher with tools/list and tools/call

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: `crates/spur-mcp/tests/worker_server_dispatch.rs` (NEW)

Implement the JSON-RPC body parser and dispatcher. `tools/list` returns `worker_tools_list()` JSON. `tools/call` routes by name to the freestanding handlers from `handlers.rs` using the middleware-extracted `WorkerCallContext`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn tools_list_returns_8_tools() {
    let server = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/list", serde_json::json!({})).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    server.shutdown().await;
}

#[tokio::test]
async fn tools_call_get_issue_routes_to_handler() {
    let server = test_server_with_issue("bd-100").await;
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/call",
        serde_json::json!({"name": "get_issue", "arguments": {"id": "bd-100"}})).await;
    assert_eq!(body["result"]["id"], "bd-100");
    server.shutdown().await;
}

#[tokio::test]
async fn tools_call_unknown_tool_returns_method_not_found() {
    let server = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/call",
        serde_json::json!({"name": "delegate_to_worker", "arguments": {}})).await;
    assert_eq!(body["error"]["code"], -32601);
    server.shutdown().await;
}

#[tokio::test]
async fn json_rpc_batched_request_rejected() {
    let server = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let url = format!("{}?token={}", server.url(), token);
    let resp = reqwest::Client::new().post(&url)
        .json(&serde_json::json!([
            {"jsonrpc":"2.0","method":"tools/list","id":1},
            {"jsonrpc":"2.0","method":"tools/list","id":2}
        ]))
        .send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32600);
    server.shutdown().await;
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement dispatcher**

The dispatcher reads the request body, rejects arrays (batches) with `-32600`, then matches `method`:
- `"tools/list"` → return `worker_tools_list()` as JSON
- `"tools/call"` → match `params.name` against the 8 tool names; for each, call the corresponding `handlers::*` freestanding fn with `(pm_service.as_ref(), &ctx, args)` (or whichever deps that handler needs); convert `Result<Value, McpHandlerError>` to `JsonRpcResponse`. Unknown name → return `-32601 Method not found`.

- [ ] **Step 4: Run, pass (4 tests)**
- [ ] **Step 5: Commit** as `spur-mcp: add JSON-RPC dispatcher to WorkerMcpServer (batch rejected)`

---

### Task 18: report_progress dual-gating

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: `crates/spur-mcp/tests/worker_server_dispatch.rs` (extend)

The server needs to know per-delegation whether `report_progress` is enabled. Add a `register_delegation(delegation_id, ProgressEnabled)` API called by the orchestrator at dispatch time. Filter `report_progress` from `tools/list` if disabled; reject `tools/call` with `-32601` if disabled.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn report_progress_filtered_when_disabled() {
    let server = test_server_with_real_pm().await;
    server.register_delegation("d-1".into(), DelegationCaps { progress: false });
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/list", serde_json::json!({})).await;
    let names: Vec<&str> = body["result"]["tools"].as_array().unwrap()
        .iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!names.contains(&"report_progress"), "report_progress should be filtered");
    server.shutdown().await;
}

#[tokio::test]
async fn report_progress_dispatch_rejected_when_disabled_even_if_called_directly() {
    let server = test_server_with_real_pm().await;
    server.register_delegation("d-1".into(), DelegationCaps { progress: false });
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/call",
        serde_json::json!({"name": "report_progress", "arguments": {"message": "hi"}})).await;
    assert_eq!(body["error"]["code"], -32601);
    server.shutdown().await;
}

#[tokio::test]
async fn report_progress_works_when_enabled() {
    let server = test_server_with_real_pm().await;
    server.register_delegation("d-1".into(), DelegationCaps { progress: true });
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/call",
        serde_json::json!({"name": "report_progress", "arguments": {"message": "hi"}})).await;
    assert_eq!(body["result"]["ok"], true);
    server.shutdown().await;
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement `DelegationCaps` struct, `register_delegation`, dispatch dual-gate**

```rust
// In worker_server.rs
#[derive(Debug, Clone, Copy)]
pub struct DelegationCaps {
    pub progress: bool,
}

pub struct WorkerMcpServer {
    // ... existing fields ...
    delegations: dashmap::DashMap<String, DelegationCaps>,
}

impl WorkerMcpServer {
    pub fn register_delegation(&self, delegation_id: String, caps: DelegationCaps) {
        self.delegations.insert(delegation_id, caps);
    }

    fn caps_for(&self, delegation_id: &str) -> DelegationCaps {
        self.delegations.get(delegation_id).map(|r| *r).unwrap_or(DelegationCaps { progress: false })
    }
}

// In tools/list dispatcher:
let caps = self.caps_for(&ctx.delegation_id);
let mut tools = crate::tools::worker_tools_list();
if !caps.progress {
    tools.retain(|t| t.name != "report_progress");
}

// In tools/call dispatcher (before matching name):
if name == "report_progress" && !self.caps_for(&ctx.delegation_id).progress {
    return JsonRpcResponse::error(id, -32601, "Method not found");
}
```

- [ ] **Step 4: Run, pass (3 tests)**
- [ ] **Step 5: Commit** as `spur-mcp: dual-gate report_progress per delegation (tools/list + tools/call)`

---

### Task 19: Synchronous audit emission for write tools

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: `crates/spur-mcp/tests/worker_server_audit.rs` (NEW)

Wrap every `tools/call` for `update_issue` (and any future write tool) with synchronous audit comment emission via `PmService`. The audit comment carries the `WorkerMcp{subkind: Call}` sentinel JSON.

- [ ] **Step 1: Write the failing test (assert the worker's `delegation_id` issue gets a `worker-mcp` audit comment with `subkind: call` after a successful update_issue)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement audit wrapper. Also wire up the auth-denied audit path from Task 16 (which was previously a stub `tracing::warn!`) to actually emit `WorkerMcp{subkind: AuthDenied}`.**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: synchronous audit sentinel for worker write tools + auth failures`

---

### Task 20: Read-tool aggregation buffer + Drop wrapper

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: extend `crates/spur-mcp/tests/worker_server_audit.rs`

Add `ReadAuditBuffer` wrapper struct stored in `dashmap::DashMap<DelegationId, ReadAuditBuffer>`. On every read-tool call, append a `ReadAuditEntry { tool_name, target_issue_id, ts }` to the buffer. The wrapper has a synchronous `Drop` impl that does a non-blocking `try_send` on a `mpsc::UnboundedSender<FlushMessage>` (channel created in Task 21).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn read_tool_calls_append_to_buffer() {
    let server = test_server_with_issue("bd-100").await;
    server.register_delegation("d-1".into(), DelegationCaps { progress: false });
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    for _ in 0..3 {
        call_jsonrpc(&server, &token, "tools/call",
            serde_json::json!({"name":"get_issue","arguments":{"id":"bd-100"}})).await;
    }
    let buf = server.peek_read_buffer("d-1").expect("buffer exists");
    assert_eq!(buf.entry_count(), 3);
    server.shutdown().await;
}

#[tokio::test]
async fn drop_buffer_sends_on_flush_channel() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let buf = ReadAuditBuffer::new("d-1".into(), tx);
    buf.append_for_test(ReadAuditEntry { tool_name: "get_issue".into(), target_issue_id: None, ts: 0 });
    drop(buf);
    let msg = rx.try_recv().expect("flush message expected");
    assert_eq!(msg.delegation_id, "d-1");
    assert_eq!(msg.entries.len(), 1);
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement buffer + Drop**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: read-audit buffer with sync Drop -> mpsc flush channel`

---

### Task 21: Background audit flusher task

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: `crates/spur-mcp/tests/worker_server_audit_flush.rs` (NEW)

Spawn a dedicated background task in `start()` that owns the `mpsc::UnboundedReceiver<FlushMessage>`. Each received message is encoded as a `WorkerMcp{subkind: Call}` aggregated audit comment and written to `PmService` with exponential backoff (30s base, 5min cap). On 5-minute continuous failure, emit one `WorkerMcp{subkind: PmDegraded}` event via `FunnelHandle`.

- [ ] **Step 1: Write the failing test (mock PM service that always fails; assert PmDegraded fires after threshold; PM that succeeds → assert audit comment lands)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement the flusher task**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: background audit flusher with exp backoff + PmDegraded after 5min`

---

### Task 22: Active-delegation count + atomic shutdown-cancel

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: `crates/spur-mcp/tests/worker_server_lifecycle.rs` (extend)

Add `active_count: AtomicUsize` and `shutting_down: AtomicBool`. `register_delegation` increments; new method `complete_delegation(delegation_id)` decrements. When count reaches 0, kick off a background drain (`tokio::spawn`). If a new `register_delegation` arrives during drain, atomic compare-and-swap `shutting_down=false`, abort the drain task, server stays up.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn server_stays_up_with_active_delegation() {
    let server = test_server().await;
    server.register_delegation("d-1".into(), DelegationCaps { progress: false });
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let token = server.issue_token("d-1", std::time::Duration::from_secs(60));
    let url = format!("{}?token={}", server.url(), token);
    let resp = reqwest::Client::new().post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send().await;
    assert!(resp.is_ok(), "server must still accept connections");
    server.shutdown().await;
}

#[tokio::test]
async fn server_shuts_down_after_last_delegation_completes() {
    let server = test_server().await;
    let url = server.url();
    server.register_delegation("d-1".into(), DelegationCaps { progress: false });
    server.complete_delegation("d-1");
    // Allow drain
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let resp = reqwest::Client::new().post(&format!("{url}?token=x"))
        .json(&serde_json::json!({}))
        .send().await;
    assert!(resp.is_err(), "server must be down");
}

#[tokio::test]
async fn shutdown_cancelled_by_new_dispatch() {
    let server = test_server().await;
    server.register_delegation("d-1".into(), DelegationCaps { progress: false });
    server.complete_delegation("d-1");
    // During drain (within 5s), register a new delegation
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    server.register_delegation("d-2".into(), DelegationCaps { progress: false });
    // Server stays up
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let token = server.issue_token("d-2", std::time::Duration::from_secs(60));
    let url = format!("{}?token={}", server.url(), token);
    let resp = reqwest::Client::new().post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send().await;
    assert!(resp.is_ok());
    server.shutdown().await;
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement count + atomic shutdown-cancel**
- [ ] **Step 4: Run, pass (3 tests)**
- [ ] **Step 5: Commit** as `spur-mcp: active-delegation lifecycle with atomic shutdown-cancel`

---

### Task 23: Per-delegation summary event emission

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: extend `crates/spur-mcp/tests/worker_server_audit.rs`

When `complete_delegation(id)` is called, emit one `SpurEventBody::WorkerMcpDelegationSummary` via `FunnelHandle` summarizing the delegation's calls (count, by-tool, p99 latency, errors). For long-running delegations, also fire every 60s via a per-delegation tokio task.

- [ ] **Step 1: Write the failing test (subscribe to funnel; assert one summary event after `complete_delegation`)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement (track per-delegation `CallStats { count, by_tool, latencies, errors }`; emit summary on complete; spawn 60s ticker for long-running)**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: emit WorkerMcpDelegationSummary on delegation end + every 60s`

---

### Task 24: Drain-with-timeout shutdown

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Test: extend `crates/spur-mcp/tests/worker_server_lifecycle.rs`

Refine `shutdown()` to:
1. Set `shutting_down = true` so `register_delegation` no longer races.
2. Wait up to 5 seconds for in-flight requests to complete (track an `in_flight: AtomicUsize` decremented at end of `handle_connection`).
3. Force-abort accept loop and worker tasks on timeout.
4. Drain audit flusher channel (process any remaining FlushMessages with one final synchronous attempt).

- [ ] **Step 1: Write the failing test (start a request that takes 10s; call shutdown; assert it returns within ~5s, and the in-flight request gets a clean error)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: WorkerMcpServer::shutdown drains in-flight (5s) then force-aborts`

---

## Phase 5 — Orchestrator Integration

### Task 25: worker_mcp_servers map + lazy init helper

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Test: `crates/spur-core/tests/orchestrator_worker_mcp.rs` (NEW)

Add field `worker_mcp_servers: Arc<DashMap<String, Arc<WorkerMcpServer>>>` to `Orchestrator` (key = `brain_session_id` as String). Add helper:

```rust
async fn ensure_worker_mcp_for(
    &self,
    brain_session_id: &str,
) -> Result<Arc<WorkerMcpServer>, BindError> {
    if let Some(existing) = self.worker_mcp_servers.get(brain_session_id) {
        return Ok(Arc::clone(&existing));
    }
    // Use entry API to avoid double-start race
    use dashmap::mapref::entry::Entry;
    match self.worker_mcp_servers.entry(brain_session_id.to_string()) {
        Entry::Occupied(e) => Ok(Arc::clone(e.get())),
        Entry::Vacant(e) => {
            let server = WorkerMcpServer::start(
                brain_session_id.to_string(),
                Arc::clone(&self.pm_service),
                Arc::clone(&self.feature_gate),
                self.funnel.handle(),
            ).await?;
            e.insert(Arc::clone(&server));
            Ok(server)
        }
    }
}
```

- [ ] **Step 1: Write the failing test (concurrent calls return same Arc; second brain_session gets a different server)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement field + helper**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-core: add worker_mcp_servers map + lazy ensure_worker_mcp_for helper`

---

### Task 26: Conditional mcp_servers injection at orchestrator.rs:6571-6577

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:6571-6577`
- Test: extend `crates/spur-core/tests/orchestrator_worker_mcp.rs`

Replace the unconditional `vec![]` with the conditional from spec §3.6. When `enable_worker_mcp = true`:
1. Call `self.ensure_worker_mcp_for(brain_session_id).await?`
2. Issue a per-delegation token via `server.issue_token(&request_id, ttl)`.
3. Register the delegation: `server.register_delegation(request_id.clone(), DelegationCaps { progress: enable_worker_progress })`.
4. Build `mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-worker-mcp", &format!("{}?token={}", server.url(), token)))]`.

When `enable_worker_mcp` is `None` or `false`: pass `vec![]` (preserve existing behavior).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn enable_worker_mcp_false_passes_empty_mcp_servers() {
    let captured = capture_session_mcp_servers(/*enable_worker_mcp:*/ None).await;
    assert!(captured.is_empty(), "default must preserve historical contract");
}

#[tokio::test]
async fn enable_worker_mcp_true_injects_token_url() {
    let captured = capture_session_mcp_servers(Some(true)).await;
    assert_eq!(captured.len(), 1);
    let url = captured[0].url();
    assert!(url.contains("?token="));
    assert!(url.contains("/mcp"));
}
```

- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement at `:6571-6577`**
- [ ] **Step 4: Run, pass + `cargo build -p spur-core`**
- [ ] **Step 5: Commit** as `spur-core: conditional worker MCP injection at worker dispatch site`

---

### Task 27: flush_delegation in worker exit hook

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (find the worker exit handler — search for `WorkerCompleted` event emission or the `attempt_setup` Drop path)
- Test: extend orchestrator integration test

When a worker completes (success, failure, or crash), call `server.complete_delegation(request_id)` AND `server.flush_delegation(request_id)` synchronously to drain any pending audit buffers.

- [ ] **Step 1: Write the failing test (dispatch worker, do a get_issue read, kill the worker; assert the read summary lands in beads via PmService)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement exit hook integration**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-core: flush worker MCP audit on worker exit`

---

### Task 28: WorkerMcpServer shutdown in retire_brain_session

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:1021` (`retire_brain_session`) — alongside existing `shutdown_mcp_server` call at `:960`
- Test: extend orchestrator integration test

Before the brain MCP server shutdown, remove the worker MCP server from `worker_mcp_servers` and call its `shutdown()` (drains in-flight, releases port).

- [ ] **Step 1: Write the failing test (start brain session with active worker MCP, retire it, assert worker server port is no longer reachable)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-core: shutdown worker MCP server on brain session retirement`

---

### Task 29: End-to-end orchestrator → worker → MCP call integration test

**Files:**
- Test: `crates/spur-core/tests/e2e_worker_mcp.rs` (NEW)

Full path test — orchestrator dispatches a mock worker (a tokio task that POSTs to the worker MCP URL); worker calls `update_issue`; orchestrator observes the audit sentinel and the WorkerMcpDelegationSummary event.

- [ ] **Step 1: Write the failing test**
- [ ] **Step 2: Run, fail (likely a wiring bug if any prior task missed integration)**
- [ ] **Step 3: Fix any issues found (this task is the integration smoke for everything in Phase 5)**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-core: end-to-end worker MCP integration test (smoke)`

---

## Phase 6 — Test Matrix

These tasks add specific high-value tests beyond the unit/integration tests embedded in earlier phases. They focus on security and SDK compatibility.

### Task 30: Cross-delegation spoof rejected

**Files:**
- Test: `crates/spur-mcp/tests/security_spoof.rs` (NEW)

- [ ] **Step 1: Write the test**

```rust
#[tokio::test]
async fn worker_b_cannot_spoof_worker_a_delegation_id() {
    let server = test_server_with_real_pm().await;
    server.register_delegation("d-A".into(), DelegationCaps { progress: false });
    server.register_delegation("d-B".into(), DelegationCaps { progress: false });
    let token_b = server.issue_token("d-B", std::time::Duration::from_secs(60));

    // Worker B uses its own valid token but tries to call update_issue with a payload
    // claiming to be on behalf of d-A. The middleware extracts d-B from the token;
    // any audit emission MUST attribute to d-B, not the payload.
    call_jsonrpc(&server, &token_b, "tools/call",
        serde_json::json!({"name":"update_issue","arguments":{"id":"bd-1","comment":"x"}})).await;

    // The audit sentinel on bd-1 should reference delegation_id "d-B", never "d-A"
    let comments = test_pm_comments_for("bd-1").await;
    assert!(comments.iter().any(|c| c.contains("\"delegation_id\":\"d-B\"")));
    assert!(!comments.iter().any(|c| c.contains("\"delegation_id\":\"d-A\"")));
    server.shutdown().await;
}
```

- [ ] **Step 2: Run** (should pass on first try if the middleware in Task 16 + audit in Task 19 are correct)
- [ ] **Step 3: If fails, fix the responsible component**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: security test — token-extracted delegation_id wins over payload claim`

---

### Task 31: enable_worker_mcp=false strictly preserves vec![]

This was covered in Task 26. Skip if those tests fully cover the contract; otherwise add an explicit security-test variant asserting that no `worker_mcp_servers` entry is created when `enable_worker_mcp` is omitted.

- [ ] **Step 1: Write the failing test (assert `orchestrator.worker_mcp_servers.is_empty()` after dispatching N workers all with `enable_worker_mcp = None`)**
- [ ] **Step 2: Run, fail or skip**
- [ ] **Step 3: If failed, fix**
- [ ] **Step 4: Pass**
- [ ] **Step 5: Commit** as `spur-mcp: regression test — default opt-out creates zero worker MCP state`

---

### Task 32: Cross-brain_session fetch_outcome_artifact returns Unauthorized

**Files:**
- Test: `crates/spur-mcp/tests/security_cross_session.rs` (NEW)

Already covered by Task 10's handler test, but add an integration variant that exercises the full HTTP path.

- [ ] **Step 1: Write the failing test (server for session-A; worker token attempts to fetch artifact from session-B → Unauthorized)**
- [ ] **Step 2-5: TDD cycle**

Commit: `spur-mcp: integration security test — cross-brain_session fetch denied`

---

### Task 33: Token NOT in argv or env at worker subprocess

**Files:**
- Test: `crates/spur-core/tests/security_token_transmission.rs` (NEW)

Build a test orchestrator that records the `Command` builder used to spawn workers. Dispatch a worker with `enable_worker_mcp=true`. Assert:
1. No element of `command.argv()` matches the regex `token=`.
2. No env var key matches `MCP|TOKEN|SPUR_WORKER` (besides any pre-existing safe ones).
3. The token IS present in the ACP `session/new` payload sent over stdin (intercepted via the test's mock ACP transport).

- [ ] **Step 1-5: TDD cycle**
- [ ] **Step 5: Commit** as `spur-core: security test — token never leaks to argv or env`

---

### Task 34-40: Per-SDK matrix smoke tests (one task per SDK)

For each of the 7 SDKs, add a CI-gated smoke test that:
1. Dispatches a worker with `enable_worker_mcp=true` using the real SDK CLI.
2. Asserts the worker successfully calls `get_issue` (verifies `mcpServers` is honored).
3. Asserts a non-existent tool returns `-32601` (verifies tools/list is correct on this SDK).

SDKs:
- **Task 34:** kimi (`kimi -y --afk acp`)
- **Task 35:** gemini (`gemini --acp -y -m deep-thinker`)
- **Task 36:** codex (`npx --yes @zed-industries/codex-acp@0.11.1`)
- **Task 37:** claude-code (`npx --yes @agentclientprotocol/claude-agent-acp@0.26.0`)
- **Task 38:** opencode
- **Task 39:** claude-code-sj
- **Task 40:** kiro

Each task:
- [ ] **Step 1: Write the per-SDK test in `crates/spur-core/tests/sdk_matrix_<sdk>.rs`**
- [ ] **Step 2: Run, fail (or skip if SDK not available locally)**
- [ ] **Step 3: Add CI gating job**
- [ ] **Step 4: Run, pass (locally if SDK available)**
- [ ] **Step 5: Commit** as `spur-core: SDK matrix smoke test for <sdk>`

If an SDK does not honor `mcpServers` in `session/new` payload (per spec §6 graceful degradation row), document the failure as a known gap and file a follow-up issue. Do not block the rollout on a non-honoring SDK.

---

### Task 41: Concurrency stress test (N=8 workers)

**Files:**
- Test: `crates/spur-mcp/tests/stress_concurrent.rs` (NEW)

Spawn 8 concurrent worker tasks each making 50 mixed read/write tool calls against a single shared `WorkerMcpServer`. Assert: no deadlock, all calls succeed, p99 latency < 500ms (use `instant::Instant` per call), audit buffer integrity preserved (sum of read entries across all delegations equals the expected total), all 8 `WorkerMcpDelegationSummary` events fire.

- [ ] **Step 1: Write the failing test**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Fix any concurrency issues found (if dashmap sharding is insufficient or there's lock contention)**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: concurrency stress test (N=8 workers, mixed read/write)`

---

### Task 42: Failure injection — PM unavailable during read-flush

**Files:**
- Test: `crates/spur-mcp/tests/failure_pm_unavailable.rs` (NEW)

Use a mock `PmService` that returns errors. Have a worker make 5 read calls. Wait 5+ minutes (test-controlled clock or `tokio::time::pause()`). Assert: backoff retries are visible (log captures) and exactly one `PmDegraded` funnel event fires.

- [ ] **Step 1: Write the failing test (use `tokio::time::pause()` for fast-forward)**
- [ ] **Step 2: Run, fail**
- [ ] **Step 3: Fix the flusher backoff logic if needed**
- [ ] **Step 4: Run, pass**
- [ ] **Step 5: Commit** as `spur-mcp: failure-injection test — PM unavailable triggers backoff + PmDegraded`

---

## Final Wrap-Up

### Task 43: Documentation update

**Files:**
- Modify: `docs/architecture-spur-mcp.md` (add a new §11 "Worker MCP Server" subsection with the topology diagram from spec §3)
- Modify: `crates/spur-mcp/src/lib.rs` (rustdoc on `worker_server` module pointing at the spec)

- [ ] **Step 1:** Write the architecture doc additions
- [ ] **Step 2:** Run `cargo doc -p spur-mcp` to verify rustdoc builds
- [ ] **Step 3:** Commit as `docs: document worker MCP server architecture`

### Task 44: File phase-2 follow-up beads tickets

Per spec §12, file 7 separate bd-* tickets:
- HMAC key rotation
- JSON-RPC batched request hardening
- Clock-skew tolerance configurability
- Metrics infrastructure (Prometheus/OTel)
- HTTP keep-alive on worker server
- Per-delegation rate-limiting / quota
- Worker roles (Reviewer / Auditor / Coder / Doc-writer)

Each ticket references this plan + spec.

- [ ] **Step 1:** Use `mcp__spur-mcp__create_issue` (or `bd create`) to file each follow-up ticket with parent `bd-14cq`
- [ ] **Step 2:** Add a final spur-audit comment on bd-14cq listing the 7 ticket IDs
- [ ] **Step 3:** Commit any docs changes as `spur: file phase-2 worker MCP follow-ups`

---

## Self-Review Checklist (run before merging)

- [ ] Spec coverage: every locked decision in spec §13 has an implementing task ✓
- [ ] No placeholders: search this plan for "TBD", "TODO", "implement later", "fill in details" → none found
- [ ] Type consistency: `WorkerCallContext`, `McpHandlerError`, `WorkerMcpServer`, `WorkerMcpSubkind`, `DelegationCaps`, `ReadAuditBuffer`, `FlushMessage`, `WorkerTokenPayload`, `TokenError`, `BindError` are referenced by the same names across all tasks ✓
- [ ] Test coverage hits all 6 layers from spec §10: unit (T1, T2, T5, T6, T20), integration (T7-T13, T17, T26-T29), SDK matrix (T34-T40), security (T30, T32, T33), concurrency (T41), failure injection (T42) ✓
- [ ] Backwards compat: T26 explicitly tests `enable_worker_mcp=None` produces `vec![]` (preserves the historical "Workers get no MCP servers" contract) ✓
- [ ] Phase-2 gaps explicitly out of scope: T44 files them as separate tickets; no in-scope task implements them ✓

---

## Execution Notes

- **Task ordering**: Phases 1, 2, 3 can be done in order. Phase 4 depends on all of 1-3. Phase 5 depends on 4. Phase 6 depends on 5. Within Phase 4, tasks 15-24 are sequential (each builds on prior). Within Phase 5, tasks 25-29 are sequential. Phase 6 tasks are independent.
- **Estimated effort**: ~4 weeks of focused work for one developer at the spec'd ~1,700 LOC. Larger if reviewer-driven iteration adds more.
- **Critical path**: Tasks 5 (token), 6 (handlers scaffold), 11 (get_task_diff extraction), 17 (dispatcher), 22 (lifecycle), 26 (orchestrator integration). These are the highest-risk single tasks.
- **Test-only tasks (no commit if test passes on first try)**: T31 may be a no-op if Task 26 already covers the assertion completely. Verify and skip if so.
