# Upgrade Check — Design Spec

**Status:** Approved for planning
**Date:** 2026-05-19
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

The banner is dismissible via the existing `user_warning` clear keybind. If the user dismisses it, the cache's `last_notified_at` field is bumped so we don't show it again for 3 days regardless of cache state (matches vercel's `notifyInterval`).

## 3. Architecture

### 3.1 New module: `crates/spur-cli/src/upgrade_check.rs`

Public surface:

```rust
pub struct UpgradeInfo {
    pub current: semver::Version,
    pub latest:  semver::Version,
    pub install_source: InstallSource, // Npm | Cargo | Unknown
}

pub fn upgrade_check_disabled() -> bool;        // honors SPUR_NO_UPGRADE_CHECK and NO_UPDATE_NOTIFIER
pub async fn check_for_upgrade(cache_path: &Path) -> Option<UpgradeInfo>;
```

`check_for_upgrade` is the only async entry point. It:

1. Reads cache at `cache_path`. If cache age < `CHECK_INTERVAL` (24h), returns cached result.
2. Otherwise, GETs `https://registry.npmjs.org/@getspur/spur-cli/latest` with a 2s total timeout, parses `{ "version": "..." }`, writes the cache, and returns `Some(UpgradeInfo)` iff `latest > current` (semver-aware, ignoring pre-releases — see §5).
3. Any failure (network, parse, JSON shape, fs) → `debug!` log, return `None`. **Never blocks, never panics, never surfaces to user.**

### 3.2 Wire-in: `crates/spur-cli/src/main.rs`

The TUI branch starts at `main.rs:898` and calls `run_tui_with_license` at `main.rs:1120`. Insert the spawn just before that call:

```rust
let upgrade_rx = if !upgrade_check::upgrade_check_disabled() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let cache_path = upgrade_check::cache_path()?;
    tokio::spawn(async move {
        let _ = tx.send(upgrade_check::check_for_upgrade(&cache_path).await);
    });
    Some(rx)
} else {
    None
};
```

`upgrade_rx` is passed through `run_tui_with_license` into the TUI app constructor.

### 3.3 TUI receiver: `crates/spur-tui/src/app/events.rs:681`

Add a `tokio::select!` arm that awaits the oneshot. On `Ok(Some(info))`, call `app.show_user_warning(format!("SPUR {} is available; current {}. Run: spur upgrade", info.latest, info.current))`. On `Ok(None)` or `Err(_)`, do nothing.

The app must tolerate a missing receiver (one-shot CLI, opt-out path).

### 3.4 HTTP

Use workspace `reqwest` already pinned in root `Cargo.toml` and used by `spur-telemetry/src/client.rs:37` with a 2s timeout. Add `reqwest = { workspace = true }` to `crates/spur-cli/Cargo.toml`. **No new HTTP dependency.**

### 3.5 SemVer

Promote `semver = "1"` from `spur-pm`'s direct dep to a workspace dep, add to `spur-cli`. Do not hand-roll comparison.

### 3.6 Cache schema

Location: `dirs::cache_dir()?.join("spur/upgrade-check.json")` (e.g. `~/Library/Caches/spur/upgrade-check.json` on macOS, `~/.cache/spur/upgrade-check.json` on Linux). Matches vercel's `~/.cache/com.vercel.cli/` convention.

```json
{
  "version": 1,
  "checked_at":      "2026-05-19T13:20:00Z",
  "last_notified_at": "2026-05-19T13:20:00Z",
  "current":  "1.1.16",
  "latest":   "1.2.0"
}
```

Atomic write via `tempfile` + `rename`. Parse failures → log `warn!` and overwrite on next successful check.

## 4. `spur upgrade` subcommand

```
spur upgrade [--check] [--force]
```

- Without flags: detects install source, then runs the appropriate command **after confirming with the user** (`y/N` prompt). This y/n is appropriate here because the user explicitly invoked `spur upgrade`.
- `--check`: prints current/latest, does not mutate.
- `--force`: skips confirmation. Useful for scripted updates.

Install-source detection (`InstallSource`):

| Signal | Verdict |
|---|---|
| `NPM_CONFIG_PREFIX` / `npm_execpath` env present, **or** binary path under `<npm-global-prefix>/bin/` | `Npm` → run `npm install -g @getspur/spur-cli@latest` |
| Binary path under `$CARGO_HOME/bin/` or `~/.cargo/bin/` | `Cargo` → print: `Detected cargo install; run: cargo install --git <repo> spur-cli` |
| Neither | `Unknown` → print: `Could not detect install source. Reinstall using your original method, or: npm install -g @getspur/spur-cli@latest` |

`Cargo`/`Unknown` paths print guidance and exit 0 — we never silently mutate the user's toolchain.

## 5. Edge cases

- **Offline / 5xx / DNS failure:** silent. `debug!` log only.
- **Pre-release current** (e.g. `1.2.0-beta.1`): only notify when npm `latest` is a stable version that is greater. Never recommend downgrade from beta to lower stable.
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

- `CI=true` env var
- stdout is not a TTY
- Invocation is not the TUI subcommand (one-shot `spur exec`, `spur plan submit`, etc.)

This is a guard at the `main.rs:898` branch entry, not inside `check_for_upgrade` — keeps the module pure and testable.

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

## 11. Open questions

1. Should the banner auto-dismiss after N seconds or stay until explicitly cleared? **Default: stay until cleared.**
2. Should `spur upgrade` (npm path) `exec` the command (replacing the process) or `spawn` and exit? **Default: `spawn` + wait + report exit code.**
3. Future: a `spur self-update` that downloads the binary directly without going through npm. Out of scope for v1.
