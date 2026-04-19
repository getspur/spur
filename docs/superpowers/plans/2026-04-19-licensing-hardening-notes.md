# Licensing Hardening Rollout Notes

Companion to [the hardening plan](./2026-04-19-licensing-hardening.md). Records Phase-0 decisions, verification evidence, and operational guidance.

## Phase 0 outcomes

Per [the emission audit RCA](../../../docs/rca/2026-04-19-licenseseat-emission-audit.md):

- **Gate 1 — C9 duplicate emission:** **CONFIRMED**. `licenseseat 0.5.3` fires through `sdk.subscribe()` synchronously during every explicit handler call (activate/validate/heartbeat/deactivate), and `LicenseSeatProvider::replace_state` broadcasts the same kinds. Task 7 (`a033aaf`) added a 10-variant filter in `spawn_sdk_event_bridge` that drops the handler-originated kinds; autonomous/server-push kinds (LicenseRevoked, Offline*, LicenseLoaded, Network*) still forward.
- **Gate 2 — D1 heartbeat gating:** **SKIP**. Upstream has no `BindingMode` enum and no per-mode heartbeat policy; `start_background_tasks` launches heartbeats unconditionally. SPUR's existing `should_heartbeat(state) = state.is_active() && !matches!(state.binding_mode, BindingMode::Unknown)` is a coarser SPUR-layer gate that prevents heartbeats on unbound subjects; it is intentionally retained. The `LicenseProvider::requires_heartbeat() -> bool` trait method with default `false` was still added (Task 4) to support future provider implementations that may want per-adapter control.
- **Gate 3 — G2 cached plan hydration:** **EXECUTE**. `LicenseSeat::current_license()` returns `Option<License>` with `trusted_license: Option<LicenseResponse>` carrying `plan_key` + `active_entitlements` pre-network. Task 6 (`3b0edfe`) introduced `hydrate_from_cached` to populate Plan + features + expires_at at cold start.

## Regression oracles

- **C9 oracle:** `crates/spur-license/tests/emission_audit.rs::explicit_handlers_emit_exactly_once` (ignored; requires `SPUR_LICENSESEAT_API_KEY`/`_PRODUCT_SLUG`/`_TEST_KEY`). Asserts each explicit handler emits exactly 1 `LicenseEvent`. Operator-runnable after any future change to the bridge or to the upstream crate.
- **Transient-error invariant:** `crates/spur-license/tests/invariants.rs` (proptest, 64 cases). Locks in "network errors never transition Active→Invalid".
- **Runtime transitions:** `crates/spur-core/tests/license_runtime_fake_provider.rs` (5 tests). Active↔Degraded, authoritative Invalid, autonomous event relay, D3 boot latency, D5 Invalid-text preservation.
- **TUI render:** `crates/spur-tui/tests/license_status_render.rs` (4 tests). Covers Inactive default, Active-cached seed, Degraded transition, Active→Invalid flip.
- **CLI behavior:** `crates/spur-cli/tests/auth_fake_provider.rs` (4 happy-paths) + `auth_cli.rs` (4 spawn-bin tests including `--format json`).

## Operator guidance

### LicenseSeat cache path

`licenseseat 0.5.3` manages its own cache; SPUR does not override the path. Per the RCA's Gate 3 investigation, the cache lives at the default location chosen by the upstream crate (check `~/.local/share/licenseseat/` or equivalent per-OS `directories`-crate conventions). Custom cache paths for air-gapped hosts or read-only environments are deferred work (see below).

### Background refresh scope

- `spur watch` — the orchestrator's `spawn_license_runtime` runs background validate (initial within 30s via D3, then `validate_interval` cadence) and heartbeat (per existing coarse gate) for the life of the TUI session.
- `spur auth ...` — **no** background refresh. Operators must run `spur auth refresh` explicitly to force a validate from the command line.
- Other `spur ...` commands (`spur run`, etc.) — no licensing activity; they use the cached state snapshot from the facade at command-dispatch time.

### Air-gapped activation

Not supported in this rollout. `spur auth login --key <KEY>` performs a network activate. Machine-file offline activation is a known gap (see deferred items below).

### JSON output

`spur auth {login,status,refresh,logout} --format json` emits one JSON line on stdout using the `LicenseStateEvent` schema (keys: `status`, `subject_kind`, `plan`, `features`, `expires_at`, `binding_mode`, `offline_ok`, `status_text`). This is the stable programmatic contract — any future change to the schema must bump the CLI's major version.

## Deferred items

- **B3** — split `SpurLicense` construction from background-start (`new(cfg)` + explicit `start_background(handle)`). Constructor currently spawns the SDK event bridge as a side effect via `Handle::try_current()`.
- **C2** — migrate `LicenseSeatProvider`'s `state: Arc<RwLock<LicenseState>>` from `std::sync::RwLock` to `parking_lot::RwLock` to eliminate poison handling.
- **H1** — sanitize upstream provider error strings before surfacing them in `auth status` / `auth refresh`. Today raw `Display` strings flow through `status_text`.
- **Custom cache path configuration** — allow operators to relocate the LicenseSeat cache.
- **Typed state-machine refactor** — replace `LicenseState` bag-of-fields with an enum like `enum LicenseFSM { Inactive, Active(ActiveState), Degraded(ActiveState), Invalid { reason: String } }`. Deferred pending evidence the current shape is actually broken.
- **Multi-provider** — second provider (self-hosted / enterprise). The `LicenseProvider` trait is ready for it.
- **Trial UX** — no provider-side trial flow yet.
- **Pre-existing clippy debt** — `spur-tui` has 19 pre-existing `clippy::collapsible_match`, `clippy::redundant_closure`, and related warnings in `session_detail.rs`, `session_picker.rs`, `mermaid_viewer.rs`, and adjacent views. These predate the licensing hardening plan and are out of scope.

## Verification summary

Captured on 2026-04-19 at HEAD `b454293` (fmt touch-ups commit, preceding the Task 15 docs commit):

```
### spur-license default (unit + integration)
test result: ok. 2 passed; 0 failed (lib)
test result: ok. 1 passed; 0 failed; 1 ignored (emission_audit)
test result: ok. 5 passed; 0 failed; 2 ignored (licenseseat_probe)

### spur-license --features test-support
test result: ok. 2 passed; 0 failed (lib)
test result: ok. 1 passed; 0 failed; 1 ignored (emission_audit)
test result: ok. 3 passed; 0 failed (fake_provider)
test result: ok. 1 passed; 0 failed (invariants, 64 proptest cases)
test result: ok. 5 passed; 0 failed; 2 ignored (licenseseat_probe)

### spur-acp license_events_roundtrip
test license_updated_roundtrips ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

### spur-tui license_status_render
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

### spur-cli auth_cli
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.49s

### spur-cli auth_fake_provider
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

### spur-core license_runtime_fake_provider
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s

### workspace (all crates)
All test suites: ok — zero failures across 20 test binaries.

### clippy workspace
19 pre-existing errors in spur-tui (collapsible_match, redundant_closure, etc.) — out of scope.
error: could not compile `spur-tui` (lib) due to 19 previous errors

### fmt
Fixed and committed in b454293. cargo fmt --all --check now clean.
```
