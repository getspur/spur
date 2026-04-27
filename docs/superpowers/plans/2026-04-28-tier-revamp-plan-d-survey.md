# Tier Revamp Plan D — Trial / Upgrade Flow / Capability-Tease Survey

Date: 2026-04-28

Scope: survey only. Plan D is the *iceberg-amplifier* layer on top of
Plans A–C (registry + signed policy + runtime enforcement). Spec
§9.5 explicitly defers three Plan-D concerns from Wave-9:

1. **7-day Pro trial** via `spur upgrade trial` with NodeLocked
   anti-abuse (one trial per machine fingerprint).
2. **Capability-tease modals** in TUI — when a Free user hits a
   `core_pro_*`/`mcp_pro_*`/`bot_pro_*` gate, surface an upgrade
   prompt instead of a plain error.
3. **CLI upgrade flow** — `spur upgrade pro` purchase nudge that
   handoffs to a checkout URL; `spur auth login --key` polish.

This survey inventories what already exists in the codebase that Plan
D would build on, what is greenfield, and what hidden dependencies
exist between the three concerns. No design.

## Grounding commands

- `rg "trial|Trial|TRIAL|upgrade|Upgrade|UPGRADE"` over `crates/`
- Reads of `crates/spur-cli/src/main.rs:200-310` (subcommand
  declaration), `crates/spur-cli/src/commands/auth.rs:1-137`, and
  `crates/spur-cli/src/onboarding.rs:1-80` (first-run flow).
- Reads of `crates/spur-license/src/lib.rs:1-200` (`SpurLicense`
  facade + `Plan` enum + `BindingMode::NodeLocked`),
  `crates/spur-license/src/community.rs:60-92` (Community provider
  rejects `activate`), and `crates/spur-license/src/licenseseat.rs:1-100`
  (`LicenseSeatProvider` env wiring).
- Reads of `crates/spur-tui/src/components/collision_modal.rs:1-100`
  (the only TUI modal primitive today).
- Read of `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
  §6.2 (trial mechanism), §9.5 (Plan D explicit deferrals).

## Summary finding

- **Trial mechanism: 0% built.** No `Trial` variant in
  `commands::auth::AuthCommands`; no `Upgrade` top-level subcommand
  in `crates/spur-cli/src/main.rs:188-269`; no trial entitlement in
  the licenseseat provider; no machine-fingerprint anti-abuse beyond
  the SDK-level `BindingMode::NodeLocked`.
- **CLI upgrade flow: 30% built.** `spur auth login --key …`
  works end-to-end via `LicenseSeatProvider::activate`
  (`crates/spur-license/src/licenseseat.rs:57-82`). Onboarding
  prompt at `crates/spur-cli/src/onboarding.rs:62-77` already nudges
  Community → Pro. There is no `spur upgrade pro` command surface
  and no checkout-URL handoff.
- **Capability-tease modals: 5% built.** `CollisionModal`
  (`crates/spur-tui/src/components/collision_modal.rs:13-99`) is the
  one production modal pattern. Two inline tease strings exist:
  `crates/spur-tui/src/app.rs:41` (read-only banner) and `app.rs:814`
  (read-only metadata write block). Neither is modal; both miss the
  conversion-trigger UX from spec §6.2.

The existing pieces are an asset, not a liability — the LicenseSeat
SDK already produces NodeLocked bindings for `LicenseSeatProvider`
(`crates/spur-license/src/lib.rs:53,139,152`), and onboarding already
runs at every Community-default first-launch.

## 1. Trial Mechanism Inventory

### 1.1 Required surface (per spec §6.2)

The spec asks for `spur upgrade trial`:

- One trial per machine fingerprint (NodeLocked binding in licenseseat)
- 7 days from activation; auto-revert to Community at expiry
- Free users see upgrade prompts during trial
- Trial features = full Pro v1 entitlement set

### 1.2 Today's state

| Concern | File:line | State |
|---|---|---|
| `LicenseProvider::start_trial` trait method | `crates/spur-license/src/provider.rs:30-50` | absent |
| `AuthCommands::Trial` clap variant | `crates/spur-cli/src/commands/auth.rs:16-40` | absent |
| `Commands::Upgrade` top-level | `crates/spur-cli/src/main.rs:130-269` | absent |
| Trial-counter on machine fingerprint | n/a | absent (SDK NodeLocked is a *current* lock; not a one-shot trial budget) |
| Trial-expiry timer / auto-revert to Community | n/a | absent |
| Trial-state telemetry event | `crates/spur-license/src/lib.rs:185-200` (`LicenseEventKind`) | absent (existing variants: e.g. `Activated`/`Validated`/`Deactivated`/`HeartbeatOk`; no `TrialStarted`/`TrialExpired`) |

> **No new `Plan::Trial` variant.** Per spec §6.2, the trial reuses
> the existing `Plan::Pro` variant with `expires_at = now + 7d`.
> No new `LicenseStatus::Trial`, no new tier. The trial is a
> **time-boxed Pro entitlement**, not a separate plan.

### 1.3 What can be reused

- `LicenseSeatProvider::activate` (`licenseseat.rs:184-200`) already
  performs the NodeLocked binding via the licenseseat SDK and sets
  `Plan::Pro` from `trusted_license.plan_key`. The trial endpoint
  reuses this exact path: server (or local-JWT) issues a Pro
  license with a 7-day `expires_at`; existing `activate` code sets
  `Plan::Pro` automatically.
- `SpurLicense::activate` (`lib.rs:277-279`) is the public facade.
  A `start_trial(&self)` method on `SpurLicense` calls into the
  same provider path, just with a trial-specific input.
- `LicenseState::expires_at: Option<DateTime<Utc>>` (`lib.rs:105`)
  is already the natural carrier for trial expiry — `validate`
  already populates it from `result.license.expires_at`
  (`licenseseat.rs:219`).
- `LicenseEventKind::Activated` precedent (`lib.rs:185-200`) shows
  how trial-start emission would route through the existing
  broadcast channel. Plan E adds `TrialStarted` / `TrialExpired`
  variants alongside existing kinds.

### 1.4 What is greenfield

- Trial-server endpoint surface (the Plan D spec must specify
  whether trial is purely client-side time-boxed via signed JWT
  with `iat + 7d`, or server-issued with a counter).
- Anti-abuse beyond NodeLocked: a returning machine fingerprint
  must be detected even after a `deactivate` round-trip.
  Implementation candidate: tie trial to a server-stored hash of
  `InstallId::load_or_create()` (`crates/spur-license/src/install_id.rs`).
- Trial-expiry warning banners (Day 6 / Day 7 / expired) — neither
  TUI nor CLI surface them today.
- "Trial expired" downgrade-to-Community flow with grace.

## 2. CLI Upgrade Flow Inventory

### 2.1 Required surface

- `spur upgrade pro` — opens checkout URL; logs purchase intent.
- `spur upgrade trial` — start the 7-day trial.
- `spur upgrade status` — show trial days remaining / Pro tier
  status (alias of `spur auth status`).
- Onboarding-prompt enhancement: nudge trial first, paid second.

### 2.2 Today's state

| Concern | File:line | State |
|---|---|---|
| `Commands::Upgrade` clap variant | `crates/spur-cli/src/main.rs:130-269` | absent |
| `commands::upgrade` module | `crates/spur-cli/src/commands/mod.rs` | absent (`auth`, `init`, `flags`, `profile`, `config_check` only) |
| Onboarding first-run prompt | `crates/spur-cli/src/onboarding.rs:62-77` | **present** — paste-key flow only; does not offer trial |
| Checkout URL constant | n/a | absent |
| `Commands::Auth { command: AuthCommands::Login }` | `auth.rs:18-23` | **present**; same flow that `upgrade pro` would call after checkout |
| `format_plain_summary` Community message | `auth.rs:117-119` | **present**: `"spur Community — free tier  ⓘ run 'spur auth login --key …' to unlock Pro"` |

### 2.3 Reuse opportunities

The onboarding flow at `crates/spur-cli/src/onboarding.rs:62-95` is
already a Community-defaulting nudge:

```
spur is running on the Community tier (free). Paste a license key
to unlock Pro now, or press Enter to continue.
> _
```

Plan D's enhancement: extend this prompt to a 3-way fork (Pro key /
Start trial / Continue Free) without breaking the existing
paste-key path. The marker file at `~/.spur/onboarded` already
prevents repeat-prompts.

### 2.4 Greenfield

- Checkout URL and brand wiring (where does `spur upgrade pro`
  send the user? Must coordinate with payment processor work, which
  is outside this codebase).
- Headless-CI behavior (must skip-prompt; existing `is_terminal()`
  guard at `onboarding.rs:48-52` handles this).
- Re-prompt on trial-expiry-soon (today's marker is one-shot).

## 3. Capability-Tease Modal Inventory

### 3.1 Required surface

Per spec §6.2, a Free user attempting a Pro-gated action should see:

- A modal that names the locked capability ("Multi-worker fan-out")
- The Pro upgrade benefit ("…runs 10 workers in parallel")
- One-keystroke action: `[U]pgrade` / `[T]rial` / `[D]ismiss`
- The modal MUST NOT obscure the user's previous typing/work

### 3.2 Today's tease surface

| Pattern | Where | Modal? | Plan-D-reusable? |
|---|---|---|---|
| Read-only inline banner | `crates/spur-tui/src/app.rs:41` ("`Edits this session WILL NOT be persisted. Upgrade SPUR to enable writes.`") | no — inline status line | partial (the *copy* is good; the surface is wrong) |
| Read-only metadata write block | `crates/spur-tui/src/app.rs:812-815` | no — toast | partial |
| Community-tier env-var hint | `crates/spur-license/src/community.rs:74-80` | no — error message returned from `activate` | partial |
| `format_plain_summary` Community line | `crates/spur-cli/src/commands/auth.rs:117-119` | n/a (CLI text, not modal) | partial |
| `CollisionModal::render` | `crates/spur-tui/src/components/collision_modal.rs:13-99` | **yes** — full modal with title/body/keybindings | **yes (template for Plan D modals)** |

### 3.3 The CollisionModal pattern as template

`CollisionModal::render` is a 99-line ratatui modal with:

- `Clear` widget over the popup area (proper z-order)
- `Block` with title + yellow border
- Multi-line body with styled spans
- Footer with `[N]/[P]/[Esc]` keybindings (Modifier::BOLD on the key)

Plan D's `CapabilityTeaseModal` would mirror this exact structure
with: capability name, Pro-tier benefit, `[U]/[T]/[Esc]` keybindings.
Estimated effort: 1 file, ~100 lines, matches existing primitive.

### 3.4 Where would tease modals fire?

The natural fire-points are wherever Plan C lands a
`require_feature(FeatureKey::*pro*)?` that returns an error to a
TUI-driven action. Examples grounded in code:

- TUI dispatches a parallel-worker delegation that exceeds the Free
  quota (`max_concurrent_workers = 1`) → `core_core_parallel_workers`
  feature is fine, but quota is hit → tease "Pro = 10 workers"
- TUI mounts the issue browser view → `tui_core_view_issue_browser`
  is Free, no tease — but if the user clicks a Beads-graph operation,
  `pm_pro_beads_advanced` is required → tease modal
- TUI palette opens `mcp_pro_review` action → tease modal

The full enumeration depends on Plan C waves **C.10–C.14** (Pro
headline conversion-trigger waves, one per spec §9.5 category;
C.9 is Free baseline plumbing). Plan D modals consume Plan C's
typed `FeatureGateError { key: FeatureKey, ... }` output channel
(see §6.5).

## 4. Plan-D Internal Dependency Graph

```
                Plan A (registry)
                       │
                       ▼
                Plan B (signed policy)
                       │
                       ▼
                Plan C (enforcement sweep)
                  │            │
                  │            └─────────┐
                  ▼                       ▼
        D.tease  (consumes Plan C errors)  D.trial  (provider/SDK + CLI)
                  ▲                       │
                  │                       ▼
                  └────────── D.upgrade-cli (top-level subcommand)
```

D.trial is the longest cycle (server-side LicenseSeat coordination).
D.tease is the shortest (single new TUI component + Plan-C error
enrichment).
D.upgrade-cli is medium (new subcommand + onboarding-flow extension).

## 5. Open questions for the Plan-D spec

### 5.1 Trial mechanism — client-only JWT (decision: v1)

**Recommendation:** Client-only signed JWT. `spur upgrade trial`
writes a 7-day `Plan::Pro` license JWT to `~/.spur/license` (same
path as paid Pro licenses) with `expires_at = now + 7d`, signed
with the existing `spur-policy-2026-04` Ed25519 key. The existing
`LicenseSeatProvider::activate` path then ingests it like any other
license (the JWT format is the same).

**Why this is correct for v1:**

- **No server-side dependency** — Plan D ships without coordinating
  with LicenseSeat upstream. The licenseseat SDK 0.5.3 has no
  `start_trial` endpoint; pinning v1 to a client-only path
  unblocks the Plan D ship.
- **Anti-abuse via local-data-destruction cost:** The naïve attack
  (wipe `~/.spur/` and re-trial) is mitigated by the spec §9.5
  iceberg lock-in: `~/.spur/.beads/`, plan history, session SQLite,
  and TUI keybinding sunk cost all live in `~/.spur/`. Wiping the
  trial nukes the user's own PM data and history. The destructive
  cost is enough deterrent for v1.
- **NodeLocked still applies:** The trial JWT carries the same
  `InstallId` payload as paid licenses; multi-machine trial-share
  still bounces off the SDK's machine-binding check.

**v2 path:** When LicenseSeat upstream ships a `start_trial`
endpoint, swap the client-side JWT mint for a server call. The
state shape (`Plan::Pro` + `expires_at`) does not change; the
swap is a provider-impl detail.

**Out of scope for the survey:** the Ed25519 signing-key story
for trial JWTs (single key shared with paid licenses, or a
trial-only key with limited entitlements). Plan D spec phase
decides.

### 5.2 Trial-during-Pro-tease coupling

If a Free user clicks "Start Trial" inside a tease modal, does the
modal close immediately and re-attempt the gated action, or close
silently and let the user retry? UX decision.

### 5.3 Onboarding-prompt re-entry

Today's `~/.spur/onboarded` marker is one-shot. If trial expires,
should onboarding re-prompt? Spec must specify.

### 5.4 Capability-tease budget

If 10 Pro keys all hit on the same screen, do we show 10 modals?
Need an accumulator or per-session budget (e.g., max 1 tease per
30 seconds).

## 6. Risks

### 6.1 Trial-counter trust boundary

The licenseseat SDK's NodeLocked binding is a *current-state* lock,
not a *historical* counter. Without server-side state, a user can
`deactivate` and `start_trial` repeatedly. Mitigation must be
specified in the Plan D spec; likely server-side install_id hash
allowlist.

### 6.2 Modal interruption regression

`CollisionModal` renders only at attach-collision boundary. A Plan
D `CapabilityTeaseModal` that fires mid-streaming-output could
interrupt the agent loop's redraw. Spec must specify: tease modals
only fire on *user action*, never on background event.

### 6.3 Onboarding-flow prompt regression

The existing TTY-skip at `onboarding.rs:48-52` handles non-TTY CI.
Plan D must preserve that (CI cannot start a trial; CI cannot see a
modal). Test surface: `crates/spur-cli/src/onboarding.rs` has no
test today; Plan D must add one.

### 6.4 spec §6.2 vs spec §9.5 — no inconsistency (resolved)

Spec §6.2 specifies the trial mechanism; spec §9.5 explicitly
defers the trial to "Plan D scope" *per spec §6.2*. Defining a
mechanism in §6.2 and formally deferring its implementation in
§9.5 is correct staging, not a contradiction. The earlier
"inconsistency" reading was a misread; no spec amendment needed.

### 6.5 Plan-C error-shape contract (Plan C ships first)

Tease modals are downstream consumers of Plan C's
`require_feature` error output. The right ordering: **Plan C
ships first** with a strongly-typed `FeatureGateError { key:
FeatureKey, ... }` returned from `require_feature`. Plan D D.6
then pattern-matches on this stable typed output — no
co-shipping required, and no temporal coupling between the two
specs.

The Plan C spec phase must commit to the typed-error contract
before Plan D D.5 starts (D.5 designs the modal against the
contract; D.6 wires it).

## 7. Suggested Plan-D wave segmentation

| Wave | Scope | Risk | Depends on |
|---|---|---|---|
| D.1 | Add `LicenseProvider::start_trial` trait method (no impl yet); reuses `Plan::Pro` + `expires_at`; no new tier | low | Plan B done |
| D.2 | `LicenseSeatProvider::start_trial` client-only JWT impl per §5.1 (mints 7-day `Plan::Pro` JWT, ingests via existing `activate` path) | medium | D.1 |
| D.3 | `commands::upgrade` module with `Upgrade::{Trial, Pro, Status}` subcommands | low | D.2 |
| D.4 | Extend `onboarding.rs::maybe_prompt_first_run` to offer 3-way fork (paid key / start trial / continue Free) | low | D.3 |
| D.5 | `CapabilityTeaseModal` TUI component (mirror `CollisionModal` shape; pure render component, no dependencies) | low | none |
| D.6 | Wire tease modal into Plan-C `FeatureGateError { key: FeatureKey, ... }` path | medium | Plan C typed-error contract + D.5 |
| D.7 | Trial-expiry warning banners (Day 6 / Day 7 / expired) | low | D.2 |
| D.8 | **Trial → Community downgrade-with-grace flow** at `expires_at` (one-shot snapshot rebuild from `LicenseEvent`; warn-then-revert on next license refresh) | medium | D.2 + D.7 |
| D.9 | Telemetry events for trial-start / trial-expired / upgrade-clicked | low | D.2 |

9 waves total. D.1–D.4 land the trial+CLI; D.5–D.6 land the TUI
tease (D.5 parallelizable with D.1–D.4 since no dependency); D.7+D.8
land the expiry+downgrade UX; D.9 is telemetry polish.

**D.8 (downgrade-with-grace)** was missing from the v1 of this
survey but is required: §1.4 flagged it as greenfield without
slotting it into a wave. Without D.8, expired trial users are
stuck in an undefined state (Pro features still showing in TUI
even though `expires_at` has passed).

## 8. Out of scope for Plan D

- **Skills marketplace** (Plan E)
- **LicenseSeat backend hardening** (Plan E — beyond what
  `start_trial` minimally needs)
- **Team v2 keys** (Plan E)
- **Refund / dispute flow** (out of v1 entirely)
- **Multi-device Pro license sharing** (v2; today's NodeLocked
  binding is one-machine-per-license)

## 9. Acceptance criteria for Plan-D survey → spec transition

- [x] Inventory the 3 sub-concerns (trial / upgrade / tease) with
      file:line citations (§§ 1–3)
- [x] Identify reusable primitives (`SpurLicense::activate`,
      `CollisionModal` template, `onboarding::maybe_prompt_first_run`)
- [x] Identify greenfield surfaces (§§ 1.4, 2.4)
- [x] Map dependency graph between Plans A→B→C→D (§4)
- [x] Enumerate cross-cutting risks (§6) including downgrade UX
- [x] Choose trial mechanism (client-only JWT per §5.1)
- [x] Propose wave segmentation grounded in dependency order (§7),
      including D.8 downgrade-with-grace flow
- [x] Triple-review by `worker://gemini` (architectural + spec
      fidelity), `worker://kimi` (citation grounding), and
      `worker://claude-code` (cross-doc consistency)

Reviewer findings applied inline:
- Removed `Plan::Trial` variant proposal (gemini 🔴: violates spec
  §6.2 "no new tier"; reuse `Plan::Pro` + `expires_at`)
- Added D.8 downgrade-with-grace wave (gemini 🔴: missing UX state
  transition)
- Fixed `licenseseat.rs:184-200` citation for `activate` (kimi 🔴:
  was 57-82 which is `new`)
- Fixed `main.rs:130-269` range to cover full `Commands` enum
  (kimi 🟡: was 188-269 mid-variant)
- Fixed `LicenseEventKind` parenthetical (kimi 🟡: removed
  non-existent `Refreshed` variant)
- §5.1 client-only-JWT decision committed (gemini)
- §6.4 deleted as misread (gemini)
- §6.5 changed to "Plan C ships first with typed FeatureGateError"
  (gemini)
