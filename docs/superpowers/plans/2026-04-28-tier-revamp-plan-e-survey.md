# Tier Revamp Plan E — Marketplace / LicenseSeat Hardening / Team v2 Survey

Date: 2026-04-28

Scope: survey only. Plan E is the most-greenfield of the tier-revamp
plans. Spec §9.5 explicitly defers three Plan-E concerns:

1. **Skills marketplace** — publish/discover surface beyond today's
   bundled-17 + per-project-overrides shape.
2. **LicenseSeat backend hardening** — server-side state needed
   for Plan D trial anti-abuse, revocation push, and v2 readiness.
3. **Team v2 reservation** — spec §4.15 ships **0 Team-tier keys**
   in v1; the Team type-system surface (`Plan::Team`, `Tier::Team`,
   `QuotaKey::MaxTeamMembers`) exists but has no entitlements to grant.

Plan E is the longest-cycle plan (server-side work in particular)
and the lowest-priority for revenue v1.x. The point of this survey
is to inventory the *forward-compat surface* needed in v1 so that
v2 Team-tier work doesn't require breaking renames or schema rewrites.

## Grounding commands

- `find` over `crates/spur-core/src/skills/` (the bundled-skills crate
  layout) and `ls` of the 17 SKILL.md sources at `mod.rs:19-80`.
- Reads of `crates/spur-license/src/licenseseat.rs:100-286` (the
  full `LicenseProvider` impl: `activate` / `validate` /
  `heartbeat` / `deactivate`) and `crates/spur-license/src/lib.rs:60-69`
  (Plan enum), `:185-200` (LicenseEventKind).
- Read of `crates/spur-license/src/quota.rs:7,17` (the existing
  `MaxTeamMembers` quota key) and `crates/spur-license/src/tier.rs:9,28`
  (`Tier::Team`).
- `rg "team|Team|TEAM|RBAC|sso"` over `crates/spur-license/`.
- Read of spec `§4.16` Wave-9 v2 backlog (lines 409-426) and
  `§4.6` PM keys with deferred `pm_team_webhooks` (lines 240-256).
- Cargo.toml license dependency: `licenseseat = "=0.5.3"`.

## Summary finding

- **Skills marketplace: 0% built.** 17 bundled skills are
  `include_str!`-baked at `crates/spur-core/src/skills/mod.rs:19-80`;
  per-project `.spur/skills/` overrides win. There is no remote
  registry, no publish CLI, no discovery search, no signature/trust
  model, no semver model.
- **LicenseSeat hardening: 60% built (foundation).** The
  external crate `licenseseat = "=0.5.3"` already provides activate/
  validate/heartbeat/deactivate with NodeLocked binding. What's
  missing is *server-side state* for (a) trial-fingerprint allowlist
  (Plan D dependency), (b) revocation polling enforcement, (c)
  offline-grace policy. Two registry keys (`LICENSE_PRO_REVOCATION_POLLING`,
  `LICENSE_PRO_OFFLINE_GRACE`) are typed-known but have zero
  callsites today.
- **Team v2 reservation: 20% built (type system).** `Plan::Team`,
  `Tier::Team`, `QuotaKey::MaxTeamMembers` exist; the registry
  declares **0 Team-tier feature keys** per spec §4.15 (final
  composition: 47F + 15P + 1Pv1.1 + 0T = 63 keys, per Plan C
  survey's authoritative correction). Legacy resolver returns
  `None` for `team_cost_dashboard` / `rbac` / `sso_saml`
  (`crates/spur-license/src/gate.rs:254-259`). v2 backlog has
  3 explicitly-named team keys (§4.16, see §3.1 below).

The right framing for Plan E: **Plan E is the v1.x→v2 surface
preparation**. The work that *must* happen in v1.x (so v2 doesn't
need a breaking change) is small. The work that *should* happen
in v2 (skills publish/discover, RBAC, multi-tenant licenseseat) is
large but out-of-scope for the current spec.

## 1. Skills Marketplace Inventory

### 1.1 Today's skills runtime

| Concern | File:line | State |
|---|---|---|
| Bundled-skills list | `crates/spur-core/src/skills/mod.rs:19-80` | 19 distinct skills via `include_str!` macro (the spec's "17" count predates `brain-delegation-claude-code{,-acp}` alias + `brain-delegation-gemini` adapter additions; cascade-fix follow-up) |
| Per-project override loader | `crates/spur-core/src/skills/mod.rs` (full file) | `.spur/skills/<id>/SKILL.md` takes precedence |
| Per-adapter renderer | `crates/spur-core/src/skills/installer.rs:263+` (the `pub fn run(repo_root)` entry; full installer file is ~656 lines including atomic-write + decide + render helpers) | renders bundled+overrides into `.<adapter>/skills/` per-vendor dirs |
| User-edit protection | `installer.rs:13-46` | `<!-- SPUR-MANAGED v=1 skill=… sha256=… -->` marker, sha256 hash protects against silent overwrites |
| `SKILLS_PRO_CUSTOM` registry entry | `crates/spur-license/src/policy/feature_key.rs:72,195` | typed key exists; no production callsite (per Plan C survey §1.3) |
| `SKILLS_CORE_REGISTRY` registry entry | `feature_key.rs:71,193` | typed key exists; no production callsite |

### 1.2 What spec §9.5 means by "marketplace"

Spec §9.5 (line 1040): *"Skills marketplace publish/discover (fully
greenfield; only local bundled+overrides today)"*. The marketplace
implies:

- Publish surface: a way for a user to push a skill to a registry
- Discover surface: a way to search/browse remotely-published skills
- Trust model: skill authorship signing + integrity checking
- Versioning: skill semver + dependency declaration
- License model: free vs paid skills
- Skill data model: how does a "remote skill" merge with bundled+overrides

### 1.3 What is greenfield (most of it)

| Surface | Today | Plan E v1.x | Plan E v2 |
|---|---|---|---|
| Remote registry server | n/a | n/a | full thing |
| `spur skills publish <path>` CLI | n/a | maybe stub | full impl |
| `spur skills search <query>` CLI | n/a | n/a | full |
| `spur skills install <id>` CLI | only `Skills::Init` exists (`main.rs:273-276`) | extend to remote install? | full |
| Skill manifest (`SKILL.toml`?) | only frontmatter in `SKILL.md` | n/a | full |
| Trust/sig model | n/a | n/a | ed25519-signed manifest? |
| Skill semver | n/a | n/a | full |
| `skills_pro_custom` enforcement point | none | needed for v1 (per Plan C **C.14 Extensibility wave**) | already prepared |

### 1.4 What v1 must reserve (forward-compat)

The minimal v1.x reservation surface that lets v2 marketplace land
without breaking v1:

- **`SKILLS_PRO_CUSTOM` enforcement point** in
  `crates/spur-core/src/skills/installer.rs` so org-internal skill
  load *already* checks the gate. v2 marketplace install path can
  reuse this gate without inventing a new one. (This is technically
  Plan C work; included here only to flag the dependency.)
- **`SKILL.md` frontmatter schema versioning** — today's bundled
  skills use ad-hoc frontmatter. Plan E should specify a versioned
  schema (`spec_version: 1`) so v2 marketplace skills can declare
  forward compatibility.
- **Hardcoded load-priority for the 19 bundled skill IDs**
  (the wedge against marketplace overrides). Today the loader
  takes `.spur/skills/<id>/SKILL.md` as override of the bundled
  skill. v1.x must enforce that the 19 names listed in
  `crates/spur-core/src/skills/mod.rs:19-80` (the `bundled_raw()`
  HashMap) are **non-overridable**: a `.spur/skills/brain-delegation/`
  override is silently ignored with a `tracing::warn!`. This is
  more robust than a namespace-prefix wedge (per gemini's review):
  a `bundled.*` prefix is trivially spoofable by a malicious local
  override (`.spur/skills/bundled.brain-delegation/`), whereas a
  hardcoded name allowlist cannot be spoofed.

That's the entire Plan E v1.x scope for skills. Three small
reservation tasks, no marketplace surface.

## 2. LicenseSeat Backend Hardening Inventory

### 2.1 Today's licenseseat surface

| Concern | File:line | State |
|---|---|---|
| External SDK | `Cargo.toml:84` | `licenseseat = "=0.5.3"` (pinned) |
| `LicenseSeatProvider::activate` | `crates/spur-license/src/licenseseat.rs:184-200` | calls `sdk.activate(key)`, sets `LicenseEventKind::Activated` |
| `LicenseSeatProvider::validate` | `licenseseat.rs:202-254` | calls `sdk.validate()`, returns expires_at + warnings + entitlements |
| `LicenseSeatProvider::heartbeat` | `licenseseat.rs:256-275` | degrades on failure, logs `LicenseEventKind::HeartbeatFailed` |
| `LicenseSeatProvider::deactivate` | `licenseseat.rs:277-285` | calls `sdk.deactivate()`, fires `Deactivated` event |
| Revocation polling | sdk-internal; see `LICENSE_PRO_REVOCATION_POLLING` registry key | typed-known, no callsite |
| Offline grace | sdk-internal; `LICENSE_PRO_OFFLINE_GRACE` | typed-known, no callsite |
| Event broadcast | `licenseseat.rs:84-93` is `replace_state`; the SDK→provider bridge is at `licenseseat.rs:95-128` (`spawn_sdk_event_bridge`) | broadcast::Sender<LicenseEvent> with bridge from SDK |
| Handler-originated dedup | `licenseseat.rs:104-128` | `is_handler_originated` filter for activate/validate/heartbeat/deactivate (see RCA `docs/rca/2026-04-19-licenseseat-emission-audit.md`) |

### 2.2 Plan-D dependencies on Plan E

Plan D §5.1 committed to **client-only JWT trial** (no server-side
state required for v1). The Plan-D dependencies on Plan E shrink
accordingly:

- **`LicenseProvider::start_trial` trait method** —
  `crates/spur-license/src/provider.rs:30-50` has no such method;
  Plan D D.1 adds it. Implementation per Plan D §5.1 mints a
  7-day `Plan::Pro` JWT locally and calls existing `activate`.
- **`LicenseEventKind::TrialStarted` / `TrialExpired` variants** —
  used by Plan D D.9 telemetry. Lands as part of Plan D D.1 (the
  origin E.6 wave was absorbed into Plan D per §6.2 below) since
  the variant addition couples directly to `start_trial` emission.
- **Trial-expiry signal** — Plan D D.7+D.8 wire this; the
  LicenseSeat provider already surfaces `expires_at` deltas via
  existing `LicenseEvent` channel.

**Server-side trial allowlist** (originally proposed here as a
trial-anti-abuse mechanism) is now wholly v2-only — Plan D §5.1
mitigates trial abuse via the local-data-destruction cost
(`~/.spur/.beads/`) instead. See §7 for v2 boundary.

### 2.3 Plan E v1.x scope for licenseseat hardening

| Task | Why v1.x not v2 | Effort |
|---|---|---|
| Wire `LICENSE_PRO_REVOCATION_POLLING` enforcement | The polling task is currently always-on; without enforcement the gate is meaningless | small (Plan C C.4 prerequisite) |
| Wire `LICENSE_PRO_OFFLINE_GRACE` enforcement | Same — the offline-grace policy needs a runtime enforcement point | small (Plan C C.4 prerequisite) |
| Add `LicenseEventKind::TrialStarted` / `TrialExpired` | Plan D dependency | tiny |
| ~~Extend `Plan` enum with `Plan::Trial`~~ | **Removed** — per Plan D §1.2 / spec §6.2, trial reuses existing `Plan::Pro` + `expires_at`, no new tier | n/a |
| Document SDK-version policy (when do we upgrade `licenseseat = 0.5.3`?) | Forward-compat | doc-only |

What is **out** of v1.x and lives in v2:

- Custom out-of-band trial endpoint (server-side counter)
- Multi-tenant LicenseSeat (one Team license, many seats)
- Revocation push (today is poll-based)
- Offline-grace policy override per-license

### 2.4 Bootstrap concern (re-flagged from Plan C survey §6.3)

`LICENSE_PRO_REVOCATION_POLLING` and `LICENSE_PRO_OFFLINE_GRACE`
*gate the license subsystem itself*. If the gate is consulted before
the snapshot is built, fail-closed default kills polling for Pro
users. Resolution must be specified in the Plan C spec phase
(bootstrap-time gates fail open for the license subsystem itself);
Plan E inherits the contract.

## 3. Team v2 Reservation Inventory

### 3.1 Today's Team surface

| Concern | File:line | State |
|---|---|---|
| `Plan::Team` variant | `crates/spur-license/src/lib.rs:66,79,92` | exists; rendered as "Team" |
| `Tier::Team` variant | `crates/spur-license/src/tier.rs:9,18,28` | exists |
| `QuotaKey::MaxTeamMembers` | `crates/spur-license/src/quota.rs:7,17,27` | exists; quota slot for v2 seat counter |
| `Tier::Team` gate arm | `crates/spur-license/src/gate.rs:162` | exists; receives Team-tier features when present |
| Team feature keys in registry | `crates/spur-license/src/policy/feature_key.rs:28-135` | **0 keys** — spec §4.15 ships 0T |
| Legacy team-keys in resolver | `gate.rs:254,256,259` | `team_cost_dashboard`, `rbac`, `sso_saml` all return `None` |

### 3.2 Spec §4.16 v2 backlog Team keys

Three Team-tier keys are explicitly named for v2:

| Key | Why deferred | File reference |
|---|---|---|
| `cli_team_command_workflow` | Phase 3 print-only stub; Team-only | spec §4.16 line 414 |
| `pm_team_webhooks` | Vaporware (no receiver implementation in spur-pm) | spec §4.16 line 417, §4.6 line 246 |
| `bot_team_multi_chat` | Single `operator_user_id` config at `crates/spur-acp/src/config/mod.rs:326`; multi-chat requires multi-user RBAC | spec §4.16 line 419 |

Plus from spec §4.16 implicit:

- `team_cost_dashboard` (legacy mapping → `None` today; v2 candidate)
- `rbac` (legacy → `None`; v2)
- `sso_saml` (legacy → `None`; v2)
- `shared_lineage` (legacy → `None`; v2)
- `centralized_config` (legacy → `None`; v2)
- `shared_review_queue` (legacy → `None`; v2)

### 3.3 Plan E v1.x reservation tasks

The minimum surface to land in v1.x (so v2 doesn't need breaking
changes) is small. Per gemini's advisory, the stub list reserves
**all 9 known Team v2 keys**, not just the 3 explicit names — this
prevents v1.x users from minting any of `rbac` / `sso_saml` /
`shared_lineage` / `team_cost_dashboard` / `centralized_config` /
`shared_review_queue` and colliding with v2:

| Task | Why now | Effort |
|---|---|---|
| Document Team-tier naming convention in spec §3 | Existing `<crate>_<tier>_<capability>` is correct; just confirm `<tier> = team` is reserved | doc-only |
| Add 9 named Team v2 stub `pub const`s with `[v2]` status comment: 3 explicit (`cli_team_command_workflow`, `pm_team_webhooks`, `bot_team_multi_chat`) + 6 implicit-from-§3.2 (`rbac`, `sso_saml`, `shared_lineage`, `team_cost_dashboard`, `centralized_config`, `shared_review_queue`) in `feature_key.rs` | Reserves the names; prevents v1.x users from minting same names | tiny (15-20 lines) |
| Stub `Tier::Team` policy section in `default_policy.json` | Today the policy file has community + pro tiers; adding an empty `team` tier is forward-compat | small |
| Add `assert_eq!(team_keys_active_in_policy.count(), 0)` test | Locks v1's "0 Team keys active" promise so v2 has to explicitly increment | tiny |
| Document the v2 Team-tier roadmap in the spec | Plan E is the right place to pin "what does Team unlock?" | doc-only |

What is **out** of v1.x and lives in v2:

- Actual Team-tier capabilities (RBAC, shared lineage, audit logs,
  multi-operator bot)
- Team-tier provisioning UX (`spur team create`, `spur team invite`)
- Multi-seat licenseseat coordination

### 3.4 Naming-convention drift risk

Today the registry has:
- 13 crates × {`core`, `pro`} = 26 prefix combos in active use
- 0 `<crate>_team_*` keys in active use

Plan E should reserve specific Team prefixes that won't collide with
v2. Recommended: pin `team_*` to **infrastructure-level features
that span crates** (RBAC, audit, multi-tenant) and `<crate>_team_*`
to crate-local Team capabilities (e.g., `bot_team_multi_chat`,
`pm_team_webhooks`). Spec §3 is the convention authority.

## 4. Plan-E Internal Dependency Graph

```
                Plan A (registry types)
                       │
                       ▼
                Plan B (signed policy schema)
                       │
                       ├──────────────────┐
                       ▼                  │
                Plan C (enforcement)      │
                       │                  │
                       ▼                  │
              ┌────────┴───────┐          │
              ▼                ▼          ▼
        E.h hygiene PR    [E.7→Plan C]  [E.6→Plan D]
        (9 Team stubs +   license_pro_*  LicenseEventKind
         policy stub +    enforcement   trial events
         load-priority +  absorbed into  absorbed into
         frontmatter      C wave C.4    D wave D.1)
         versioning +
         SDK doc)
              │
              ▼
        Plan D (trial) — receives Plan C typed
        FeatureGateError contract; mints Plan::Pro
        7-day JWTs; surfaces tease modals
```

The hygiene PR (E.h) has no dependency on Plans C or D and can
land any time after Plan A. The two absorbed waves (former E.7,
E.6) ship as part of Plans C and D respectively.

## 5. Risks

### 5.1 Premature v2 surface

The temptation is to over-design v1.x reservation surface "just in
case". Plan E should be **minimal**: only land what blocks v2's
non-breaking growth. Anything else is YAGNI.

Mitigation: every Plan E v1.x task in §1.4 / §2.3 / §3.3 is small
(under 100 LoC) and reversible.

### 5.2 SDK pinning trap

`licenseseat = "=0.5.3"` is exact-pinned. If LicenseSeat upstream
ships server-side trial endpoints, we cannot upgrade without a
spec-level decision. Plan E should specify the SDK upgrade policy:
(a) bump major versions in dedicated PRs only, (b) require RCA-level
review of every upstream changelog before bumping, (c) maintain
compatibility tests in `crates/spur-license/tests/` against the
pinned version.

### 5.3 Marketplace as wedge against bundled skills

Today's bundled-17 are deeply embedded (`include_str!`). A v2
marketplace will tempt users to override every bundled skill with a
remote variant — including ones critical to brain prompt assembly
(`brain-delegation`, `spur-way`). Plan E v1.x should reserve a
`bundled.*` skill_id prefix that the marketplace cannot override.
This is the single most important forward-compat decision in §1.4.

### 5.4 Team v2 type-system bloat

Adding `Plan::Team` variants and `Tier::Team` arms today
*without enforcement* leaves dead-code branches. Clippy may flag
`#[allow(dead_code)]` proliferation. Plan E should specify a
narrow `[v2]` boundary comment marker like Plan A's Wave-9 boundary:

```rust
// === Tier revamp v2 reserved keys (2026-04-28) ===
// These keys are typed-known but not in any policy. Removing the
// `[v2]` comment and adding to default_policy.json activates them.
```

### 5.5 Skills frontmatter schema break

Today's bundled skills have ad-hoc frontmatter. If Plan E ships a
versioned schema (`spec_version: 1`) but the bundled-17 don't carry
one, every loader call must default-to-v1. Spec must specify the
default-version policy.

## 6. Plan-E dissolution — fragments absorbed into Plans C and D

**Decision (post-gemini-review):** Plan E is not a cohesive
standalone feature — it is a grab-bag of prerequisites and
forward-compat stubs. The original 8-wave segmentation created a
circular dependency (E.7 enforcement claimed to depend on Plan C
C.4 while purporting to live in Plan E) and double-counted work
that belongs upstream. Plan E dissolves into:

### 6.1 Move to Plan C (runtime enforcement)

| Origin wave | New home | Rationale |
|---|---|---|
| E.7 (`LICENSE_PRO_REVOCATION_POLLING` + `LICENSE_PRO_OFFLINE_GRACE` enforcement) | Plan C wave C.4 | These are registry-key enforcement tasks. Plan C is the enforcement-sweep plan; license-meta keys are no different from any other Pro keys. |

### 6.2 Move to Plan D (trial mechanism)

| Origin wave | New home | Rationale |
|---|---|---|
| E.6 (`LicenseEventKind::TrialStarted` + `TrialExpired` variants) | Plan D D.1 | `start_trial` provider method (D.1) needs the event kinds before it can emit them. Co-locating shrinks D.1's review surface by zero net cost. |

The `Plan::Trial` variant from the original E.6 was already removed
per spec §6.2 (no new tier; trial reuses `Plan::Pro` + `expires_at`).

### 6.3 Single v1.x hygiene PR (≤300 LoC)

| Wave | Scope | Risk |
|---|---|---|
| E.h.1 | Add 9 `[v2]` Team-tier stub `pub const`s (full §3.2 list) + 0-key-active invariant test | trivial |
| E.h.2 | Add empty `team` tier section to `default_policy.json` schema | small |
| E.h.3 | Document Team-tier naming convention + v2 roadmap in spec §3 / §4.16 | doc-only |
| E.h.4 | Hardcoded load-priority for the 19 bundled skill IDs (override-blocking; see §1.4 + §5.3 strengthened wedge) | small |
| E.h.5 | Specify SKILL.md frontmatter `spec_version: 1` schema; default-to-v1 in loader | small |
| E.h.6 | Document SDK upgrade policy + compat tests for `licenseseat = 0.5.3` | doc + tests |

6 small tasks ship as one hygiene PR. Total estimated effort:
under 300 LoC across registry stubs, policy schema, skill loader,
and docs.

### 6.4 v2-only (deferred)

- Skills marketplace publish/discover surface
- Multi-tenant LicenseSeat (Team license → many seats)
- Revocation push (vs poll)
- Actual Team-tier capabilities (RBAC, shared lineage, audit logs,
  multi-operator bot)
- Linear/Plane PM adapters
- Custom worktree policies, TUI custom keybindings

(Note: the original v2 backlog item "server-side trial fingerprint
allowlist" is now **obsolete**, not deferred — Plan D §5.1's
client-only-JWT + local-data-destruction-cost mitigation supersedes
that mechanism.)

### 6.5 Strategic verdict (gemini)

> *Plan E is not a cohesive standalone feature; it is a grab-bag of
> prerequisites and forward-compat stubs. The actual implementations
> (Marketplace, Team tier, multi-seat licensing) should be strictly
> deferred to v2. However, the preparation fragments must ship in
> v1.x: E.6 and E.7 are immediate blockers that should be absorbed
> into Plans D and C, respectively. The remaining reservation stubs
> (E.1-E.5) are so trivial (<100 LoC) they should be shipped in v1.x
> as a single hygiene PR to prevent breaking schema changes and
> naming collisions later.*

This survey adopts that recommendation in full.

## 7. Out of scope for Plan E v1.x (v2 only)

- Skills marketplace publish/discover surface
- Revocation push (vs poll)
- Multi-tenant LicenseSeat (Team license → many seats)
- RBAC, audit logs, SSO/SAML, shared lineage, shared review queue
- Multi-operator Telegram bot (`bot_team_multi_chat`)
- Linear/Plane PM adapters (`pm_pro_linear_sync`, `pm_pro_plane_sync`)
- Custom worktree policies (`worktree_pro_custom_policies`)
- TUI custom keybindings (`tui_pro_custom_keybindings`)

**Obsolete (explicitly NOT a v2 deferral):**
- ~~Server-side trial fingerprint allowlist~~ — superseded by Plan
  D §5.1's client-only-JWT + local-data-destruction-cost
  mitigation.

## 8. Acceptance criteria for Plan-E survey → spec transition

- [x] Inventory the 3 sub-concerns (marketplace / licenseseat /
      team-v2) with file:line citations (§§1–3)
- [x] Identify reusable primitives (frontmatter loader, NodeLocked
      binding, Plan/Tier/QuotaKey type system already on Team)
- [x] Identify forward-compat reservation surface (§1.4, §2.3, §3.3)
- [x] Map dependency graph (§4)
- [x] Enumerate risks with explicit YAGNI guard (§5.1)
- [x] Dissolve Plan E as standalone phase per gemini's strategic
      verdict; absorb fragments into Plans C/D + one v1.x hygiene
      PR (§6)
- [x] Explicitly enumerate v2-only items so v1.x scope stays small
      (§7); flag obsolete-but-not-deferred items separately
- [x] Triple-review by `worker://gemini` (architectural correctness
      + v1.x-vs-v2 boundary discipline), `worker://kimi`
      (callsite-grounding audit), and `worker://claude-code`
      (cross-doc consistency)

Reviewer findings applied inline:
- E.7 license enforcement moved to Plan C wave C.4 (gemini 🔴: was
  circular-dep on Plan C while purporting to live in Plan E)
- E.6 LicenseEventKind variants moved to Plan D D.1 (gemini 🔴:
  Plan D needs the variants before emitting them)
- §1.4 wedge strengthened from `bundled.*` namespace prefix to
  hardcoded load-priority for the 19 bundled IDs (gemini 🟡:
  prefix is spoofable)
- §3.3 Team stubs expanded from 3 to 9 keys covering the full v2
  backlog (gemini 🟡)
- §7 server-side trial allowlist marked obsolete, not v2-deferred
  (gemini 🔴 ghost-requirement)
- §1.1 installer.rs renderer citation fixed to `:263+` (kimi 🔴:
  was `:1-80` which is marker/sha256 helpers)
- §1.1 bundled-skills count corrected to 19 (kimi 🟡: spec said 17,
  cascade-fix follow-up)
- §2.1 licenseseat.rs:95-128 bridge attribution fixed (kimi 🟡)
