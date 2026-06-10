# Spur App SDK

This directory is the **source of truth** for the open-source Spur App SDK.
On release tags it is mirrored to a public repository (Apache-2.0 license).

See the design specs for full context (internal monorepo paths; not present in the public mirror):
- `docs/superpowers/specs/2026-06-10-app-platform-contract-design.ipynb` — App Platform Contract
- `docs/superpowers/specs/2026-06-10-spur-app-sdk-design.ipynb` — Spur App SDK design

## Directory layout

| Path | Status | Description |
|---|---|---|
| `typescript/` | exists | `@spur/app` TypeScript/Deno SDK |
| `fixtures/` | exists | Golden conformance fixtures (see below) |
| `python/` | planned (U2) | `spur_app` Python SDK |
| `skill/` | planned (U6) | Bundled Claude skill for SDK consumers |
| `examples/` | planned | Example Spur Apps |

## Fixture lockstep rule

`sdk/fixtures/port-store/` is a byte-for-byte copy of
`crates/spur-notebook/fixtures/port-store/`.  These golden files pin the
wire format that the Rust `PortStore` writer produces and that SDK language
readers must parse.

**The two directories must always be identical.**  CI enforces this via
`scripts/check-sdk-fixture-lockstep.sh` (INV-SDK-F1 in `.github/workflows/lint-invariants.yml`)
(internal monorepo paths; not present in the public mirror).

If you change the Rust port-store wire format, regenerate the SDK copy and
update any SDK reader tests before committing:

```sh
cp -R crates/spur-notebook/fixtures/port-store/. sdk/fixtures/port-store/
```

If you change the SDK copy directly, sync it back to the Rust side and update
the Rust round-trip test in `crates/spur-notebook/tests/`.
