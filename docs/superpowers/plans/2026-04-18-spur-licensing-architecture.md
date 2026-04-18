# SPUR Licensing Architecture Implementation Plan

> **For agentic workers:** follow the existing spec-to-plan workflow. Keep the rollout phased, use checkboxes for tracking, and do not start cross-crate integration until the compile-spike gate below is resolved.

**Goal:** land commercial licensing in SPUR without regressing startup latency, offline viability, or runtime truthfulness. LicenseSeat is the first provider, but SPUR must expose a provider-agnostic internal contract and route runtime updates through the existing event model.

**Architecture:** this rollout is intentionally split into two stages. Stage A is a compile spike in a new `spur-license` crate pinned to `licenseseat = "=0.5.3"` to verify the real Rust SDK surface, storage behavior, and refresh primitives. Stage B uses that proven adapter to thread a shared `LicenseState` through `spur-cli`, `spur-core`, and `spur-tui`, with background refresh propagated via the orchestrator's broadcast/event funnel instead of a TUI-private side channel.

**Tech Stack:** Rust 2021, `tokio`, `clap`, `ratatui`, `serde`, `chrono`, `directories`, `anyhow`, `thiserror`, `licenseseat = "=0.5.3"`. No additional third-party dependencies unless the compile spike proves they are required.

**Source spec:** [docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md:1)

---

## Branch Decisions

- Choose compile spike before integration. Public LicenseSeat Rust docs are inconsistent, so the cheapest truthful path is to verify the actual crate surface first.
- Keep the root SPUR abstraction as cached signed entitlement state, not machine-locked key handling.
- Route refresh and downgrade signals through the existing `SpurEvent` funnel. Reject a TUI-only license channel.
- Keep canonical provider artifacts user-global. Repo-local `.spur/` state may cache derived UX only.
- Add `spur auth` only after the adapter contract is proven, not before.

---

## File Map

| File | Responsibility | Role in plan |
|---|---|---|
| `Cargo.toml` | workspace members + shared dependency pin | Modified by Task 1 |
| `crates/spur-license/Cargo.toml` (new) | licensing crate manifest | Created by Task 1 |
| `crates/spur-license/src/lib.rs` (new) | provider-agnostic `LicenseState` contract | Created by Tasks 1-2 |
| `crates/spur-license/src/provider.rs` (new) | provider trait / facade | Created by Task 2 |
| `crates/spur-license/src/licenseseat.rs` (new) | LicenseSeat adapter | Created by Tasks 1-2 |
| `crates/spur-license/tests/licenseseat_probe.rs` (new) | compile-spike and mapping tests | Created by Tasks 1-2 |
| `crates/spur-acp/src/domain/events.rs` | serializable runtime event payloads | Modified by Task 3 |
| `crates/spur-core/src/orchestrator.rs` | startup ownership + event emission seam | Modified by Tasks 3-4 |
| `crates/spur-core/src/event_funnel.rs` | existing event propagation seam | Audited by Task 3 |
| `crates/spur-core/src/license_runtime.rs` (new) | background validate/heartbeat/subscribe pump | Created by Task 3 |
| `crates/spur-cli/src/main.rs` | CLI command surface + startup hydration | Modified by Tasks 4-5 |
| `crates/spur-cli/src/commands/auth.rs` (new) | `spur auth` handlers | Created by Task 5 |
| `crates/spur-tui/src/app.rs` | initial state injection + update handling | Modified by Task 4 |
| `crates/spur-tui/src/views/dashboard.rs` | visible license status projection | Modified by Task 4 |
| `crates/spur-tui/src/components/status_bar.rs` | compact license badge / degraded marker | Modified by Task 4 |
| `crates/spur-acp/tests/license_events_roundtrip.rs` (new) | round-trip event serialization | Created by Task 3 |
| `crates/spur-tui/tests/license_status_render.rs` (new) | TUI render/update coverage | Created by Task 4 |
| `crates/spur-cli/tests/auth_cli.rs` (new) | CLI auth surface tests | Created by Task 5 |
| `docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md` | design source of truth | Updated by Task 1 only if spike disproves assumptions |

---

## Dependencies Across Tasks

- Task 1 is a hard gate for all later work. If the pinned crate surface differs materially from the spec, stop and update the spec before merging Tasks 2-5.
- Task 2 depends on Task 1 because the provider-agnostic facade must be shaped by the verified SDK API, not the docs alone.
- Task 3 depends on Task 2 because runtime refresh should consume the stable adapter instead of raw provider types.
- Task 4 depends on Tasks 2-3 because startup hydration and TUI updates need the shared `LicenseState` and the runtime event path.
- Task 5 depends on Task 2 and can overlap partially with Task 4 once the adapter contract is stable.
- Task 6 is last: final verification, docs reconciliation, and rollout notes.

---

## Task 1: Create `spur-license` Compile Spike and Prove SDK Reality

**Intent:** verify the exact `licenseseat = "=0.5.3"` Rust surface before landing cross-crate integration.

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/spur-license/Cargo.toml`
- Create: `crates/spur-license/src/lib.rs`
- Create: `crates/spur-license/src/licenseseat.rs`
- Create: `crates/spur-license/tests/licenseseat_probe.rs`

- [ ] **Step 1: Add the new workspace crate and pin the provider dependency**

Add `crates/spur-license` to `[workspace].members` and pin `licenseseat = "=0.5.3"` in the new crate manifest. Do not add the provider crate to unrelated workspace crates yet.

- [ ] **Step 2: Implement a minimal probe that compiles the documented SDK surface**

The probe should do the minimum truthful work needed to verify:

- client construction with a publishable `pk_*` key path
- cached-state access via the documented accessor
- method availability for `activate`, `validate`, `heartbeat`, and `subscribe`
- deactivation support if the actual crate surface exposes it

This task is allowed to use placeholder config values for compile coverage. It must not embed secrets.

- [ ] **Step 3: Add a local-only test harness and a manual ignored test path**

Split verification into two layers:

- normal unit / compile tests that run in CI without live provider credentials
- optional `#[ignore]` manual tests, enabled only when explicit env vars are supplied, for confirming live behavior against a real tenant

The point of the ignored path is to validate uncertain runtime behavior such as cache location, subscription lifecycle, and no-network startup after activation. The default suite must stay offline.

- [ ] **Step 4: Run the spike gate**

Run:

```bash
cargo check -p spur-license
cargo test -p spur-license
```

If the real crate surface disagrees with the spec on naming, construction, storage customization, or refresh semantics, stop here and update the source spec before continuing.

- [ ] **Step 5: Record the findings**

Update [the spec](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md:1) only if the spike disproves a current claim. Do not proceed on undocumented assumptions.

---

## Task 2: Stabilize the Provider-Agnostic `spur-license` Contract

**Intent:** hide vendor details behind a small SPUR-native facade.

**Files:**
- Modify: `crates/spur-license/src/lib.rs`
- Create: `crates/spur-license/src/provider.rs`
- Modify: `crates/spur-license/src/licenseseat.rs`
- Modify: `crates/spur-license/tests/licenseseat_probe.rs`

- [ ] **Step 1: Define the canonical SPUR-native state model**

Create serializable, provider-agnostic types for:

- `LicenseState`
- `SubjectKind`
- `BindingMode`
- `Plan`
- `FeatureFlag`
- degraded / inactive / expired status representation

Keep the state model rich enough for LTD, Pro, Team, Enterprise, and CI subjects. Do not leak raw provider structs into public APIs.

- [ ] **Step 2: Define the facade surface**

Expose a small API equivalent to:

- `current_state()`
- `activate(key)`
- `validate()`
- `heartbeat()`
- `deactivate()`
- `subscribe()`
- `has_entitlement(feature)`

This can be a trait plus concrete adapter, or a concrete facade with private provider internals. Choose the shape that keeps consumer crates independent of provider types.

- [ ] **Step 3: Add mapping and fallback behavior**

Map provider-side cached state into `LicenseState` so consumers can distinguish at least:

- no activation present
- active and healthy
- active but degraded due to transient refresh failure
- expired / revoked / invalid after authoritative refresh

Be explicit about offline-safe behavior: cached state is authoritative until the provider's lease semantics say otherwise.

- [ ] **Step 4: Test the public contract**

Add unit tests that cover:

- inactive startup
- cached active startup
- feature entitlement checks
- degraded-state mapping
- downgrade / invalid transitions after refresh

These tests should not require live provider network access.

---

## Task 3: Add a Truthful Runtime Refresh Path Through `SpurEvent`

**Intent:** reuse the orchestrator's event funnel for background license refresh and status changes.

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Create: `crates/spur-acp/tests/license_events_roundtrip.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`
- Create: `crates/spur-core/src/license_runtime.rs`

- [ ] **Step 1: Add a serializable runtime event payload**

Introduce a new event payload for license updates, for example:

```rust
SpurEventBody::LicenseUpdated {
    state: LicenseStateEvent,
}
```

If pulling `spur-license` types into `spur-acp` creates awkward layering, mirror the event payload the same way `IssueDetailEvent` mirrors `spur_pm::Issue`.

- [ ] **Step 2: Round-trip the event contract**

Create an ACP event serialization test modeled on existing round-trip tests so log replay and TUI consumption remain stable.

- [ ] **Step 3: Implement a runtime pump**

Create a small runtime helper in `spur-core` that:

- accepts the proven `spur-license` facade
- emits an initial `LicenseUpdated` snapshot
- runs `validate()` on the configured interval
- runs `heartbeat()` only when the active binding mode requires it
- forwards provider subscription updates into the event funnel

This task must not block orchestrator startup on network I/O.

- [ ] **Step 4: Add a narrow emission seam in `Orchestrator`**

Keep the emission path explicit. Either the orchestrator owns the runtime helper directly, or it exposes one dedicated method that lets trusted runtime helpers emit a `SpurEventBody` through the existing funnel. Do not create an unrelated broadcast channel for licensing.

- [ ] **Step 5: Test degraded and update propagation**

Add unit tests around the runtime helper using a fake provider so refresh failures, revocations, and subscription updates are converted into truthful `LicenseUpdated` emissions.

---

## Task 4: Thread `LicenseState` Into CLI Startup and TUI Rendering

**Intent:** first frame uses cached state, and the TUI updates when runtime refresh changes that state.

**Files:**
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/components/status_bar.rs`
- Create: `crates/spur-tui/tests/license_status_render.rs`

- [ ] **Step 1: Hydrate cached state before TUI launch**

On the `Watch` path, construct the license facade before `run_tui_with_config(...)`, read cached state synchronously, and pass the initial `LicenseState` into the TUI constructor. This preserves zero-latency first paint.

- [ ] **Step 2: Extend app construction with initial license state**

Update `App::new_with_config(...)` and the surrounding state to carry the initial license snapshot. Keep this state separate from provider internals.

- [ ] **Step 3: Apply runtime updates from `LicenseUpdated` events**

Teach `App::handle_spur_event(...)` and the dashboard projection to react to the new event without bypassing the existing event stream.

- [ ] **Step 4: Surface compact status in the dashboard**

Extend the dashboard/status-bar rendering to show a small, non-intrusive badge with enough signal for:

- plan
- active vs inactive
- degraded / offline status when relevant

Do not add verbose billing copy to the main dashboard.

- [ ] **Step 5: Test render and update behavior**

Add TUI tests that cover:

- first render with inactive cached state
- first render with active cached state
- transition from active to degraded after a runtime event
- transition from active to invalid after authoritative refresh

---

## Task 5: Add `spur auth` Command Surface on Top of the Proven Adapter

**Intent:** provide operator UX for activation and inspection without coupling CLI parsing to raw provider types.

**Files:**
- Modify: `crates/spur-cli/src/main.rs`
- Create: `crates/spur-cli/src/commands/auth.rs`
- Create: `crates/spur-cli/tests/auth_cli.rs`

- [ ] **Step 1: Add the command family**

Introduce:

- `spur auth login --key ...`
- `spur auth status`
- `spur auth refresh`
- `spur auth logout`

Do not add `trial` unless the compile spike and product policy confirm a grounded provider path for it.

- [ ] **Step 2: Implement the handlers against `spur-license`**

The handlers should call the SPUR-native facade only. They must not depend on raw LicenseSeat types or vendor-specific error formatting at the command layer.

- [ ] **Step 3: Print truthful status output**

`spur auth status` should show a compact snapshot:

- active / inactive / degraded
- plan
- expiry or lease information when available
- binding mode

Avoid printing secrets, fingerprints, or raw provider blobs.

- [ ] **Step 4: Test the command surface**

Add CLI tests for parsing and output shape using a fake provider or an injectable facade. The tests should not require live provider credentials.

---

## Task 6: Final Verification, Docs Reconciliation, and Rollout Notes

**Intent:** finish with tests and docs that match the implementation rather than the proposal.

**Files:**
- Modify: `docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md` only if implementation findings require it
- Modify: `docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md`

- [ ] **Step 1: Run focused verification**

Run:

```bash
cargo test -p spur-license
cargo test -p spur-acp license_events_roundtrip
cargo test -p spur-tui license_status_render
cargo test -p spur-cli auth_cli
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

- [ ] **Step 2: Reconcile code and docs**

If the final implementation differs from the spec in any material way, update the spec immediately so the design record remains truthful.

- [ ] **Step 3: Capture rollout notes**

Document:

- whether the provider cache path is configurable or provider-managed only
- whether background refresh should run for non-TUI commands
- any manual activation steps needed for air-gapped environments
- any follow-up work for plan-based feature gates, team/org entitlements, or trial UX

---

## Exit Criteria

- `spur-license` compiles against the pinned LicenseSeat crate and hides provider types behind a SPUR-native facade.
- Startup remains local-only: cached state is available before first TUI frame.
- License refresh travels through the orchestrator's existing event path, not a private TUI channel.
- `spur auth` exists with truthful status, login, refresh, and logout commands.
- Tests cover event serialization, TUI state/render updates, and CLI auth UX without requiring live provider access.
- The spec and plan match the code that ships.

---

## Deferred Work

- Fine-grained product gating for specific premium commands and workflows. This should land only after product policy defines the exact entitlement matrix.
- Self-hosted / enterprise provider replacement. The contract should make this possible, but it is not part of the first provider rollout.
- Trial UX if the provider and policy model support it cleanly.
