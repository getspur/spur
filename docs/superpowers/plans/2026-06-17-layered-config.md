# Layered (Multi-Level) SPUR Config — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax for tracking.
> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`. Each task becomes a beads issue with `spur:plan-task-id` / `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-17-layered-config-design.md`

**Goal:** Make `~/.spur/config.toml` (user) a base layer that the project
`.spur/config.toml` inherits and overrides field-by-field, across every section.

**Architecture:** Merge at the `toml::Value` level — tables deep-merge, scalars
override, arrays replace, with `agents.entries` keyed-merged by `name`. A single
`load_layered()` replaces all current load paths. Writes are sparse relative to
the layers beneath them. New `spur config show` and `spur init --global`.

**Tech Stack:** Rust 2021, `toml`, `serde`, `directories`, `anyhow`, `clap`.

> **Build/test:** ALWAYS use `scripts/spur-cargo`, never bare `cargo`
> (`scripts/spur-cargo test -p spur-acp`, `… -p spur-cli`). Lint from a sandbox
> with `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`.

---

## File Structure

- **Create** `crates/spur-acp/src/config/layered.rs` — merge engine
  (`merge_tables`, `load_layered`, `sparse_diff`, `set_key_path`,
  `effective_with_origins`). One module owns all layering logic.
- **Modify** `crates/spur-acp/src/config/mod.rs` — `pub mod layered;` + re-export.
- **Modify** `crates/spur-cli/src/main.rs` — replace `load_config_for_repo` body
  with `load_layered`; clap wiring for `config show` and `init --global`.
- **Modify** `crates/spur-cli/src/commands/pm_ingest.rs` — drop the duplicate
  loader, call `load_layered`.
- **Modify** `crates/spur-cli/src/commands/config_check.rs` — validate merged.
- **Modify** `crates/spur-cli/src/commands/config_set.rs` — value-level RMW.
- **Create** `crates/spur-cli/src/commands/config_show.rs` — `spur config show`.
- **Modify** `crates/spur-cli/src/commands/init.rs` — `--global` + sparse writes.
- **Modify** `crates/spur-cli/src/commands/mod.rs` — register `config_show`.

## Dependency DAG

```text
task-1-merge-engine
  -> task-2-sparse-and-write-helpers
  -> task-3-unify-loader
  -> task-4-config-check-merged
  -> task-5-config-show

task-2-sparse-and-write-helpers
  -> task-6-config-set-value-rmw

task-3-unify-loader, task-5-config-show
  -> task-7-init-global-sparse
```

> Note: beads caps `spur:plan-task-id:<id>` labels at 50 chars
> (prefix is 18), so task ids stay ≤ 32 chars.

`task-1` is the foundation; everything depends on it. `main.rs` is touched by
task-3, task-5, task-7 — the DAG serializes them (3 → 5 → 7) so parallel workers
never collide on it. `layered.rs` is created in task-1 and extended in task-2.

---

### Task 1: Merge engine (`merge_tables` + `load_layered`)

**Task ID:** `task-1-merge-engine`

**Files:**
- Create: `crates/spur-acp/src/config/layered.rs`
- Modify: `crates/spur-acp/src/config/mod.rs` (add `pub mod layered;`)

**Depends on:** none

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the `merge_tables` deep-merge engine, `load_layered`, their unit tests.
- OUT: any write/sparse logic (task-2), any CLI command changes (task-3+).
- If you find you must touch a `commands/*.rs` file, emit `scope_drift`.

**Acceptance Criteria:**
- [ ] `spur_acp::config::layered::load_layered(repo_root)` returns a merged
      `SpurConfig` with precedence `Default < ~/.spur < <repo>/.spur`.
- [ ] Scalars from project override user; nested tables deep-merge; plain arrays
      replace; `agents.entries` is union-by-`name` with matched entries deep-merged.
- [ ] A missing file at a layer contributes nothing; all-missing → `SpurConfig::default()`.
- [ ] A malformed file at either layer is an `Err` whose message names the path.
- [ ] Unresolvable home dir (`BaseDirs::new()` → `None`) → user layer treated as absent.
- [ ] `scripts/spur-cargo test -p spur-acp config::layered` passes.

- [ ] **Step 1: Write failing tests** in `crates/spur-acp/src/config/layered.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value;

    fn t(s: &str) -> toml::value::Table {
        match toml::from_str::<Value>(s).unwrap() {
            Value::Table(t) => t,
            _ => panic!("not a table"),
        }
    }

    #[test]
    fn scalar_override_and_table_deep_merge() {
        let mut base = t("[brain]\ndefault='claude-code'\nfallback=['kiro']\n");
        merge_tables(&mut base, t("[brain]\ndefault='codex'\n"));
        let brain = base["brain"].as_table().unwrap();
        assert_eq!(brain["default"].as_str(), Some("codex")); // overridden
        assert_eq!(brain["fallback"].as_array().unwrap().len(), 1); // inherited
    }

    #[test]
    fn plain_array_replaces() {
        let mut base = t("[brain]\nfallback=['kiro','codex']\n");
        merge_tables(&mut base, t("[brain]\nfallback=['gemini']\n"));
        let fb = base["brain"]["fallback"].as_array().unwrap();
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].as_str(), Some("gemini"));
    }

    #[test]
    fn agents_entries_merge_by_name_with_field_override() {
        let mut base = t(
            "[[agents.entries]]\nname='claude-code'\ncommand='claude'\ncapabilities=['x']\n\
             [[agents.entries]]\nname='codex'\ncommand='codex'\n",
        );
        merge_tables(
            &mut base,
            t("[[agents.entries]]\nname='claude-code'\ncapabilities=['y']\n\
               [[agents.entries]]\nname='gemini'\ncommand='gemini'\n"),
        );
        let entries = base["agents"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3); // union: claude-code, codex, gemini
        let cc = entries.iter().find(|e| e["name"].as_str() == Some("claude-code")).unwrap();
        assert_eq!(cc["command"].as_str(), Some("claude")); // inherited from user
        assert_eq!(cc["capabilities"].as_array().unwrap()[0].as_str(), Some("y")); // overridden
    }

    #[test]
    fn all_missing_is_default() {
        let dir = tempfile::tempdir().unwrap();
        // No ~/.spur and no project file under an empty repo root.
        let cfg = load_layered(dir.path()).unwrap();
        assert_eq!(cfg.brain.default, SpurConfig::default().brain.default);
    }
}
```

- [ ] **Step 2: Run to confirm they fail**
  Run: `scripts/spur-cargo test -p spur-acp config::layered`
  Expected: FAIL — `merge_tables` / `load_layered` not found.

- [ ] **Step 3: Implement the engine** at the top of `layered.rs`:

```rust
use crate::config::SpurConfig;
use anyhow::{anyhow, Result};
use std::path::Path;
use toml::value::Table;
use toml::Value;

/// Deep-merge `over` onto `base` (project onto user). Tables recurse, scalars
/// and plain arrays from `over` replace, and an `agents.entries` array is
/// merged by the `name` key (matched entries recurse).
pub fn merge_tables(base: &mut Table, over: Table) {
    merge_at(base, over, &[]);
}

fn merge_at(base: &mut Table, over: Table, path: &[&str]) {
    for (k, ov) in over {
        let is_agents_entries = path == ["agents"] && k == "entries";
        match (base.get_mut(&k), ov) {
            (Some(Value::Table(bt)), Value::Table(ot)) => {
                let mut child = path.to_vec();
                child.push(k.as_str());
                merge_at(bt, ot, &child);
            }
            (Some(Value::Array(ba)), Value::Array(oa)) if is_agents_entries => {
                merge_agents(ba, oa);
            }
            (_, ov) => {
                base.insert(k, ov);
            }
        }
    }
}

fn merge_agents(base: &mut Vec<Value>, over: Vec<Value>) {
    for ov in over {
        let oname = ov.get("name").and_then(Value::as_str).map(str::to_owned);
        let pos = oname.as_deref().and_then(|n| {
            base.iter()
                .position(|b| b.get("name").and_then(Value::as_str) == Some(n))
        });
        match (pos, ov) {
            (Some(i), Value::Table(ot)) => {
                if let Some(Value::Table(bt)) = base.get_mut(i) {
                    merge_at(bt, ot, &[]); // inner arrays (capabilities, args) replace
                } else {
                    base[i] = Value::Table(ot);
                }
            }
            (Some(i), other) => base[i] = other,
            (None, other) => base.push(other),
        }
    }
}

fn read_table(path: &Path) -> Result<Table> {
    let s = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read {}: {e}", path.display()))?;
    match toml::from_str::<Value>(&s)
        .map_err(|e| anyhow!("failed to parse {}: {e}", path.display()))?
    {
        Value::Table(t) => Ok(t),
        _ => Err(anyhow!("{} is not a TOML table", path.display())),
    }
}

/// THE config entry point: merge `Default < ~/.spur < <repo>/.spur`.
pub fn load_layered(repo_root: &Path) -> Result<SpurConfig> {
    let user_path = directories::BaseDirs::new().map(|d| d.home_dir().join(".spur/config.toml"));
    let project_path = repo_root.join(".spur").join("config.toml");

    let mut merged = Table::new();
    if let Some(up) = user_path.as_ref().filter(|p| p.exists()) {
        merge_tables(&mut merged, read_table(up)?);
    }
    if project_path.exists() {
        merge_tables(&mut merged, read_table(&project_path)?);
    }
    Value::Table(merged)
        .try_into()
        .map_err(|e| anyhow!("failed to build merged SpurConfig: {e}"))
}
```

- [ ] **Step 4: Wire the module** — in `crates/spur-acp/src/config/mod.rs` add
  `pub mod layered;` near the other `pub mod` lines, and
  `pub use layered::{load_layered, merge_tables};`. Add `tempfile` to
  `spur-acp` `[dev-dependencies]` if not already present (check first).

- [ ] **Step 5: Run tests to verify they pass**
  Run: `scripts/spur-cargo test -p spur-acp config::layered`
  Expected: PASS (4 tests).

- [ ] **Step 6: Commit**
```bash
git add crates/spur-acp/src/config/layered.rs crates/spur-acp/src/config/mod.rs crates/spur-acp/Cargo.toml
git commit -m "feat(spur-acp): task-1 toml::Value merge engine + load_layered"
```

---

### Task 2: Sparse-diff + value-level write helpers

**Task ID:** `task-2-sparse-and-write-helpers`

**Files:**
- Modify: `crates/spur-acp/src/config/layered.rs`

**Depends on:** `task-1-merge-engine`

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `sparse_diff`, `set_key_path`, and a `default_user_baseline()` helper, plus tests.
- OUT: CLI changes; do not call these from commands yet (task-6/task-7).

**Acceptance Criteria:**
- [ ] `sparse_diff(config, baseline)` returns a `Value::Table` containing only
      keys whose value differs from `baseline`; nested tables recurse; identical
      `agents.entries` entries are dropped, differing ones kept whole.
- [ ] `sparse_diff(x, x)` is an empty table.
- [ ] `set_key_path(&mut table, &["tui","theme"], Value::String("light".into()))`
      sets a nested key, creating intermediate tables, without disturbing siblings.
- [ ] `scripts/spur-cargo test -p spur-acp config::layered` passes.

- [ ] **Step 1: Write failing tests** (append to the `tests` module):

```rust
#[test]
fn sparse_diff_omits_equal_keeps_changed() {
    let base = toml::Value::try_from(SpurConfig::default()).unwrap();
    let mut t = base.clone();
    t.as_table_mut().unwrap()
        .entry("brain").or_insert(Value::Table(Default::default()))
        .as_table_mut().unwrap()
        .insert("default".into(), Value::String("codex".into()));
    let diff = sparse_diff(&t, &base);
    let dt = diff.as_table().unwrap();
    assert!(dt.contains_key("brain")); // changed → present
    assert!(!dt.contains_key("worktree")); // unchanged → omitted
    assert_eq!(diff.as_table().unwrap()["brain"]["default"].as_str(), Some("codex"));
}

#[test]
fn sparse_diff_identical_is_empty() {
    let base = toml::Value::try_from(SpurConfig::default()).unwrap();
    assert!(sparse_diff(&base, &base).as_table().unwrap().is_empty());
}

#[test]
fn set_key_path_creates_nested() {
    let mut tbl = Table::new();
    set_key_path(&mut tbl, &["tui", "theme"], Value::String("light".into()));
    assert_eq!(tbl["tui"]["theme"].as_str(), Some("light"));
}
```

- [ ] **Step 2: Run to confirm fail**
  Run: `scripts/spur-cargo test -p spur-acp config::layered`
  Expected: FAIL — `sparse_diff` / `set_key_path` not found.

- [ ] **Step 3: Implement** (append to `layered.rs`):

```rust
/// Produce a table holding only what `config` adds/changes over `baseline`.
pub fn sparse_diff(config: &Value, baseline: &Value) -> Value {
    match (config, baseline) {
        (Value::Table(c), Value::Table(b)) => {
            let mut out = Table::new();
            for (k, cv) in c {
                match b.get(k) {
                    Some(bv) if bv == cv => {} // identical → omit
                    Some(bv @ Value::Table(_)) if cv.is_table() => {
                        let d = sparse_diff(cv, bv);
                        if !d.as_table().map(Table::is_empty).unwrap_or(true) {
                            out.insert(k.clone(), d);
                        }
                    }
                    Some(Value::Array(ba)) if k == "entries" && cv.is_array() => {
                        let kept = sparse_agents(cv.as_array().unwrap(), ba);
                        if !kept.is_empty() {
                            out.insert(k.clone(), Value::Array(kept));
                        }
                    }
                    _ => {
                        out.insert(k.clone(), cv.clone());
                    }
                }
            }
            Value::Table(out)
        }
        _ => config.clone(),
    }
}

fn sparse_agents(config: &[Value], baseline: &[Value]) -> Vec<Value> {
    config
        .iter()
        .filter(|cv| {
            let name = cv.get("name").and_then(Value::as_str);
            match baseline
                .iter()
                .find(|bv| bv.get("name").and_then(Value::as_str) == name)
            {
                Some(bv) => bv != *cv, // present in baseline → keep only if changed
                None => true,          // new agent → keep
            }
        })
        .cloned()
        .collect()
}

/// Set a nested key path, creating intermediate tables, leaving siblings intact.
pub fn set_key_path(table: &mut Table, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = table;
    for p in parents {
        let entry = cur
            .entry((*p).to_string())
            .or_insert_with(|| Value::Table(Table::new()));
        if !entry.is_table() {
            *entry = Value::Table(Table::new());
        }
        cur = entry.as_table_mut().unwrap();
    }
    cur.insert((*last).to_string(), value);
}

/// Baseline used when writing the PROJECT layer: `Default` deep-merged with the
/// user layer (so a project file stays sparse vs everything beneath it).
pub fn default_user_baseline(repo_root_unused: &Path) -> Result<Value> {
    let _ = repo_root_unused;
    let mut base = match Value::try_from(SpurConfig::default())? {
        Value::Table(t) => t,
        _ => Table::new(),
    };
    let user_path = directories::BaseDirs::new().map(|d| d.home_dir().join(".spur/config.toml"));
    if let Some(up) = user_path.as_ref().filter(|p| p.exists()) {
        merge_tables(&mut base, read_table(up)?);
    }
    Ok(Value::Table(base))
}
```

- [ ] **Step 4: Run tests to verify pass**
  Run: `scripts/spur-cargo test -p spur-acp config::layered`
  Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/spur-acp/src/config/layered.rs
git commit -m "feat(spur-acp): task-2 sparse_diff + set_key_path write helpers"
```

---

### Task 3: Unify the runtime loader

**Task ID:** `task-3-unify-loader`

**Files:**
- Modify: `crates/spur-cli/src/main.rs` (replace `load_config_for_repo` body)
- Modify: `crates/spur-cli/src/commands/pm_ingest.rs` (drop the duplicate)

**Depends on:** `task-1-merge-engine`

**Suggested Worker:** codex

**Scope Boundary:**
- IN: routing both runtime loaders through `load_layered`.
- OUT: `config_check.rs`/`config_set.rs`/`init.rs` (later tasks); clap additions.
- Do not change `load_config()`'s public signature/return type.

**Acceptance Criteria:**
- [ ] `main.rs::load_config_for_repo` delegates to `spur_acp::config::load_layered`.
- [ ] The duplicated loader in `pm_ingest.rs` is removed and calls `load_layered`.
- [ ] A user-only config (no project file) is now picked up at runtime (the old
      fallback also did this — verify it still does, plus merge when both exist).
- [ ] `scripts/spur-cargo build -p spur-cli` and `scripts/spur-cargo test -p spur-cli` pass.

- [ ] **Step 1:** In `crates/spur-cli/src/main.rs`, replace the body of
  `load_config_for_repo` (the project-or-user fallback, ~lines 1657–1673) with:

```rust
fn load_config_for_repo(repo_root: &Path) -> Result<SpurConfig> {
    spur_acp::config::load_layered(repo_root)
}
```
  Keep `fn load_config()` calling `load_config_for_repo(&std::env::current_dir()?)`.

- [ ] **Step 2:** In `crates/spur-cli/src/commands/pm_ingest.rs`, delete the
  local `load_config_for_repo` copy (~lines 219–237) and replace its call site
  (~line 243) with `spur_acp::config::load_layered(&repo_root)?`.

- [ ] **Step 3:** Add/keep an integration test in
  `crates/spur-cli/tests/` (e.g. `config_layering.rs`) proving precedence:

```rust
#[test]
fn project_overrides_user_at_runtime() {
    // Build a temp HOME with ~/.spur/config.toml (brain=codex) and a repo
    // with .spur/config.toml (brain=gemini); assert load_layered picks gemini.
    // Use the `directories`-respected env (HOME on unix) via a guard, or call
    // spur_acp::config::load_layered against a repo whose .spur overrides.
}
```
  (If overriding HOME in-process is unavailable, assert the project-only and the
  merge cases through `spur_acp::config::layered::merge_tables` directly here.)

- [ ] **Step 4: Build + test**
  Run: `scripts/spur-cargo test -p spur-cli`
  Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/spur-cli/src/main.rs crates/spur-cli/src/commands/pm_ingest.rs crates/spur-cli/tests/config_layering.rs
git commit -m "feat(spur-cli): task-3 route all config loads through load_layered"
```

---

### Task 4: `config check` validates the merged config

**Task ID:** `task-4-config-check-merged`

**Files:**
- Modify: `crates/spur-cli/src/commands/config_check.rs`

**Depends on:** `task-1-merge-engine`

**Suggested Worker:** codex

**Scope Boundary:**
- IN: switching `config check` to the merged config; updating its tests.
- OUT: anything outside `config_check.rs` and its test.

**Acceptance Criteria:**
- [ ] `spur config check` loads via `load_layered` (merged), not project-only.
- [ ] It succeeds when only `~/.spur/config.toml` exists, and when only defaults
      exist (no hard error on a missing project file).
- [ ] It still exits non-zero on a fatal agent validation error in the merged roster.
- [ ] `scripts/spur-cargo test -p spur-cli config_check` passes.

- [ ] **Step 1:** Replace `config_check.rs`'s `load_spur_config` (project-only,
  hard-errors on missing) with a call to `spur_acp::config::load_layered(repo_root)`.
  Keep the existing Telegram env resolution and the
  `validate_agent_config(entry)` loop over `cfg.agents.entries` unchanged.

- [ ] **Step 2:** Update/extend the test so a missing project file with a present
  (or absent) user layer yields exit 0, and a fatal agent error still yields exit 1.

- [ ] **Step 3: Test**
  Run: `scripts/spur-cargo test -p spur-cli config_check`
  Expected: PASS.

- [ ] **Step 4: Commit**
```bash
git add crates/spur-cli/src/commands/config_check.rs
git commit -m "feat(spur-cli): task-4 config check validates the merged config"
```

---

### Task 5: `spur config show`

**Task ID:** `task-5-config-show`

**Files:**
- Create: `crates/spur-cli/src/commands/config_show.rs`
- Modify: `crates/spur-cli/src/commands/mod.rs` (`pub mod config_show;`)
- Modify: `crates/spur-cli/src/main.rs` (clap subcommand + dispatch)

**Depends on:** `task-1-merge-engine`, `task-3-unify-loader`

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the read-only `config show` command + its clap wiring + one test.
- OUT: write paths; `init`'s `--global` (task-7).
- `main.rs` collision: this runs after task-3, before task-7 (per DAG).

**Acceptance Criteria:**
- [ ] `spur config show` prints the merged effective `SpurConfig` as TOML.
- [ ] Output identifies each top-level section's origin (`project` / `user` /
      `default`) and each agent's origin by `name`.
- [ ] Read-only: writes nothing to disk. Exit 0 even with no config files.
- [ ] `scripts/spur-cargo test -p spur-cli config_show` passes.

- [ ] **Step 1:** Add `effective_with_origins` to `layered.rs`:

```rust
/// (merged config, per-top-level-section origin, per-agent-name origin).
pub fn effective_with_origins(
    repo_root: &Path,
) -> Result<(SpurConfig, std::collections::BTreeMap<String, &'static str>,
            std::collections::BTreeMap<String, &'static str>)> {
    let user_t = directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".spur/config.toml"))
        .filter(|p| p.exists())
        .map(|p| read_table(&p))
        .transpose()?;
    let proj_p = repo_root.join(".spur").join("config.toml");
    let proj_t = proj_p.exists().then(|| read_table(&proj_p)).transpose()?;

    let cfg = load_layered(repo_root)?;

    let origin = |k: &str| -> &'static str {
        if proj_t.as_ref().map(|t| t.contains_key(k)).unwrap_or(false) { "project" }
        else if user_t.as_ref().map(|t| t.contains_key(k)).unwrap_or(false) { "user" }
        else { "default" }
    };
    let merged_t = match Value::try_from(&cfg)? { Value::Table(t) => t, _ => Table::new() };
    let sections = merged_t.keys().map(|k| (k.clone(), origin(k))).collect();

    let agent_origin = |name: &str, t: &Option<Table>| -> bool {
        t.as_ref()
            .and_then(|t| t.get("agents"))
            .and_then(|a| a.get("entries"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter().any(|e| e.get("name").and_then(Value::as_str) == Some(name)))
            .unwrap_or(false)
    };
    let agents = cfg.agents.entries.iter().map(|a| {
        let o = if agent_origin(&a.name, &proj_t) { "project" }
                else if agent_origin(&a.name, &user_t) { "user" }
                else { "default" };
        (a.name.clone(), o)
    }).collect();

    Ok((cfg, sections, agents))
}
```

- [ ] **Step 2:** Create `crates/spur-cli/src/commands/config_show.rs`:

```rust
use anyhow::Result;
use std::path::Path;

pub fn run(repo_root: &Path) -> Result<()> {
    let (cfg, sections, agents) = spur_acp::config::layered::effective_with_origins(repo_root)?;
    println!("# Effective SPUR config (Default < ~/.spur < .spur)");
    println!("# section origins:");
    for (k, origin) in &sections {
        println!("#   {k:<14} <- {origin}");
    }
    if !agents.is_empty() {
        println!("# agent origins:");
        for (name, origin) in &agents {
            println!("#   {name:<18} <- {origin}");
        }
    }
    println!();
    print!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}
```

- [ ] **Step 3:** Register in `commands/mod.rs` (`pub mod config_show;`) and in
  `main.rs` add a `Show` variant to the `config` subcommand enum and dispatch
  `commands::config_show::run(&repo_root)`. Mirror how `Check` is wired.

- [ ] **Step 4:** Add `crates/spur-cli/tests/config_show.rs` asserting the
  command exits 0 and prints `# section origins:` against a temp repo.

- [ ] **Step 5: Test**
  Run: `scripts/spur-cargo test -p spur-cli config_show`
  Expected: PASS.

- [ ] **Step 6: Commit**
```bash
git add crates/spur-cli/src/commands/config_show.rs crates/spur-cli/src/commands/mod.rs crates/spur-cli/src/main.rs crates/spur-acp/src/config/layered.rs crates/spur-cli/tests/config_show.rs
git commit -m "feat(spur-cli): task-5 add spur config show with per-section origins"
```

---

### Task 6: `config set` value-level RMW (preserve sparseness)

**Task ID:** `task-6-config-set-value-rmw`

**Files:**
- Modify: `crates/spur-cli/src/commands/config_set.rs`

**Depends on:** `task-2-sparse-and-write-helpers`

**Suggested Worker:** codex

**Scope Boundary:**
- IN: making `config set` mutate at the `toml::Value` level via `set_key_path`.
- OUT: changing the set of supported keys (`tui.*`) or `--global` resolution.

**Acceptance Criteria:**
- [ ] `spur config set tui.theme light` writes only that key; a previously
      sparse file stays sparse (no re-expansion to a fully-defaulted config).
- [ ] Existing supported keys still work; unknown keys still error.
- [ ] `--global` still targets `~/.spur/config.toml`; default targets the project file.
- [ ] `scripts/spur-cargo test -p spur-cli config_set` passes.

- [ ] **Step 1: Add a failing test** proving no re-expansion: write a sparse file
  containing only `[tui]\ntheme='dark'`, run set `tui.disable_paste_burst true`,
  then assert the file does NOT contain `[brain]` / `[worktree]` (i.e. it was not
  re-expanded to a full default config).

- [ ] **Step 2:** Replace the `update_config(&target, |c| ...)` RMW with a
  value-level read-modify-write:

```rust
let mut table = if target.exists() {
    match toml::from_str::<toml::Value>(&std::fs::read_to_string(&target)?)? {
        toml::Value::Table(t) => t,
        _ => toml::value::Table::new(),
    }
} else {
    toml::value::Table::new()
};
let value = /* parse `value` into the right toml::Value per key, as today */;
spur_acp::config::layered::set_key_path(&mut table, &path_segments, value);
// atomic write (NamedTempFile + persist + fsync), same as before
```
  Keep the existing key→type parsing (`edit_mode`, `disable_paste_burst`, `theme`)
  and the `resolve_target_path(repo_root, global)` logic. `path_segments` is the
  key split on `.` (e.g. `["tui","theme"]`).

- [ ] **Step 3: Test**
  Run: `scripts/spur-cargo test -p spur-cli config_set`
  Expected: PASS.

- [ ] **Step 4: Commit**
```bash
git add crates/spur-cli/src/commands/config_set.rs
git commit -m "feat(spur-cli): task-6 config set mutates at toml::Value level"
```

---

### Task 7: `spur init --global` + sparse writes

**Task ID:** `task-7-init-global-sparse`

**Files:**
- Modify: `crates/spur-cli/src/commands/init.rs`
- Modify: `crates/spur-cli/src/main.rs` (add `--global` to `init` clap args + pass through)

**Depends on:** `task-2-sparse-and-write-helpers`, `task-5-config-show`

**Suggested Worker:** codex

**Scope Boundary:**
- IN: a `global: bool` param on `init::run`; sparse serialization on write;
      `--global` clap flag.
- OUT: changing discovery/merge/recompute logic in `init.rs` (keep `merge_agents`,
      `recompute_brain_and_fallback`, the three yes/no steps).
- `main.rs` collision: runs after task-5 per DAG.

**Acceptance Criteria:**
- [ ] `spur init --global` targets `~/.spur/config.toml`; plain `spur init`
      targets `<repo>/.spur/config.toml` (unchanged default location).
- [ ] The written file is **sparse**: `--global` writes `sparse_diff(config, Default)`;
      project writes `sparse_diff(config, Default ⊕ user)`. Unchanged sections are absent.
- [ ] A project `spur init` with a populated user layer and no project-specific
      settings writes a near-empty file (no full default dump).
- [ ] `load_layered` of the written file reproduces the intended effective config.
- [ ] `scripts/spur-cargo test -p spur-cli init` and the existing `init_ux` tests pass.

- [ ] **Step 1:** Add `global: bool` to `pub async fn run(...)` in `init.rs` and a
  `--global` flag on the `Init` clap command in `main.rs`, threading it through.

- [ ] **Step 2:** Compute the target path:

```rust
let config_path = if global {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".spur").join("config.toml"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory for --global"))?
} else {
    repo_root.join(".spur").join("config.toml")
};
```
  When `global`, load the existing user file (not the project file) as the
  starting point for `load_or_default_config`.

- [ ] **Step 3:** Replace the persist (`std::fs::write(&config_path,
  toml::to_string_pretty(&config)?)`) with a sparse write:

```rust
let baseline = if global {
    toml::Value::try_from(&SpurConfig::default())?
} else {
    spur_acp::config::layered::default_user_baseline(&repo_root)?
};
let full = toml::Value::try_from(&config)?;
let sparse = spur_acp::config::layered::sparse_diff(&full, &baseline);
std::fs::create_dir_all(config_path.parent().unwrap())?;
std::fs::write(&config_path, toml::to_string_pretty(&sparse)?)?;
```

- [ ] **Step 4:** Add a test: with a fake user layer defining all agents, a
  project `init` (assume_yes) writes a file whose parsed table lacks `worktree`,
  `cost`, etc., and `load_layered` of the result still yields those agents.

- [ ] **Step 5: Test**
  Run: `scripts/spur-cargo test -p spur-cli init`
  Expected: PASS (including `init_ux`).

- [ ] **Step 6:** Final lint:
  Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`
  Expected: clean.

- [ ] **Step 7: Commit**
```bash
git add crates/spur-cli/src/commands/init.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): task-7 spur init --global + sparse layer writes"
```

---

## Self-Review

- **Spec coverage:** merge algebra (task-1), sparse writes (task-2/7), loader
  unification (task-3), `config check` merged (task-4), `config show` (task-5),
  `config set` no-re-expand (task-6), `init --global` (task-7), error-on-malformed
  + home-unresolvable (task-1 AC). All spec sections map to a task.
- **Out-of-scope** items (env layer, `SPUR_CONFIG`, comment preservation, theme
  `$HOME` migration) are not in any task — correct.
- **Type consistency:** `merge_tables`, `load_layered`, `sparse_diff`,
  `set_key_path`, `default_user_baseline`, `effective_with_origins` are defined in
  task-1/2/5 and referenced with matching signatures in task-3/4/5/6/7.
