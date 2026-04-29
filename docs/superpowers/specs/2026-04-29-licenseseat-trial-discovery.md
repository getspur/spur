# bd-22q.2 - LicenseSeat Trial Mechanism Discovery

**Status:** Re-dispatch discovery doc, authored 2026-04-29.
**Beads:** `bd-22q.2` (P1, parent epic `bd-22q`).
**Scope:** Discovery only. No source changes.
**Recommendation:** **backend-feature-required**.
**Confidence:** **medium** on trial capability: high that SPUR and the
`licenseseat = 0.5.3` SDK expose no first-class trial flow; medium on the
LicenseSeat backend because the schema can represent expiring entitlement
licenses, but the local code does not prove tenant/admin trial issuance.

## Executive Summary

SPUR can consume a time-limited Pro-like LicenseSeat license today, but cannot
issue one, request one, or distinguish it as a trial. The existing runtime model
has enough shape for "Pro until date X": `Plan::Pro` plus
`LicenseState.expires_at: Some(...)`. It does not have a `Trial` plan, trial
status, trial metadata field, or a CLI/TUI activation path beyond activating an
already-issued key.

For manually provisioned demo/trial keys, keep the CTA as
`spur auth login --key <TRIAL_KEY>`. Do not advertise `spur auth trial` or
`spur upgrade trial` until a backend issuer exists and the license shape is
agreed.

## Verified Facts (file:line)

- Version surface: `Cargo.toml:84` pins `licenseseat = "=0.5.3"` and
  `crates/spur-license/Cargo.toml:17` consumes it.
- End-user provider: `crates/spur-license/src/licenseseat.rs:17-28` configures
  publishable key + product slug only; `:184-199` activates an existing key;
  `:202-253` copies `result.license.expires_at`; `:411-436` hydrates cached
  `plan_key`, entitlements, and `expires_at`.
- Runtime state: `crates/spur-license/src/lib.rs:60-107` has no `Trial` plan;
  `crates/spur-license/src/tier.rs:14-21` maps Pro-like plans to `Tier::Pro`;
  `crates/spur-license/src/snapshot.rs:20-25` carries only `plan`,
  `expires_at`, and `is_offline` as source metadata.
- CLI surface: `crates/spur-cli/src/commands/auth.rs:15-40` exposes only
  `login/status/refresh/logout`; `:71-80` calls `license.activate(key)`;
  `crates/spur-cli/src/main.rs:245-249` wires `AuthCommands`.
- CTA surface: `crates/spur-license/src/upgrade_cta.rs:30-64`,
  `crates/spur-tui/src/components/upgrade_modal.rs:49-128`, and
  `crates/spur-tui/src/app.rs:1648-1666` all point at
  `spur auth login --key <KEY>`.
- SDK model: `licenseseat-0.5.3/src/models.rs:23-64` supports
  `expires_at` and metadata on license/entitlement objects; `:148-168` returns
  `LicenseResponse` from validation; `:251-266` caches a
  `trusted_license: Option<LicenseResponse>`.
- SDK client/events: `licenseseat-0.5.3/src/client.rs:143-178` activates an
  existing key; `:2005-2018` activation options are device/activation metadata;
  `:2236-2238` builds existing-license action paths; `events.rs:14-118` has no
  trial event.
- Admin surface: `crates/spur-license-admin/src/api.rs:42-75` and
  `crates/spur-license-admin/src/cli.rs:79-101` can create an operator license
  with `plan_key`, optional `email`, and optional `seats`, but no expiry,
  metadata, fingerprint dedupe, or customer-facing trial flow.
- Prior specs: `2026-04-18-spur-licensing-architecture.md:280-295,350-354`
  allows `spur auth trial` only if provider support exists;
  `2026-04-19-community-default-onboarding-design.md:592-602,662-663,729`
  frames demo keys as tenant-side config and defers per-user trials;
  `2026-04-26-individual-tier-revamp-design.md:735-775,1037-1047,1098-1115`
  contains trial product intent, not an implemented mechanism.

## Q1 - Does LicenseSeat Backend Support Trial Licenses Today?

**Verdict:** No first-class/proven trial support is present in SPUR or the
`licenseseat = 0.5.3` Rust SDK.

The SDK and SPUR runtime can represent a time-limited license. The key evidence
is `LicenseResponse.expires_at` and entitlement `expires_at`
(`licenseseat-0.5.3/src/models.rs:23-64`), plus SPUR copying
`result.license.expires_at` into `LicenseState`
(`crates/spur-license/src/licenseseat.rs:202-253`).

That is not enough to call trials plumbed. The end-user SDK only activates,
validates, heartbeats, and deactivates existing keys. The local admin client can
create licenses with `plan_key`, `email`, and `seats`, but exposes no expiry,
trial flag, metadata, or machine-fingerprint dedupe fields
(`crates/spur-license-admin/src/api.rs:42-75`).

So the safe answer is: LicenseSeat-like expiring Pro licenses are technically
representable; trial issuance and trial semantics are not proven by code.

## Q2 - What Does The Activation Flow Look Like For Trial?

**Verdict:** Today it is only `spur auth login --key <KEY>`.

The CLI has no trial command. `AuthCommands` is exactly `Login`, `Status`,
`Refresh`, and `Logout` (`crates/spur-cli/src/commands/auth.rs:15-40`), and
`login_inner` calls `license.activate(key)` (`crates/spur-cli/src/commands/auth.rs:71-80`).

The architecture spec already allowed `spur auth trial` only if provider support
exists (`docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md:280-295`).
The 2026-04-26 `spur upgrade trial` flow is therefore aspirational until an
issuer exists and the CLI command is added.

For a manually provisioned trial/demo key, the flow is: backend/operator creates
a Pro-like expiring key out of band; user runs
`spur auth login --key <TRIAL_KEY>`; SPUR activates through the normal
LicenseSeat path; validation/hydration carries `plan=Pro` and `expires_at`.

## Q3 - What Metadata Does Trial-State `LicenseState` Carry?

**Verdict:** A trial would map to `Plan::Pro` plus
`expires_at: Some(...)`, not a distinct trial variant.

`Plan` has Community, LTD variants, Pro, Team, Enterprise, and Unknown
(`crates/spur-license/src/lib.rs:60-70`). `LicenseState` carries `plan`,
`features`, `expires_at`, binding/offline flags, and status text
(`crates/spur-license/src/lib.rs:100-109`). The feature gate source metadata
keeps only `plan`, `expires_at`, and `is_offline`
(`crates/spur-license/src/snapshot.rs:20-25`).

The SDK model does include optional license and entitlement metadata
(`licenseseat-0.5.3/src/models.rs:23-64`), but SPUR does not retain that
metadata when building `LicenseState` from validation or cached license data
(`crates/spur-license/src/licenseseat.rs:202-253`,
`crates/spur-license/src/licenseseat.rs:411-436`). Without an explicit SPUR
field or a preserved metadata convention, a trial is indistinguishable from any
other expiring Pro license.

## Q4 - What CTA Copy Adjustments Are Needed?

**Verdict:** Keep `spur auth login --key <TRIAL_KEY>` for demo/manual trial
keys; advertise `spur auth trial` only after backend issuance exists.

The current CLI and TUI CTA surfaces all point at key activation:
`crates/spur-license/src/upgrade_cta.rs:30-64`,
`crates/spur-tui/src/components/upgrade_modal.rs:49-128`, and
`crates/spur-tui/src/app.rs:1648-1666`.

Recommended copy states:

- Existing/manual key: `Activate a license: spur auth login --key <KEY>`.
- Demo key campaign: `Try Pro with a demo key: spur auth login --key <TRIAL_KEY>`.
- Future self-service trial: only after issuer work lands, use a command such
  as `spur auth trial` or `spur upgrade trial`; choose one command family during
  implementation, then update CLI/TUI copy together.

Do not ship CTA text that implies no-card self-service trial availability until
dedupe/rate-limiting and issuance are real.

## Q5 - Backend Gaps

**Verdict:** Backend/product work is required before a self-service trial can be
shipped.

Required gaps:

- **Customer-facing issuance API:** safe trial request path without embedding
  `sk_*` credentials; existing admin create is operator-only.
- **Agreed license shape:** likely `plan_key=pro`, `expires_at=now+7d`,
  NodeLocked/machine-bound activation, and metadata such as
  `license_purpose=trial`, `trial_id`, and `issued_for_fingerprint_hash`.
- **Dedupe and abuse controls:** one trial per machine fingerprint, optional
  email/account dedupe, request rate limits, and no CI/floating trials.
- **Metadata propagation:** if UI must say "Pro trial", SPUR must preserve a
  trial discriminator from LicenseSeat metadata or another signed source.
- **CTA/command contract:** product must pick `spur auth trial` vs
  `spur upgrade trial` before CLI/TUI copy changes.
- **Tenant/API proof:** confirm whether LicenseSeat admin create accepts expiry
  and metadata fields; otherwise keep manual expiring keys or build a SPUR-owned
  issuer.

## Final Recommendation

Classify bd-22q.2 as **backend-feature-required**.

Do not treat the trial as "plumb existing capability." The nearest existing
capability is narrower: SPUR can activate and consume an expiring Pro-like
LicenseSeat key if something else issues it. Turning that into a self-service
trial requires backend issuance, dedupe/rate limiting, agreed metadata, and CLI
/ TUI copy changes after the backend contract is settled.
