# Versioning

This page documents how Spur app schema versioning, SDK version tracking, and
fixture compatibility work.

## Schema version

The manifest's `schema` field is the schema version identifier. Currently the
only valid value is `"spur.app/v1"`. The host rejects manifests with any other
value.

The Rust constant `SPUR_APP_SCHEMA = "spur.app/v1"` in
`crates/spur-notebook/src/spur_app.rs` is the single source of truth for this
string.

## SDK version tracking (planned)

> **Status: planned.** The mechanics below are the intended design from
> `2026-06-10-spur-app-sdk-design.ipynb §7`. They are NOT yet implemented in
> the SDK or the host. This section documents the plan so that future
> implementations follow the same design.

The design intent: **SDK minor version tracks the `spur.app` schema version.**

When the schema acquires a new minor version (e.g. `spur.app/v1.1`), the SDK
packages bump their minor version to match. Agents and tooling can infer the
expected schema version from the SDK version they have installed.

## sdk_min (planned)

> **Status: planned.** The `sdk_min` field does NOT exist in the current Rust
> `SpurAppManifest` type (verified: `crates/spur-notebook/src/spur_app.rs` has
> no `sdk_min` field). It is not included in the JSON Schema. When it ships,
> the schema will be updated.

The intended semantics: an optional manifest field declaring the minimum SDK
version required to develop this app. The doctor checks compatibility:

```json
{
  "schema": "spur.app/v1",
  "sdk_min": "0.2.0",
  ...
}
```

Doctor would fail if the installed SDK version is below `sdk_min`. The design
rationale mirrors `jute_min` (which was never enforced — `sdk_min` is intended
to fix that pattern).

## contract_version (planned)

> **Status: planned.** Not yet implemented anywhere.

The intended semantics: fixture files carry a `contract_version` integer.
SDKs refuse to read a fixture whose major `contract_version` is newer than the
SDK understands. This prevents silent data-corruption when the wire format has
a breaking change.

Example (intended future shape):

```json
{
  "contract_version": 1,
  "ports": { ... }
}
```

The SDK reader checks `contract_version` before parsing:
- Same major version → OK.
- Newer major version → raise `ContractVersionError("port-store contract v2 requires SDK >= 1.0.0")`.

## Fixture compatibility today

Today there is no `contract_version` in the fixture files. The lockstep
invariant (`INV-SDK-F1`) is the only compatibility gate: any wire-format change
that does not atomically update `sdk/fixtures/port-store/` and
`crates/spur-notebook/fixtures/port-store/` together fails CI.

See `sdk/docs/port-store.md` for the lockstep procedure.

## Additive-compatibility rule

The manifest root has `additionalProperties: true` — existing manifests without
new fields (e.g. `capabilities`, `skill`) continue to deserialize unchanged.

Only the `capabilities` inner object has `additionalProperties: false` (mirrors
the Rust `deny_unknown_fields`). This is intentionally strict: a capability the
host cannot provision must be declared, so the host can return a structured error
rather than silently ignoring it.
