# Upgrade Check — Design Spec

**Status:** Approved for planning (v3 — incorporates codex second-pass review)
**Date:** 2026-05-19 (v3: 2026-05-19, same day)
**Owner:** Kevin Truong
**Crates touched:** `spur-cli` (new module + subcommand), `spur-tui` (banner receiver), workspace `Cargo.toml` (promote `semver`)
**Distribution context:** SPUR ships as a Rust binary wrapped by the npm package `@getspur/spur-cli`.

---

## 1. Goal

On TUI startup, detect when a newer `@getspur/spur-cli` is published on npm and surface it to the user **without blocking startup**, without breaking offline/CI, and without nagging on every launch.

Non-goals:
- Auto-applying upgrades. The check only **notifies**; `spur upgrade` is a separate, explicit subcommand.
- Notifying for non-TUI invocations (one-shot commands, scripted use, CI).
- Handling cargo-installed builds as a first-class upgrade path — we detect and print guidance, we do not mutate.

## 2. UX decision

**Passive top-row banner. No blocking prompt.**

This matches what gemini-cli, claude-code, vercel, and wrangler all do. A blocking y/n on startup adds network latency to the first screen, fails closed when offline, breaks any scripted/CI usage, and offers no benefit over a passive banner since the user must run `npm i -g` (or equivalent) out-of-band anyway.

The banner reuses the existing top-row `user_warning` surface in `spur-tui` (`crates/spur-tui/src/app/mod.rs:586`). Text format:

```
SPUR 1.2.0 is available; current 1.1.16. Run: spur upgrade
```

The banner is dismissible via the existing `user_warning` clear keybind. To keep the TUI from needing a cache handle, **`last_notified_at` is bumped at banner-render time** (not on dismissal): immediately before calling `show_user_warning`, the spawn-side code (or a small helper) writes the cache with `last_notified_at = now`. On subsequent launches `check_for_upgrade` consults `last_notified_at` and suppresses the banner if `now - last_notified_at < NOTIFY_INTERVAL` (3 days). Matches vercel's `notifyInterval`. The TUI side never touches the cache.

## 3. Architecture

### 3.1 New module: `crates/spur-cli/src/upgrade_check.rs`

Public surface:

```rust
pub struct UpgradeInfo {
    pub current: semver::Version,
    pub latest:  semver::Version,
    pub install_source: InstallSource, // Npm | Volta | Asdf | Fnm | Pnpm | Bun | Homebrew | Cargo | Unknown
}

pub fn upgrade_check_disabled() -> bool;        // honors SPUR_NO_UPGRADE_CHECK and NO_UPDATE_NOTIFIER
pub fn cache_path() -> Option<PathBuf>;         // ~/.spur/cache/upgrade-check.json via `directories`
pub async fn check_for_upgrade(cache_path: &Path) -> Option<UpgradeInfo>;
```

All three functions are infallible from the caller's perspective: `cache_path()` returns `None` (not `Result`) on resolution failure so the caller never has to handle errors that would block startup.

`check_for_upgrade` is the only async entry point. It:

1. Reads cache at `cache_path`. Two TTLs apply: `CHECK_INTERVAL` (24h) gates re-fetching from npm; `NOTIFY_INTERVAL` (3d) gates whether to return `Some(_)` even when a newer version is known.
2. If cache age < `CHECK_INTERVAL`, skip the network call and use cached `latest`. Otherwise, GET `https://registry.npmjs.org/@getspur/spur-cli/latest` (and `/@getspur/spur-cli` for `dist-tags` if current is pre-release — see §5) with a 2s total timeout, parse `{ "version": "..." }`, update the cache.
3. If `latest > current` (semver-aware): return `Some(UpgradeInfo)` **iff** `now - last_notified_at >= NOTIFY_INTERVAL`. When returning `Some`, write `last_notified_at = now` to the cache atomically so subsequent launches stay silent for 3 days. The TUI never writes the cache; suppression lives entirely inside this module.
4. Any failure (network, parse, JSON shape, fs) → `debug!` log, return `None`. **Never blocks, never panics, never surfaces to user.**

### 3.2 Wire-in: `crates/spur-cli/src/main.rs`

The TUI branch starts at `main.rs:898`, but most of the bootstrap (config load, onboarding, singleton lock, PM service, orchestrator spawn, translation task, warm/resume task) happens between that branch entry and the `run_tui_with_license` call at `main.rs:1120`. Insert the spawn **immediately before** the `run_tui_with_license` call — after the bootstrap is complete but before the TUI takes over the terminal:

```rust
// Around main.rs:1120, just before `run_tui_with_license(...)`.
let upgrade_rx = if !upgrade_check::upgrade_check_disabled() && !is_non_interactive() {
    match upgrade_check::cache_path() {
        Some(path) => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _ = tx.send(upgrade_check::check_for_upgrade(&path).await);
            });
            Some(rx)
        }
        None => None, // cache dir resolution failed; silently skip
    }
} else {
    None
};
```

`upgrade_rx: Option<oneshot::Receiver<Option<UpgradeInfo>>>` is passed through `run_tui_with_license` into the TUI app constructor. **No `?` operator** — any failure to set up the check must silently degrade to "no banner", never abort startup.

### 3.3 TUI receiver: `crates/spur-tui/src/app/events.rs:681`

Note: `events.rs:681` is the event loop's **first wait** (phase-1 `tokio::select!`), followed by explicit non-blocking drain phases. The upgrade arm sits in this first wait without affecting drain semantics.

**Ownership lifecycle** — the app struct holds the receiver as an `Option<oneshot::Receiver<Option<UpgradeInfo>>>`. The select-arm pattern:

```rust
// In the phase-1 select! at events.rs:681.
result = async {
    match self.upgrade_rx.as_mut() {
        Some(rx) => rx.await,
        None => std::future::pending().await, // never resolves; arm is inert
    }
} => {
    self.upgrade_rx = None; // one-shot; never poll again
    if let Ok(Some(info)) = result {
        self.show_user_warning(format!(
            "SPUR {} is available; current {}. Run: spur upgrade",
            info.latest, info.current
        ));
    }
}
```

When the receiver is `None` (opt-out, non-TTY, cache-dir failure), the arm parks on `pending()` and never fires — no busy-loop, no starvation of other arms. After the arm fires once, `self.upgrade_rx = None` ensures it parks forever after.

### 3.4 HTTP

Use workspace `reqwest` already pinned in root `Cargo.toml` and used by `spur-telemetry/src/client.rs:37` with a 2s timeout. Add `reqwest = { workspace = true }` to `crates/spur-cli/Cargo.toml`. **No new HTTP dependency.**

### 3.5 SemVer

Root `Cargo.toml` does **not** currently expose `semver` as a workspace dep. `spur-pm/Cargo.toml` declares `semver = "1"` but `rg semver crates/spur-pm/src` finds no source usage — likely dead. Plan:

1. Add `semver = "1"` to root `[workspace.dependencies]`.
2. Add `semver = { workspace = true }` to `crates/spur-cli/Cargo.toml`.
3. Either retire `spur-pm`'s direct dep in favor of the workspace one, or remove it if confirmed dead (out of scope for this spec — flag as cleanup).

Do not hand-roll comparison.

### 3.6 Cache schema

**Location: `~/.spur/cache/upgrade-check.json`** — matches existing SPUR convention. The codebase already uses `~/.spur/` for config, onboarding state, crash reports, and `~/.spur/cache/` for TUI analytics. Resolve via the workspace `directories` crate (already a dep), not `dirs::cache_dir()`, to match how the rest of the codebase resolves user state.

```json
{
  "version": 1,
  "checked_at":      "2026-05-19T13:20:00Z",
  "last_notified_at": "2026-05-19T13:20:00Z",
  "current":  "1.1.16",
  "latest":   "1.2.0"
}
```

**Atomic write strategy.** `tempfile` is currently a dev-dep only in `spur-cli`. Rather than promoting it to a runtime dep just for this, write to a sibling path (`upgrade-check.json.tmp.<pid>`) then `std::fs::rename` to the final path. Same-filesystem atomicity is guaranteed on Unix (macOS APFS, Linux ext4/xfs/btrfs). On Windows, `std::fs::rename` over an existing target is supported since Rust 1.62 but has historically been platform-quirky; if/when SPUR ships a Windows build, add a smoke test that exercises this path. The cache is non-critical state — a failed write degrades to "ask npm again next launch", never blocks. Parse failures → log `warn!` and overwrite on next successful check.

## 4. `spur upgrade` subcommand

```
spur upgrade [--check] [--force]
```

- Without flags: detects install source, then runs the appropriate command **after confirming with the user** (`y/N` prompt). This y/n is appropriate here because the user explicitly invoked `spur upgrade`.
- `--check`: prints current/latest, does not mutate.
- `--force`: skips confirmation. Useful for scripted updates.

**Install-source detection (`InstallSource`).** The npm-env-var heuristic from v1 is insufficient — those vars (`npm_execpath`, `NPM_CONFIG_PREFIX`) are only set during an active npm process, not when the user runs the installed `spur` binary directly. Real detection must inspect `std::env::current_exe()` (resolving symlinks via `std::fs::canonicalize`) and pattern-match its path against known package-manager store layouts:

| Detection order | Signal | Verdict |
|---|---|---|
| 1 | Canonical path contains `/.volta/tools/image/packages/@getspur/` | `Volta` → `volta install @getspur/spur-cli@latest` |
| 2 | Canonical path contains `/installs/nodejs/` **and** `/lib/node_modules/@getspur/` (whether under `~/.asdf/` or a custom `$ASDF_DATA_DIR`) | `Asdf` → `npm install -g @getspur/spur-cli@latest && asdf reshim nodejs` — asdf does **not** auto-reshim on global npm installs; reshim is required for new executables. (Re-installs of an existing executable keep working via the existing shim, but always emit the reshim command so the guidance is correct for both first installs and version bumps.) |
| 3 | Canonical path contains `/.fnm/node-versions/` or `/fnm_multishells/` | `Fnm` → `npm install -g @getspur/spur-cli@latest` |
| 4 | Canonical path contains `/pnpm/global/` or `$PNPM_HOME/` | `Pnpm` → `pnpm add -g @getspur/spur-cli@latest` |
| 5 | Canonical path contains `/.bun/install/global/` | `Bun` → `bun add -g @getspur/spur-cli@latest` |
| 6 | Canonical path starts with `/opt/homebrew/`, `/usr/local/Cellar/`, or `/home/linuxbrew/` | `Homebrew` (npm-via-brew) → `npm install -g @getspur/spur-cli@latest` |
| 7 | Canonical path is under the output of `npm prefix -g` + `/bin/`, **or** original `npm_execpath`/`NPM_CONFIG_PREFIX` env vars present | `Npm` → `npm install -g @getspur/spur-cli@latest` |
| 8 | Canonical path under `$CARGO_HOME/bin/` or `~/.cargo/bin/` | `Cargo` → print: `Detected cargo install; rebuild with: cargo install --path crates/spur-cli` (or git equivalent) |
| 9 | None of the above | `Unknown` → print: `Could not detect install source. Reinstall using your original method, or: npm install -g @getspur/spur-cli@latest` |

Order matters: Volta/asdf/fnm shims often live under paths that also match the generic npm prefix, so the specific managers must be checked first. Querying `npm prefix -g` is a shell-out — do it lazily (only when step 1–6 fail and `npm` exists on `PATH`) and with a 1s timeout.

`Cargo`/`Unknown` paths print guidance and exit 0 — we never silently mutate the user's toolchain.

## 5. Edge cases

- **Offline / 5xx / DNS failure:** silent. `debug!` log only.
- **Pre-release current** (e.g. `1.3.0-beta.1`): when the installed binary is a pre-release, **also query** `https://registry.npmjs.org/@getspur/spur-cli` (without `/latest`) and inspect `dist-tags.beta` and `dist-tags.next`. Both tags are **optional** — their absence is not an error and must not cause the check to fail; treat a missing tag exactly like a non-pre-release current (only `latest` is consulted). Notify if a higher pre-release exists on a tag that does exist. Continue to also notify when stable `latest` exceeds the pre-release (the user can choose to leave the beta channel). Never recommend downgrade from beta to lower stable. **Without this, beta users are stranded** — `latest` only tracks the stable channel.
- **Downgrade scenario** (`latest < current`, e.g. user is on a dev build): cache the value, do not show banner, log at `debug!`. This is benign drift.
- **Crate ↔ npm wrapper version drift:** the npm wrapper is the source of truth for the user's installed version because that's how they actually installed it. We compare `env!("CARGO_PKG_VERSION")` (the binary the npm wrapper shipped) against `registry.npmjs.org/.../latest`. If the wrapper later diverges from the binary's crate version, the banner is still actionable ("run `spur upgrade`").
- **Cache corruption:** treat as cache-miss, overwrite on next check.
- **Concurrent TUI instances:** worst case is two writes to the same file; `tempfile` + `rename` keeps the file valid. No locking needed.

## 6. Opt-out

Honor both env vars (either disables the check):

- `SPUR_NO_UPGRADE_CHECK=1` — SPUR-native
- `NO_UPDATE_NOTIFIER=1` — industry standard (vercel, wrangler, npm-update-notifier ecosystem)

No config-file toggle in v1. If demand emerges, add to `~/.spur/config.toml` later.

## 7. CI / non-interactive defaults

Skip the check when **any** of:

- `CI` env var is set to any non-empty, non-`false` value — **match `spur-telemetry`'s existing rule** rather than a narrower `CI=true`. Single source of truth for "are we in CI" across the codebase.
- `std::io::stdout().is_terminal()` returns false
- Invocation is not `Commands::Tui`

These guards live in a `fn is_non_interactive() -> bool` near the wire-in site in `main.rs`, not inside `check_for_upgrade` — keeps the module pure and testable, and lets the spawn-or-skip decision happen before any cache or network work.

## 8. Telemetry

No new events. If the existing `spur-telemetry` Tier-1 channel is enabled, an existing `cli_startup` event can carry an `upgrade_available: bool` property. Out of scope for v1; flag for a follow-up.

## 9. Testing

- Unit: cache read/write roundtrip, TTL boundary, semver comparison including pre-release, install-source detection from synthetic env/paths.
- Integration: stub the npm registry with a local `wiremock`-style server; assert silent failure on 5xx, malformed JSON, timeout. Reuse the pattern from `spur-telemetry` tests.
- Manual: smoke-test the banner appearance with a forged cache file claiming `latest = 999.0.0`.

## 10. Industry references

| Tool | Pattern |
|---|---|
| **gemini-cli** | Async-on-start, passive, no persistent cache, config-file opt-out (`bundle/interactiveCli-MZFG35NB.js` ~30878) |
| **claude-code** | Passive native banner, never blocks |
| **vercel** | Detached worker process for fetch, 24h check + 3d notify intervals, cache at `~/.cache/com.vercel.cli/package-updates/`, respects `NO_UPDATE_NOTIFIER` (`dist/index.js` ~842) |
| **wrangler** | `Promise.race()` with grace timeout, abandons if slow |

No modern tool uses the `update-notifier` npm library — too heavy. We follow the lightweight detached-check pattern.

## 11. Revision history

**v3 (2026-05-19, post-codex-second-pass).** Five fixes from codex's review of v2:

1. **`last_notified_at` wiring.** v2 specified bumping the cache on dismissal, but the TUI receives only `UpgradeInfo` and has no cache handle. Moved the bump entirely into `check_for_upgrade`: when it returns `Some`, it has already written `last_notified_at = now`. The TUI side never touches the cache. Updated §2 and §3.1 accordingly.
2. **asdf reshim guidance.** v2 said asdf "reshims automatically" — incorrect. asdf requires `asdf reshim nodejs` after npm global installs of new executables. Updated §4 step 2 to emit the reshim command.
3. **asdf detection too narrow.** v2 matched `~/.asdf/installs/nodejs/`, missing custom `$ASDF_DATA_DIR` setups. Broadened the pattern to `/installs/nodejs/` + `/lib/node_modules/@getspur/` independent of the `~/.asdf/` prefix.
4. **`dist-tags.beta` / `next` made explicitly optional.** v2 didn't say what happens if those tags don't exist on the registry. §5 now states their absence is not an error and falls back to `latest`-only behavior.
5. **Windows atomic-rename caveat.** v2 implied universal atomicity. Added a note that Unix is guaranteed; Windows needs a smoke test if/when SPUR ships there. Emphasized the cache is non-critical so a failed write degrades safely.

**v2 (2026-05-19, post-codex-review).** Fixes from codex review of v1:

1. `cache_path()` added to public module API; `?` removed from wire-in snippet so cache-dir failure cannot abort startup.
2. Cache location switched from `dirs::cache_dir()` to `~/.spur/cache/upgrade-check.json` via the workspace `directories` crate, matching existing SPUR convention.
3. Install-source detection expanded into a 9-step ordered heuristic table covering Volta, asdf, fnm, pnpm, bun, Homebrew, npm, cargo, and unknown. Previous v1 heuristic (`npm_execpath` env) only fired during npm-launched processes and missed nearly every direct-invocation case.
4. Pre-release policy made explicit: query `dist-tags.beta` / `dist-tags.next` when current binary is pre-release. v1 would have stranded beta users.
5. Oneshot receiver lifecycle specified in §3.3 (`Option<Receiver>` + `pending()` arm when `None` + clear-after-fire).
6. Atomic write switched from `tempfile` to `std::fs::rename` from a sibling tmp path — `tempfile` is dev-dep only in `spur-cli` today, avoid promoting it for one use.
7. CI detection aligned with `spur-telemetry`'s existing rule (any non-empty, non-`false` `CI`), not narrower `CI=true`.
8. Wire-in description corrected — insertion point is just before `run_tui_with_license` at `main.rs:1120`, not "TUI branch entry"; bootstrap (config, onboarding, singleton, PM, orchestrator) happens in between.
9. Flagged `spur-pm`'s unused `semver` direct dep as a cleanup candidate alongside the workspace-promotion.

## 12. Open questions

1. Should the banner auto-dismiss after N seconds or stay until explicitly cleared? **Default: stay until cleared.**
2. Should `spur upgrade` (npm path) `exec` the command (replacing the process) or `spawn` and exit? **Default: `spawn` + wait + report exit code.**
3. Future: a `spur self-update` that downloads the binary directly without going through npm. Out of scope for v1.
