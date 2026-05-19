# Upgrade Check Implementation Plan

> **For agentic workers:** Tasks below correspond 1:1 with `submit_plan` task IDs. Spec: `docs/superpowers/specs/2026-05-19-upgrade-check-design.md` (v3). Follow TDD: write failing test → minimal impl → green → commit. Each task ends with green `cargo test -p spur-cli` (or scoped equivalent) and a single commit.

**Goal:** Ship a passive npm-registry upgrade banner on TUI startup for `spur-cli` distributed via `@getspur/spur-cli`, plus a `spur upgrade` subcommand with install-source-aware guidance, per spec v3.

**Architecture:** New `upgrade_check` module in `spur-cli` that performs an async, non-blocking GET against `registry.npmjs.org` (reusing workspace `reqwest`), caches the result at `~/.spur/cache/upgrade-check.json` via the existing `directories` workspace dep, and surfaces an available upgrade to the TUI via a `tokio::sync::oneshot` channel. The TUI consumes the receiver from its phase-1 select loop and reuses the existing `user_warning` banner. Cache writes (including `last_notified_at`) happen entirely inside the module — the TUI is read-only on cache state. A new `Commands::Upgrade` subcommand detects install source via canonical `current_exe()` path matching and runs/prints the appropriate package-manager command.

**Tech stack:** Rust 2021, tokio (existing), reqwest+rustls (workspace), semver (new workspace), serde/serde_json (workspace), directories (workspace), tracing (workspace), wiremock + tempfile (dev-deps, already present in `spur-telemetry`).

---

## File map

```
crates/spur-cli/
├── Cargo.toml                                  # T1 (add reqwest, semver, time/chrono if needed)
└── src/
    ├── upgrade_check.rs                        # T2-T6 (new module)
    ├── upgrade_check/
    │   ├── cache.rs                            # T3 (atomic R/W, schema)
    │   ├── registry.rs                         # T4 (npm GET + dist-tags)
    │   ├── install_source.rs                   # T5 (9-step detection table)
    │   └── tests.rs                            # T6 (unit tests)
    ├── cmd/
    │   └── upgrade.rs                          # T7 (Commands::Upgrade)
    └── main.rs                                 # T8 (wire-in + is_non_interactive)
crates/spur-tui/src/app/
├── mod.rs                                      # T9 (App holds Option<Receiver>)
└── events.rs                                   # T9 (select! arm)
crates/spur-cli/tests/upgrade_check_e2e.rs      # T10 (wiremock integration)
Cargo.toml                                      # T1 (promote semver to workspace)
```

---

## Task DAG summary

```
T1 (workspace deps + crate deps)
   │
   ├─> T2 (module skeleton + UpgradeInfo/InstallSource types)
   │     │
   │     ├─> T3 (cache R/W + schema + atomic rename)
   │     ├─> T4 (npm registry GET + dist-tags fallback)
   │     └─> T5 (install_source detection)
   │           │
   │           └─> T6 (check_for_upgrade orchestration + unit tests)
   │                 │
   │                 ├─> T7 (spur upgrade subcommand)
   │                 ├─> T8 (main.rs wire-in + is_non_interactive guard)
   │                 │     │
   │                 │     └─> T9 (TUI receiver + user_warning render)
   │                 │
   │                 └─> T10 (e2e wiremock test: stale→fetch→banner→suppress-for-3d)
```

T7, T8, T9 can run in parallel after T6; T10 fans in.

---

## Task 1 — Dependency setup

**Files:**
- Modify: workspace `Cargo.toml` — add `semver = "1"` to `[workspace.dependencies]`.
- Modify: `crates/spur-cli/Cargo.toml` — add `reqwest = { workspace = true }`, `semver = { workspace = true }`, `serde_json = { workspace = true }` (if not present), and verify `directories`, `tracing`, `tokio`, `serde` are already there.
- Inspect (read-only): `crates/spur-pm/Cargo.toml` — its direct `semver = "1"` is unused (per codex review); leave as-is for now, flag for cleanup in a follow-up issue.

**Goal:** `cargo check -p spur-cli` passes; new deps available to subsequent tasks.

**Tests:** `cargo check -p spur-cli` and `cargo check --workspace`.

**Commit:** `feat(spur-cli): add reqwest+semver deps for upgrade check`

---

## Task 2 — Module skeleton + types

**Files:**
- Create: `crates/spur-cli/src/upgrade_check.rs` (module root, declares `mod cache; mod registry; mod install_source;` and re-exports).
- Modify: `crates/spur-cli/src/main.rs` — add `mod upgrade_check;` near other module declarations.

**Goal:** Define the public types and stub the public functions to compile against:

```rust
pub struct UpgradeInfo {
    pub current: semver::Version,
    pub latest:  semver::Version,
    pub install_source: InstallSource,
}

pub enum InstallSource { Volta, Asdf, Fnm, Pnpm, Bun, Homebrew, Npm, Cargo, Unknown }

pub fn upgrade_check_disabled() -> bool { /* SPUR_NO_UPGRADE_CHECK || NO_UPDATE_NOTIFIER */ }
pub fn cache_path() -> Option<PathBuf>   { /* ~/.spur/cache/upgrade-check.json via `directories` */ }
pub async fn check_for_upgrade(cache_path: &Path) -> Option<UpgradeInfo> { None /* stubbed */ }
```

**Tests:**
- Unit: `upgrade_check_disabled()` returns true when either env var is set to `1`, `true`, anything non-empty (decide and document); false otherwise.
- Unit: `cache_path()` returns `Some(path)` ending with `.spur/cache/upgrade-check.json` (assert via `ends_with`).
- `cargo build -p spur-cli` succeeds.

**Commit:** `feat(spur-cli): upgrade_check module skeleton + types`

---

## Task 3 — Cache R/W

**Files:**
- Create: `crates/spur-cli/src/upgrade_check/cache.rs`.

**Goal:** Schema, atomic write, lenient read.

```rust
#[derive(Serialize, Deserialize)]
pub(crate) struct CacheV1 {
    pub version: u32,         // == 1
    pub checked_at:        DateTime<Utc>,
    pub last_notified_at:  DateTime<Utc>,
    pub current: String,      // semver
    pub latest:  String,      // semver
}

pub(crate) fn read(path: &Path) -> Option<CacheV1>;             // None on any failure
pub(crate) fn write(path: &Path, cache: &CacheV1) -> io::Result<()>;  // atomic via sibling tmp + rename
```

Write strategy: serialize to `path.with_extension(format!("json.tmp.{}", std::process::id()))`, then `std::fs::rename` to `path`. Create parent dir with `create_dir_all` first; ignore `AlreadyExists`.

**Tests (unit, using `tempfile::tempdir` from dev-deps):**
- Roundtrip: write a `CacheV1`, read it back, assert equality.
- Missing file → `read` returns `None`, no panic.
- Malformed JSON → `read` returns `None`, logs `warn!`.
- Wrong `version` field → `read` returns `None`.
- Atomic write: rapid successive writes don't corrupt the file.
- Write to non-existent parent directory creates it.

**Commit:** `feat(spur-cli): upgrade_check cache R/W with atomic rename`

---

## Task 4 — npm registry client

**Files:**
- Create: `crates/spur-cli/src/upgrade_check/registry.rs`.

**Goal:** Two async functions with 2s total timeout, no panics:

```rust
pub(crate) async fn fetch_latest(client: &reqwest::Client) -> Option<semver::Version>;
pub(crate) async fn fetch_dist_tags(client: &reqwest::Client) -> Option<DistTags>; // { latest, beta?, next? }
```

Endpoints:
- `fetch_latest` → `GET https://registry.npmjs.org/@getspur/spur-cli/latest`, parse `{ "version": "..." }`.
- `fetch_dist_tags` → `GET https://registry.npmjs.org/@getspur/spur-cli`, parse `dist-tags` object. Tolerate missing `beta` and `next` per spec §5.

Use `reqwest::Client::builder().timeout(Duration::from_secs(2))`. All errors (network, 4xx/5xx, parse, missing field) → `debug!` log, return `None`.

**Tests (using `wiremock`):**
- 200 with `{ "version": "1.2.0" }` → `Some(Version::new(1,2,0))`.
- 200 with malformed JSON → `None`.
- 200 missing `version` field → `None`.
- 5xx → `None`.
- Timeout simulation (wiremock `set_delay(5s)` against 2s timeout) → `None`.
- `dist-tags` with only `latest` → `DistTags { latest, beta: None, next: None }`.
- `dist-tags` with `latest` + `beta` → both populated.

**Commit:** `feat(spur-cli): npm registry client for upgrade check`

---

## Task 5 — Install-source detection

**Files:**
- Create: `crates/spur-cli/src/upgrade_check/install_source.rs`.

**Goal:** Implement the 9-step ordered detection per spec §4:

```rust
pub fn detect() -> InstallSource;
fn detect_from_path(canonical: &Path, env: &impl EnvProvider) -> InstallSource;  // testable
```

Use `std::env::current_exe()` + `std::fs::canonicalize` to get the canonical path. Match against the spec's path-substring table in order. For step 7 (npm), shell out to `npm prefix -g` with a 1s timeout via `tokio::process::Command` only when path-based detection has failed AND `npm` is on `PATH`. Wrap env access in an `EnvProvider` trait for unit-testability.

**Tests (unit, using a mock `EnvProvider` and synthetic paths):**
- Each of Volta/asdf/fnm/pnpm/bun/Homebrew (Apple Silicon + Intel)/cargo paths → correct variant.
- asdf with custom `$ASDF_DATA_DIR` not under `~/.asdf/` → `Asdf`.
- Path matches *no* pattern → `Unknown`.
- `npm_execpath` env set → `Npm`.
- Order precedence: a path that matches both Volta and the generic npm-prefix pattern resolves to `Volta`.

**Commit:** `feat(spur-cli): install-source detection for upgrade subcommand`

---

## Task 6 — `check_for_upgrade` orchestration

**Files:**
- Modify: `crates/spur-cli/src/upgrade_check.rs` — fill in `check_for_upgrade`.
- Create: `crates/spur-cli/src/upgrade_check/tests.rs` (or `#[cfg(test)]` module).

**Goal:** Implement the algorithm from spec §3.1:

1. Read cache. If parse failure or missing, treat as cold.
2. Determine current version from `env!("CARGO_PKG_VERSION")`.
3. If cache age < `CHECK_INTERVAL` (24h), use cached `latest`; else hit registry. If current is a pre-release, also fetch `dist-tags`. Update cache `checked_at` + `latest` on successful fetch.
4. Compute candidate `latest`: max of `dist-tags.latest`, and (if current is pre-release) `dist-tags.beta`/`next` when present.
5. If candidate > current AND `now - last_notified_at >= NOTIFY_INTERVAL` (3d): write `last_notified_at = now` to cache, call `install_source::detect()`, return `Some(UpgradeInfo)`. Otherwise `None`.
6. Any error → `None`.

Constants: `CHECK_INTERVAL = Duration::from_secs(24 * 3600)`, `NOTIFY_INTERVAL = Duration::from_secs(3 * 24 * 3600)`.

**Tests (unit with a `tempdir` cache + wiremock):**
- Cold cache + stable current `1.0.0` + npm `latest=1.1.0` → `Some(UpgradeInfo)`, cache populated.
- Warm cache (<24h, latest≤current) → no network call, `None`.
- Warm cache (<24h, latest>current, last_notified <3d ago) → `None`.
- Warm cache (<24h, latest>current, last_notified >3d ago) → `Some(_)`, `last_notified_at` bumped.
- Current is pre-release + beta dist-tag higher → `Some(_)` with beta version.
- Current is pre-release + no beta dist-tag → falls back to latest comparison only.
- Network failure on cold cache → `None`, no cache write.
- Downgrade scenario (latest<current) → `None`, log at `debug!`.

**Commit:** `feat(spur-cli): check_for_upgrade orchestration + tests`

---

## Task 7 — `spur upgrade` subcommand

**Files:**
- Create: `crates/spur-cli/src/cmd/upgrade.rs`.
- Modify: `crates/spur-cli/src/main.rs` — add `Commands::Upgrade { check: bool, force: bool }` to the clap enum, dispatch to the handler.

**Goal:** Implement `spur upgrade [--check] [--force]` per spec §4:

- `--check`: print current/latest + detected install source. No mutation.
- Default: print plan, prompt `y/N` (skip prompt if `--force`), then `Command::spawn` the install command and wait, propagating exit code.
- For `Cargo`/`Unknown` install sources: print guidance, exit 0 without prompting or running anything.
- The install command for each source is fully specified by the §4 table (e.g. `Asdf` → `npm install -g @getspur/spur-cli@latest && asdf reshim nodejs`).

**Tests:**
- Unit: handler with `--check` against a stubbed `UpgradeInfo` produces the expected stdout.
- Unit: handler with `Cargo` source prints guidance + exit 0, never invokes the spawn.
- Integration (manual smoke, not automated): run `spur upgrade --check` on the dev machine; verify install-source detection matches reality.

**Commit:** `feat(spur-cli): spur upgrade subcommand with install-source dispatch`

---

## Task 8 — main.rs wire-in + non-interactive guard

**Files:**
- Modify: `crates/spur-cli/src/main.rs` — add `is_non_interactive()` helper and the spawn block just before `run_tui_with_license` at ~line 1120.

**Goal:** Per spec §3.2 and §7:

```rust
fn is_non_interactive() -> bool {
    // Match spur-telemetry's CI rule: any non-empty, non-"false" CI value.
    let ci_set = std::env::var("CI").ok()
        .map(|v| !v.is_empty() && v.to_ascii_lowercase() != "false")
        .unwrap_or(false);
    ci_set || !std::io::stdout().is_terminal()
}

let upgrade_rx = if !upgrade_check::upgrade_check_disabled() && !is_non_interactive() {
    match upgrade_check::cache_path() {
        Some(path) => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move { let _ = tx.send(upgrade_check::check_for_upgrade(&path).await); });
            Some(rx)
        }
        None => None,
    }
} else { None };
```

Then pass `upgrade_rx` through `run_tui_with_license` (signature change) into the TUI app constructor.

Inspect `spur-telemetry/src/config.rs` (or wherever it lives) for the canonical CI-detection helper; if it's `pub`, **reuse it** instead of duplicating the rule.

**Tests:**
- Unit: `is_non_interactive()` returns true when `CI=1`, `CI=true`, `CI=anything`, and false when `CI=false` or unset (mock env). TTY branch tested manually.
- `cargo check --workspace` clean after signature change to `run_tui_with_license`.

**Commit:** `feat(spur-cli): wire upgrade_check into TUI bootstrap`

---

## Task 9 — TUI receiver + banner render

**Files:**
- Modify: `crates/spur-tui/src/app/mod.rs` — `App` struct holds `upgrade_rx: Option<oneshot::Receiver<Option<UpgradeInfo>>>`. Constructor takes it as a parameter.
- Modify: `crates/spur-tui/src/app/events.rs` near line 681 — add the select-arm per spec §3.3.

**Goal:** Implement the exact pattern from spec §3.3:

```rust
result = async {
    match self.upgrade_rx.as_mut() {
        Some(rx) => rx.await,
        None => std::future::pending().await,
    }
} => {
    self.upgrade_rx = None;
    if let Ok(Some(info)) = result {
        self.show_user_warning(format!(
            "SPUR {} is available; current {}. Run: spur upgrade",
            info.latest, info.current
        ));
    }
}
```

The `UpgradeInfo` import needs to cross the crate boundary — either re-export from `spur-cli` to `spur-tui` (preferred via a small shared type in a workspace crate, e.g. `spur-core` if it exists), or define a minimal `UpgradeBanner { current: String, latest: String }` in `spur-tui` and convert in `spur-cli`'s wire-in code. Pick whichever is cleaner during implementation; document the choice in the commit message.

**Tests:**
- Unit: app constructed with `upgrade_rx = None` runs the loop without firing the arm (smoke).
- Unit: with a manually-fired `oneshot::Sender::send(Some(UpgradeInfo { ... }))`, the next loop tick calls `show_user_warning` exactly once and clears `upgrade_rx`.
- Manual smoke: forge a cache file claiming `latest = 999.0.0`, launch `spur tui`, see the banner.

**Commit:** `feat(spur-tui): render upgrade-available banner from oneshot receiver`

---

## Task 10 — End-to-end integration test

**Files:**
- Create: `crates/spur-cli/tests/upgrade_check_e2e.rs`.

**Goal:** Black-box test the full pipeline against a wiremock npm registry. Three scenarios:

1. **Cold cache, upgrade available:** wiremock serves `{ "version": "999.0.0" }`. `check_for_upgrade` returns `Some(UpgradeInfo { latest: 999.0.0, ... })`. Cache file written with both `checked_at` and `last_notified_at` set to ~now.
2. **Warm cache, suppressed by NOTIFY_INTERVAL:** pre-seed cache with `last_notified_at = now - 1d`. Same wiremock response. `check_for_upgrade` returns `None`. Cache unchanged on disk (or `checked_at` may update but `last_notified_at` must not).
3. **Cache expired (>24h), latest unchanged:** pre-seed cache with `checked_at = now - 2d, latest = current`. Wiremock serves the same version as current. `check_for_upgrade` returns `None`, cache `checked_at` is bumped to ~now.

Use `wiremock::MockServer` and point the registry base URL via an env-driven override (add a `pub(crate) fn registry_base() -> String { env::var("SPUR_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into()) }` in `registry.rs`; document as test-only override).

**Tests:** This task IS the test — it must pass.

**Commit:** `test(spur-cli): e2e upgrade check pipeline against wiremock registry`

---

## Out-of-scope follow-ups

These are explicitly NOT in this plan; file as separate issues if needed:

1. `spur-pm`'s unused `semver = "1"` direct dep → cleanup issue.
2. Telemetry event (`cli_startup.upgrade_available`) per spec §8 — wait until the rest is shipped and gather usage signal first.
3. `spur self-update` that downloads the binary directly without going through npm — spec §12 open question; design later if there's demand.
4. Windows atomic-rename smoke test — gate on the decision to ship a Windows build.

---

## Verification gate

Before merging:

- `cargo test -p spur-cli` green (includes T6 unit tests and T10 e2e).
- `cargo test -p spur-tui` green.
- `cargo clippy --workspace -- -D warnings` clean.
- Manual smoke: launch `spur tui` with no network — banner does not appear, startup is not delayed.
- Manual smoke: launch `spur tui` with forged stale cache claiming `latest = 999.0.0` — banner appears within ~1s.
- Manual smoke: dismiss banner with Esc, relaunch within 3 days — no banner (suppression works).
- `SPUR_NO_UPGRADE_CHECK=1 spur tui` and `NO_UPDATE_NOTIFIER=1 spur tui` both skip the check (no network traffic, verified with `tcpdump` or similar).
