# Tier Revamp Plan A — Status Hand-off

**Status:** ✅ Complete (Waves 1–9 + Final Wave)
**Date:** 2026-04-27
**Spec:** `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
**Plan:** `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-registry-expansion.md`

## What Plan A delivered

- **64 new typed `FeatureKey` constants** in `crates/spur-license/src/policy/feature_key.rs` (final shape after 9 design-pass waves)
- **1 new `QuotaKey` variant**: `BrainFailoverChainDepth`
- **Per-crate roundtrip tests** for every kept key, including negative assertions for absorbed/dropped/deferred keys (guards against accidental re-introduction)
- **Comprehensive 64-key roundtrip test** (Task 24): `tier_revamp_v1_keys_roundtrip` validates exact registry shape
- **Boundary comment marker** (Task 25) separating legacy 36-key block from Wave-9-final 64-key block in source
- **Total registry**: 100 typed `pub const` (36 legacy + 64 Wave-9 new)
- **Test count**: 26 in `policy::feature_key::tests`, all passing
- **Clippy**: clean with `-D warnings`

## Plan A wave-by-wave trajectory

| Wave | Date | Action | Net keys | Total |
|---|---|---|---|---|
| 1–4 | 2026-04-26 | Initial 135-key registry build | +135 | 135 |
| 5 | 2026-04-26 | First 4-reviewer pass: drop vaporware/duplicates | −12 | 123 |
| 6 | 2026-04-26 | L9-MCTS first-principles: drop always-on infra/security baselines | −16 | 107 |
| 7 | 2026-04-26 | Wave 7 4-reviewer: drop trait-impl variants + ghost notif keys | −8 | 99 |
| 8 | 2026-04-26 | Second-order composition: 15 family consolidations + 4 drops + 5 defers | −35 | 64 |
| 9 | 2026-04-27 | Iceberg+MCTS: 2 surgical Pro→Free tier shifts (with renames) | 0 | 64 |
| Final | 2026-04-27 | Comprehensive 64-key roundtrip + boundary marker | 0 | 64 |

## Final tier composition (post-Wave-9)

```
Free   (48)  ← daily-driver baseline; covers solo-dev complete workflow
Pro v1 (15)  ← 5 ★ headline conversion triggers (Remote Control,
              Multi-Agent Coordination, Review Control Plane, Cost
              Insights, Extensibility)
Pro v1.1 (1) ← session_resume_event_replay (Q3 roadmap)
─────────────
Total   64
```

## What Plan A did NOT change

- `crates/spur-license/resources/default_policy.json` (still references legacy 36 keys)
- `crates/spur-license/build.rs` (build-time policy verification still uses legacy schema)
- `crates/spur-license/src/gate.rs` (`FeatureGate::has` API unchanged; new keys go through the same path)
- `crates/spur-license/src/licenseseat.rs` (no trial flow yet — Plan D)
- Any consumer crate (`spur-core`, `spur-mcp`, `spur-pm`, `spur-tui`, etc.) — no `FeatureGate::require()` calls added yet (Plan C)
- CLI commands — `spur upgrade trial` and `spur upgrade pro` not implemented (Plan D)

## Behavioral state after Plan A

- **Free users**: identical experience to pre-Plan A (legacy 11-key Community policy still active)
- **Pro users** (if any exist): identical experience (legacy 8 Pro keys still active)
- **New 64 keys**: typed-known but unreachable through `FeatureGate::has()` because no policy declares them in any tier
- Workspace builds clean; clippy passes; all `policy::feature_key` tests green

## Plan B prerequisites (verify before starting Plan B)

- [x] All Plan A waves committed (Waves 1–9 + Final Wave)
- [x] `cargo test --package spur-license --lib policy::feature_key` passes (26 tests)
- [x] `cargo clippy --package spur-license --lib -- -D warnings` passes
- [x] Git log shows the wave-progression commits with `tier revamp Plan A` in message

## What Plan B will do

1. Rewrite `crates/spur-license/resources/default_policy.json` per spec §5 (with Wave-9-final 64-key tier composition: 48 Free + 15 Pro v1 + 1 Pro v1.1 + 0 Team)
2. Extend `PolicyResolver` to handle `@inherit:community` directive
3. Extend `PolicyResolver` to handle `v1_1_q3_roadmap` field
4. Re-sign with `spur-policy-2026-04` Ed25519 key (use `scripts/sign-policy.sh`)
5. Update `build.rs` compile-time check to validate new schema
6. Migrate existing call sites that reference legacy keys (see spec §8.2 rename map)
7. Remove legacy 36 keys from `feature_key.rs` after migration completes (delete the boundary block above)
8. Update `from_known()` to no longer parse legacy keys

After Plan B ships, the registry has only the 64 Wave-9 new keys and the policy reflects the new tier structure.

## Known unrelated issue

`crates/spur-license/tests/feature_gate.rs` references `PolicyResolver::from_document` which doesn't exist in the current `PolicyResolver` API (E0599). This is **pre-existing** (failed identically with or without Plan A changes); not introduced by any Plan A wave. Tracked separately for Plan B to address as part of the policy rewrite.

## Spec sections worth re-reading before starting Plans B–E

- **§4.15 Registry summary** — final tier counts (48F/15P/1Pv1.1/0T)
- **§4.16 Deferred-keys backlog** — full audit trail of every dropped/deferred/consolidated key with code-grounded reasoning + wave attribution (10+ Wave-1-through-9 subsections)
- **§9.1 Comparison Table** — Wave-9-corrected marketing copy (no ghost adapters; graph_tools in Free; review retry config in Free)
- **§9.5 Iceberg framework analysis** — 4-persona model with B2D-realistic 2-12% conversion baseline + 5 Pro headline groups + explicit Plan D/E deferrals (trial mechanism, capability teases, skills marketplace, Team tier pricing)
