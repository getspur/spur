# Phase 2: Quota Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire `FeatureGate` quota reads into actual enforcement points in `spur-core` (semaphore, event sink) and `spur-cli` (PM adapter gating).

**Architecture:** `Orchestrator` receives an `Option<Arc<FeatureGate>>` at construction. It uses the gate's `quota()` reads to set the delegation semaphore limit and event sink rotation threshold. If no gate is provided (tests, offline paths), it falls back to existing config defaults. PM service initialization is gated behind the `pm_integration` feature check in CLI before the orchestrator is even involved.

**Tech Stack:** Rust 2021, `spur-core`, `spur-cli`, `spur-license` (FeatureGate already implemented)

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-core/src/event_sink.rs` | Modify | `spawn_sink` accepts `max_bytes` parameter |
| `crates/spur-core/src/orchestrator.rs` | Modify | Add `feature_gate` field; use quota in semaphore + sink |
| `crates/spur-cli/src/main.rs` | Modify | Pass `license.feature_gate()` to `Orchestrator::new`; gate PM init |
| `crates/spur-cli/src/commands/init.rs` | Modify | Pass `None` for feature_gate to `Orchestrator::new` |
| `crates/spur-core/tests/init_agents.rs` | Modify | Pass `None` for feature_gate to `Orchestrator::new` |

---

## Task 1: Modify EventSink to Accept max_bytes

**Files:**
- Modify: `crates/spur-core/src/event_sink.rs`

- [ ] **Step 1: Modify `spawn_sink` signature**

```rust
pub fn spawn_sink(mut rx: broadcast::Receiver<SpurEvent>, max_bytes: u64)
```

- [ ] **Step 2: Modify `SinkState::open` to accept `max_bytes` instead of reading env var**

```rust
fn open(dir: &Path, max_bytes: u64) -> std::io::Result<Self> {
    let path = rotated_path(dir);
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    Ok(Self {
        dir: dir.to_path_buf(),
        writer: BufWriter::with_capacity(FLUSH_BYTES, file),
        current_path: path,
        bytes_in_file: bytes,
        max_bytes,
    })
}
```

- [ ] **Step 3: Update `spawn_sink` body to pass `max_bytes` to `SinkState::open`**

```rust
let mut state = match SinkState::open(&events_dir, max_bytes) {
```

- [ ] **Step 4: Update test helper `open_with_max`**

It already accepts `max_bytes`. Ensure it still works with the new `SinkState::open` signature.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-core event_sink`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/event_sink.rs
git commit -m "feat(spur-core): EventSink accepts max_bytes parameter"
```

---

## Task 2: Add FeatureGate to Orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Add `feature_gate` field to `Orchestrator`**

```rust
pub struct Orchestrator {
    // ... existing fields ...
    /// Feature gate for dynamic quota/feature enforcement.
    feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
}
```

- [ ] **Step 2: Modify `Orchestrator::new` to accept `feature_gate`**

```rust
pub fn new(
    repo_root: PathBuf,
    config: SpurConfig,
    feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
) -> Result<Self> {
    // ... existing logic ...
    
    // Compute max_bytes for event sink from feature gate or default
    let max_bytes = feature_gate
        .as_ref()
        .and_then(|g| g.quota(spur_license::QuotaKey::EventRetentionBytes))
        .and_then(|v| v.as_bytes())
        .unwrap_or(DEFAULT_MAX_BYTES);
    crate::event_sink::spawn_sink(event_tx.subscribe(), max_bytes);
    
    // ... rest of existing logic ...
    
    Ok(Self {
        // ... existing fields ...
        feature_gate,
    })
}
```

- [ ] **Step 3: Replace static `max_concurrent` reads with dynamic quota at 3 call sites**

Find all 3 occurrences of `let max_concurrent = self.config.worktree.max_concurrent;` (lines ~794, ~2024, ~2280).

Replace each with:
```rust
let max_concurrent = self
    .feature_gate
    .as_ref()
    .and_then(|g| g.quota(spur_license::QuotaKey::MaxConcurrentWorkers))
    .and_then(|v| v.as_count())
    .map(|n| n as usize)
    .unwrap_or(self.config.worktree.max_concurrent);
```

- [ ] **Step 4: Add `with_feature_gate` builder method**

```rust
pub fn with_feature_gate(mut self, gate: std::sync::Arc<spur_license::FeatureGate>) -> Self {
    self.feature_gate = Some(gate);
    self
}
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p spur-core`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): Orchestrator reads semaphore + sink quotas from FeatureGate"
```

---

## Task 3: Update Call Sites

**Files:**
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-cli/src/commands/init.rs`
- Modify: `crates/spur-core/tests/init_agents.rs`

- [ ] **Step 1: Update `main.rs` Watch path**

Find: `let orch = Orchestrator::new(repo_root.clone(), config)?;`
Replace with:
```rust
let orch = Orchestrator::new(repo_root.clone(), config, Some(license.feature_gate()))?;
```

- [ ] **Step 2: Update `main.rs` other paths**

Find other `Orchestrator::new` calls (lines ~661, ~748). Pass `None` for feature_gate (these are non-interactive CLI commands that don't need dynamic quotas):
```rust
let mut orch = Orchestrator::new(repo_root, config, None)?;
```

- [ ] **Step 3: Update `init.rs`**

Find: `let mut orch = Orchestrator::new(repo_root.clone(), SpurConfig::default())?;`
Replace with:
```rust
let mut orch = Orchestrator::new(repo_root.clone(), SpurConfig::default(), None)?;
```

- [ ] **Step 4: Update `init_agents.rs` tests**

Find all `Orchestrator::new(tmp.path().into(), SpurConfig::default())` calls.
Replace with:
```rust
Orchestrator::new(tmp.path().into(), SpurConfig::default(), None)
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check --workspace`
Expected: PASS (all crates compile)

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/main.rs crates/spur-cli/src/commands/init.rs crates/spur-core/tests/init_agents.rs
git commit -m "feat(spur-core): pass FeatureGate to Orchestrator at construction"
```

---

## Task 4: Gate PM Service Initialization

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Gate PM service behind `pm_integration` feature**

In the Watch path of `main.rs`, before creating `PmService`, check the feature gate:

```rust
let pm_service = if license.feature_gate().has(spur_license::FeatureKey::PM_INTEGRATION) {
    spur_pm::PmService::try_new(
        config.pm.github.as_ref().and_then(|g| g.repo.clone()),
        config.pm.beads.as_ref().is_none_or(|b| b.enabled),
        config.pm.github.as_ref().is_none_or(|g| g.enabled),
        &repo_root,
        None,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("PM service initialization failed: {e}");
        None
    })
} else {
    tracing::info!("PM integration not available on current tier");
    None
};
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p spur-cli`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): gate PM service behind pm_integration FeatureGate"
```

---

## Task 5: Tests and Verification

- [ ] **Step 1: Run all spur-core tests**

Run: `cargo test -p spur-core`
Expected: ALL PASS

- [ ] **Step 2: Run all spur-cli tests**

Run: `cargo test -p spur-cli`
Expected: ALL PASS

- [ ] **Step 3: Run all spur-license tests**

Run: `cargo test -p spur-license`
Expected: ALL PASS

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: CLEAN

- [ ] **Step 5: Run formatter**

Run: `cargo fmt --all`

- [ ] **Step 6: Final commit**

```bash
git add -A
git commit -m "feat(spur-core): Phase 2 quota enforcement — semaphore, sink, PM gating

- EventSink accepts max_bytes parameter (was hardcoded/env-only)
- Orchestrator reads MaxConcurrentWorkers from FeatureGate for semaphore
- Orchestrator reads EventRetentionBytes from FeatureGate for sink rotation
- CLI gates PM service initialization behind pm_integration feature check
- All call sites updated for new Orchestrator::new signature"
```

---

*Plan complete. Ready for execution.*
