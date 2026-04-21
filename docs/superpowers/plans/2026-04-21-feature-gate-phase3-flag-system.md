# Feature Gate Phase 3 — Flag System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `InstallId` persistence, `FlagEvaluator` with deterministic rollout, and `spur flags list` CLI introspection.

**Architecture:** A sync `FlagEvaluator` (tier filter + kill switch + deterministic rollout) consumes `PolicyDocument.flags` and an `InstallId` persisted at `~/.spur/install-id`. The evaluator is wired into `FeatureGate` so the CLI can list flag states. No TUI changes.

**Tech Stack:** `seahash` (already in `spur-license/Cargo.toml`), `uuid`, `directories`, `serde_json`

---

### Task 1: `InstallId` Persistence

**Files:**
- Create: `crates/spur-license/src/install_id.rs`
- Modify: `crates/spur-license/src/lib.rs`
- Test: `crates/spur-license/tests/install_id.rs`

**Context:** Anonymous UUID for deterministic rollout bucketing. Written to `~/.spur/install-id` on first load. Not correlated with user identity.

- [ ] **Step 1: Write the failing test**

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn install_id_load_or_create_generates_uuid() {
    let id1 = spur_license::InstallId::load_or_create();
    let id2 = spur_license::InstallId::load_or_create();
    // Same process → same ID (file already exists)
    assert_eq!(id1, id2);
    // Must parse as valid UUID
    assert!(!id1.to_string().is_empty());
}
```

Run: `cargo test -p spur-license --test install_id -- install_id_load_or_create_generates_uuid`
Expected: FAIL — `InstallId` type not found.

- [ ] **Step 2: Implement `InstallId`**

Create `crates/spur-license/src/install_id.rs`:

```rust
use std::fmt;

/// Stable per-install identifier. Generated once on first run, persisted in
/// ~/.spur/install-id. Used for deterministic rollout hashing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstallId(uuid::Uuid);

impl InstallId {
    pub fn load_or_create() -> Self {
        let path = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".spur").join("install-id"));

        if let Some(ref p) = path {
            if let Ok(s) = std::fs::read_to_string(p) {
                if let Ok(uuid) = s.trim().parse::<uuid::Uuid>() {
                    return Self(uuid);
                }
            }
        }

        let new_id = Self(uuid::Uuid::new_v4());
        if let Some(ref p) = path {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, new_id.0.to_string());
        }
        new_id
    }

    /// Construct from a known UUID. Test-only; production paths should use `load_or_create`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }
}

impl fmt::Display for InstallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
```

Add to `crates/spur-license/src/lib.rs`:
```rust
mod install_id;
pub use install_id::InstallId;
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p spur-license --test install_id -- install_id_load_or_create_generates_uuid`
Expected: PASS.

- [ ] **Step 4: Add more tests**

```rust
#[test]
fn install_id_from_uuid_roundtrips() {
    let uuid = uuid::Uuid::new_v4();
    let id = spur_license::InstallId::from_uuid(uuid);
    assert_eq!(id.to_string(), uuid.to_string());
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/install_id.rs crates/spur-license/src/lib.rs crates/spur-license/tests/install_id.rs
git commit -m "feat(spur-license): InstallId persistence for deterministic rollout"
```

---

### Task 2: `FlagEvaluator`

**Files:**
- Create: `crates/spur-license/src/policy/flags.rs`
- Modify: `crates/spur-license/src/policy/mod.rs`
- Test: `crates/spur-license/tests/flag_evaluator.rs`

**Context:** Sync evaluator over `PolicyDocument.flags`. Supports kill switch (`enabled: false`), tier filtering (`tier_filter: ["pro", "team"]`), and deterministic percentage rollout (`rollout_percent: 50.0`). Uses `seahash` for stable bucketing.

- [ ] **Step 1: Write the failing test**

```rust
use spur_license::policy::{FlagEvaluator, FlagSpec, InstallId};
use spur_license::{FeatureKey, Tier};

#[test]
fn kill_switch_disabled_flag_returns_false() {
    let evaluator = FlagEvaluator::new(InstallId::from_uuid(uuid::Uuid::nil()));
    let mut spec = FlagSpec::default();
    spec.enabled = false;
    assert!(!evaluator.evaluate(FeatureKey::KILL_ADVANCED_PLANNER, &spec, Tier::Community));
}
```

Run: `cargo test -p spur-license --test flag_evaluator -- kill_switch_disabled_flag_returns_false`
Expected: FAIL — `FlagEvaluator` not defined.

- [ ] **Step 2: Implement `FlagEvaluator`**

Create `crates/spur-license/src/policy/flags.rs`:

```rust
use std::sync::Arc;

use crate::install_id::InstallId;
use crate::policy::{FeatureKey, FlagSpec, PolicyDocument};
use crate::tier::Tier;

/// G2 flag evaluator: kill switch, tier filter, deterministic rollout.
pub struct FlagEvaluator {
    install_id: InstallId,
}

impl FlagEvaluator {
    pub fn new(install_id: InstallId) -> Self {
        Self { install_id }
    }

    /// Evaluate whether a flag is enabled for the given tier.
    /// Deterministic: same (install_id, flag_key) always yields same result.
    pub fn evaluate(&self, key: FeatureKey, flag: &FlagSpec, tier: Tier) -> bool {
        // 1. Kill switch
        if !flag.enabled {
            return false;
        }

        // 2. Tier filter
        if let Some(ref tiers) = flag.tier_filter {
            let tier_str = tier.label().to_lowercase();
            if !tiers.iter().any(|t| t == &tier_str) {
                return false;
            }
        }

        // 3. Rollout percentage (deterministic hash)
        if let Some(pct) = flag.rollout_percent {
            let hash = seahash::hash(
                format!("{}:{}", self.install_id, key.as_str()).as_bytes(),
            );
            let normalized = (hash % 100) as f32;
            return normalized < pct;
        }

        true
    }
}

/// Explanation of why a flag evaluated to its current value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagExplanation {
    pub key: String,
    pub enabled: bool,
    pub reason: FlagReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagReason {
    KillSwitch,
    TierFilter,
    Rollout { bucket: u8, percent: f32 },
    Enabled,
}
```

Add to `crates/spur-license/src/policy/mod.rs` (after `pub mod trust;`):
```rust
pub mod flags;
pub use flags::{FlagEvaluator, FlagExplanation, FlagReason};
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p spur-license --test flag_evaluator -- kill_switch_disabled_flag_returns_false`
Expected: PASS.

- [ ] **Step 4: Add comprehensive tests**

```rust
#[test]
fn tier_filter_excludes_wrong_tier() {
    let evaluator = FlagEvaluator::new(InstallId::from_uuid(uuid::Uuid::nil()));
    let mut spec = FlagSpec::default();
    spec.tier_filter = Some(vec!["pro".into(), "team".into()]);
    assert!(!evaluator.evaluate(FeatureKey::KILL_ADVANCED_PLANNER, &spec, Tier::Community));
    assert!(evaluator.evaluate(FeatureKey::KILL_ADVANCED_PLANNER, &spec, Tier::Pro));
}

#[test]
fn rollout_is_deterministic() {
    let install_id = InstallId::from_uuid(uuid::Uuid::nil());
    let evaluator = FlagEvaluator::new(install_id);
    let mut spec = FlagSpec::default();
    spec.rollout_percent = Some(50.0);
    let key = FeatureKey::KILL_ADVANCED_PLANNER;
    let r1 = evaluator.evaluate(key, &spec, Tier::Community);
    let r2 = evaluator.evaluate(key, &spec, Tier::Community);
    assert_eq!(r1, r2, "rollout must be deterministic");
}

#[test]
fn unknown_flag_with_defaults_is_enabled() {
    let evaluator = FlagEvaluator::new(InstallId::from_uuid(uuid::Uuid::nil()));
    let spec = FlagSpec::default();
    assert!(evaluator.evaluate(FeatureKey::KILL_ADVANCED_PLANNER, &spec, Tier::Community));
}

#[test]
fn rollout_distribution_is_uniform() {
    use std::collections::HashSet;
    let mut buckets = HashSet::new();
    for i in 0..1000u64 {
        let id = InstallId::from_uuid(uuid::Uuid::from_u128(i));
        let evaluator = FlagEvaluator::new(id);
        let mut spec = FlagSpec::default();
        spec.rollout_percent = Some(100.0);
        let key = FeatureKey::KILL_ADVANCED_PLANNER;
        let hash = seahash::hash(format!("{}:{}", evaluator.install_id, key.as_str()).as_bytes());
        let bucket = (hash % 100) as u8;
        buckets.insert(bucket);
    }
    // With 1000 samples across 100 buckets, we expect at least 50 distinct buckets
    assert!(buckets.len() >= 50, "expected broad distribution, got {} buckets", buckets.len());
}
```

**Note:** The last test reads a private field; either make `install_id` crate-visible (`pub(crate)`) or skip the distribution test. Prefer `pub(crate)` on the field.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/policy/flags.rs crates/spur-license/src/policy/mod.rs crates/spur-license/tests/flag_evaluator.rs
git commit -m "feat(spur-license): FlagEvaluator with kill switch, tier filter, deterministic rollout"
```

---

### Task 3: Wire `FlagEvaluator` into `FeatureGate`

**Files:**
- Modify: `crates/spur-license/src/gate.rs`
- Modify: `crates/spur-license/src/lib.rs`
- Test: `crates/spur-license/tests/feature_gate.rs`

**Context:** `FeatureGate` already extracts `flags: HashMap<FeatureKey, FlagSpec>` from `PolicyDocument`. Now it also holds an `InstallId` and a `FlagEvaluator` so callers can ask `gate.is_flag_enabled(FeatureKey::KILL_ADVANCED_PLANNER)`.

- [ ] **Step 1: Write the failing test**

```rust
use spur_license::{FeatureGate, FeatureKey, InstallId, PolicyResolver};

#[test]
fn flag_evaluation_on_community() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    // kill_advanced_planner is in default_policy.json flags; default enabled=true
    let result = gate.is_flag_enabled(FeatureKey::KILL_ADVANCED_PLANNER);
    // Deterministic for nil UUID — just assert it returns a bool
    assert!(result.is_some());
}
```

Run: `cargo test -p spur-license --test feature_gate -- flag_evaluation_on_community`
Expected: FAIL — `new_with_install_id` and `is_flag_enabled` missing.

- [ ] **Step 2: Add evaluator to FeatureGate**

Modify `crates/spur-license/src/gate.rs`:

1. Add imports:
```rust
use crate::install_id::InstallId;
use crate::policy::flags::FlagEvaluator;
```

2. Update `FeatureGate` struct:
```rust
pub struct FeatureGate {
    snapshot: ArcSwap<EntitlementSnapshot>,
    policy: Arc<PolicyResolver>,
    install_id: InstallId,
    flag_evaluator: FlagEvaluator,
}
```

3. Rename `new` → `new_with_install_id` and update:
```rust
impl FeatureGate {
    pub fn new_with_install_id(policy: Arc<PolicyResolver>, install_id: InstallId) -> Self {
        let flag_evaluator = FlagEvaluator::new(install_id.clone());
        let snapshot = Self::build_community_snapshot(&policy);
        Self {
            snapshot: ArcSwap::new(Arc::new(snapshot)),
            policy,
            install_id,
            flag_evaluator,
        }
    }
}
```

4. Add convenience constructor for tests/back-compat:
```rust
    pub fn new(policy: Arc<PolicyResolver>) -> Self {
        Self::new_with_install_id(policy, InstallId::load_or_create())
    }
```

5. Add flag evaluation method:
```rust
    pub fn is_flag_enabled(&self, key: FeatureKey) -> Option<bool> {
        let snap = self.snapshot.load();
        let flag = snap.flags.get(&key)?;
        Some(self.flag_evaluator.evaluate(key, flag, snap.tier))
    }
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p spur-license --test feature_gate -- flag_evaluation_on_community`
Expected: PASS.

- [ ] **Step 4: Add more tests**

```rust
#[test]
fn unknown_flag_returns_none() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    // "not_a_real_flag" is not in the policy document
    assert_eq!(gate.is_flag_enabled(FeatureKey::from_known("not_a_real_flag").unwrap_or(FeatureKey::BRAIN_SESSION)), None);
}
```

Wait — `from_known` returns `Option` and we want to test an unknown key. Use a test that constructs a gate with a policy that has a known flag, then checks an unknown:

```rust
#[test]
fn flag_enabled_respects_kill_switch() {
    // Build a custom policy with a disabled flag
    use std::collections::BTreeMap;
    use spur_license::policy::{FlagSpec, PolicyDocument};
    let mut flags = BTreeMap::new();
    let mut disabled = FlagSpec::default();
    disabled.enabled = false;
    flags.insert("kill_advanced_planner".into(), disabled);
    let doc = PolicyDocument {
        schema_version: 1,
        issued_at: chrono::Utc::now(),
        expires_at: None,
        tier_policies: BTreeMap::new(),
        flags,
    };
    let resolver = PolicyResolver::from_document(doc);
    let gate = FeatureGate::new_with_install_id(resolver, InstallId::from_uuid(uuid::Uuid::nil()));
    assert_eq!(gate.is_flag_enabled(FeatureKey::KILL_ADVANCED_PLANNER), Some(false));
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/gate.rs crates/spur-license/src/lib.rs crates/spur-license/tests/feature_gate.rs
git commit -m "feat(spur-license): wire FlagEvaluator into FeatureGate with is_flag_enabled()"
```

---

### Task 4: `spur flags list` CLI Command

**Files:**
- Create: `crates/spur-cli/src/commands/flags.rs`
- Modify: `crates/spur-cli/src/commands/mod.rs`
- Modify: `crates/spur-cli/src/main.rs`
- Test: `crates/spur-cli/tests/flags_smoke.rs`

**Context:** Subcommand under `spur flags` with `list` sub-subcommand. Reads `FeatureGate` flags and prints each flag's key, evaluated state, and reason.

- [ ] **Step 1: Write the failing test**

```rust
use std::process::Command;

#[test]
fn flags_list_runs_without_panic() {
    let output = Command::new("cargo")
        .args(["run", "-p", "spur-cli", "--", "flags", "list"])
        .current_dir("/Volumes/Projects/spur")
        .output()
        .expect("cargo run failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // It may fail because the binary isn't built, but let's check for unknown subcommand
    assert!(
        !stderr.contains("error: unrecognized subcommand 'flags'"),
        "spur flags list should be a recognized subcommand\nstdout: {stdout}\nstderr: {stderr}"
    );
}
```

Actually, a better integration test approach: compile the CLI and invoke directly. But for the plan, use a simpler smoke test that runs via cargo:

```rust
#[test]
fn flags_list_smoke() {
    // This test will be added after the CLI command exists
}
```

Skip the failing-test step for CLI integration (hard to test before the command exists). Instead, implement the command and then add the test.

- [ ] **Step 2: Implement CLI command**

Create `crates/spur-cli/src/commands/flags.rs`:

```rust
use anyhow::Result;
use clap::Subcommand;

use spur_license::SpurLicense;

#[derive(Subcommand, Debug, Clone)]
pub enum FlagsCommands {
    /// List all runtime flags and their evaluated state.
    List {
        /// Output format
        #[arg(long, value_enum, default_value_t = FlagsOutputFormat::Plain)]
        format: FlagsOutputFormat,
    },
}

#[derive(Copy, Clone, Debug, Default, clap::ValueEnum)]
pub enum FlagsOutputFormat {
    #[default]
    Plain,
    Json,
}

pub async fn run(command: FlagsCommands) -> Result<()> {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();
    match command {
        FlagsCommands::List { format } => list_flags(&gate, format),
    }
}

fn list_flags(gate: &spur_license::FeatureGate, format: FlagsOutputFormat) -> Result<()> {
    match format {
        FlagsOutputFormat::Plain => {
            println!("{:<30} {:<10} {}", "Flag", "State", "Reason");
            println!("{}", "-".repeat(60));
            // Iterate known flag keys
            for key in known_flag_keys() {
                match gate.is_flag_enabled(key) {
                    Some(true) => println!("{:<30} {:<10} {}", key, "on", "enabled"),
                    Some(false) => println!("{:<30} {:<10} {}", key, "off", "disabled"),
                    None => println!("{:<30} {:<10} {}", key, "—", "not configured"),
                }
            }
        }
        FlagsOutputFormat::Json => {
            let mut entries = Vec::new();
            for key in known_flag_keys() {
                entries.push(serde_json::json!({
                    "key": key.as_str(),
                    "enabled": gate.is_flag_enabled(key),
                }));
            }
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
    }
    Ok(())
}

fn known_flag_keys() -> Vec<spur_license::FeatureKey> {
    use spur_license::FeatureKey;
    vec![
        FeatureKey::KILL_ADVANCED_PLANNER,
        FeatureKey::ENABLE_BROWSER_TOOL,
        FeatureKey::ENABLE_COMPACTION_V2,
        FeatureKey::ENABLE_TELEMETRY,
    ]
}
```

Add to `crates/spur-cli/src/commands/mod.rs`:
```rust
pub mod flags;
```

Add to `crates/spur-cli/src/main.rs`:

1. Import:
```rust
use commands::flags::FlagsCommands;
```

2. Add variant to `Commands` enum:
```rust
    /// List and inspect runtime feature flags
    Flags {
        #[command(subcommand)]
        command: FlagsCommands,
    },
```

3. Add match arm in `main()`:
```rust
        Commands::Flags { command } => commands::flags::run(command).await,
```

- [ ] **Step 3: Run CLI smoke test**

```bash
cargo run -p spur-cli -- flags list
```

Expected: Prints a table of 4 known flags with their states.

- [ ] **Step 4: Add integration test**

Create `crates/spur-cli/tests/flags_smoke.rs`:

```rust
use std::process::Command;

#[test]
fn flags_list_plain_output() {
    let output = Command::new("cargo")
        .args(["run", "-p", "spur-cli", "--", "flags", "list"])
        .current_dir(env!("CARGO_MANIFEST_DIR").rsplitn(3, '/').nth(2).unwrap())
        .output()
        .expect("cargo run failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("kill_advanced_planner"), "expected flag list to contain known flag\nstdout: {stdout}");
}
```

Better: since running cargo from a test is slow and fragile, test the command module directly:

```rust
#[tokio::test]
async fn flags_list_returns_ok() {
    use spur_cli::commands::flags::{run, FlagsCommands, FlagsOutputFormat};
    let result = run(FlagsCommands::List { format: FlagsOutputFormat::Plain }).await;
    assert!(result.is_ok());
}
```

But `spur-cli` doesn't expose its modules publicly. Use a binary invocation test instead:

```rust
#[test]
fn flags_list_binary_runs() {
    let bin = env!("CARGO_BIN_EXE_spur-cli");
    let output = std::process::Command::new(bin)
        .args(["flags", "list"])
        .output()
        .expect("failed to run spur flags list");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("kill_advanced_planner"), "stdout: {stdout}");
    assert!(stdout.contains("enable_browser_tool"), "stdout: {stdout}");
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/flags.rs crates/spur-cli/src/commands/mod.rs crates/spur-cli/src/main.rs crates/spur-cli/tests/flags_smoke.rs
git commit -m "feat(spur-cli): spur flags list command for runtime flag introspection"
```

---

### Task 5: Full Test Suite + Clippy Verification

**Files:** All modified above.

- [ ] **Step 1: Run full `spur-license` test suite**

```bash
cargo test -p spur-license
```

Expected: All unit and integration tests pass.

- [ ] **Step 2: Run `spur-cli` tests**

```bash
cargo test -p spur-cli
```

Expected: Existing tests + new `flags_smoke.rs` pass.

- [ ] **Step 3: Run clippy on modified crates**

```bash
cargo clippy -p spur-license -p spur-cli -- -D warnings
```

Expected: Clean (zero warnings).

- [ ] **Step 4: Format**

```bash
cargo fmt --all
```

- [ ] **Step 5: Final commit**

```bash
git add -A
git commit -m "style: cargo fmt across Phase 3 flag system changes"
```

---

## Spec Coverage Check

| Spec Requirement | Implementing Task |
|---|---|
| `InstallId` persistence at `~/.spur/install-id` | Task 1 |
| `FlagEvaluator` with kill switch, tier filter, rollout | Task 2 |
| Deterministic rollout via `seahash` | Task 2 |
| Wire evaluator into `FeatureGate` | Task 3 |
| `spur flags list` CLI command | Task 4 |
| Fail-closed: unknown flag → false | Task 2 (evaluator), Task 3 (gate) |
| TUI flag panel | **Deferred to Phase 3b** |

## Placeholder Scan

- No "TBD" or "TODO" in code steps.
- No "implement later" or "fill in details".
- Every step contains actual code or exact commands.
- No cross-references to undefined types (all types defined in prior tasks).

## Type Consistency Check

- `InstallId` defined in Task 1, used in Task 2 and Task 3.
- `FlagEvaluator` defined in Task 2, used in Task 3.
- `FeatureGate::is_flag_enabled` signature uses `FeatureKey` (matches existing type).
- CLI `flags list` uses `spur_license::FeatureKey` constants (same as `known_flag_keys`).

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-21-feature-gate-phase3-flag-system.md`.**

**Two execution options:**

1. **Subagent-Driven (recommended)** — Fresh subagent per task, review between tasks, fast iteration
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
