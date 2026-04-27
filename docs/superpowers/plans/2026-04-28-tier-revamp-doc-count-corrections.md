# Tier Revamp — Doc-Count Cascade Fix Follow-up

Date: 2026-04-28

Scope: documentation hygiene only. No code change. Captures two
pre-existing miscounts in the tier-revamp doc tree that the Plan
C/D/E surveys' triple-review process surfaced. Filing as a
follow-up rather than handling inline because both miscounts
predate the survey work and span more than one upstream doc.

## Origin

- **Plan C survey kimi audit (2026-04-28):** registry has 63
  typed `pub const`s, not 64. The block comment at
  `crates/spur-license/src/policy/feature_key.rs:28` is correct
  ("Wave-9 final shape: 63 keys"); upstream doc text drifted.
  Tier breakdown: **47 Free + 15 Pro v1 + 1 Pro v1.1
  (`core_pro_session_resume_event_replay`) + 0 Team = 63**.
- **Plan E survey kimi audit (2026-04-28):** the bundled-skills
  HashMap at `crates/spur-core/src/skills/mod.rs:19-80` contains
  19 distinct skill IDs (one alias entry: `brain-delegation-claude-code`
  ↔ `brain-delegation-claude-code-acp` map to the same content).
  Spec text says "17 bundled skills × 7 render targets"; the
  count drifted as adapter additions landed.

Both errors propagated through copy-paste across multiple docs
and through marketing copy in the spec. The errors do NOT affect
runtime behavior — they are entirely documentation drift.

## Cascade — files to update

### 63-key correction (47F/15P/1Pv1.1/0T)

| File | Search → Replace | Notes |
|---|---|---|
| `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md` §4.15 | "48F + 15P + 1Pv1.1 + 0T" / "Total 64" → "47F + 15P + 1Pv1.1 + 0T = 63" | Authoritative spec — fix here first |
| `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-status.md` lines 12, 27-28, 33-41 | `64` → `63`, `48F` → `47F`, "Wave 9: 0 +0 64" → "Wave 9: 0 +0 63", "Total 64" → "Total 63" | Status doc; the wave-9 +0 net-change row is correct, just the running total |
| `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-registry-expansion.md` | global `64`→`63` where the count is asserted | Plan A's own implementation plan |
| `docs/superpowers/plans/2026-04-26-tier-revamp-plan-b-survey.md` | check for any "64-key" / "48F" references | Plan B carried the Plan A miscount forward |

The Plan C/D/E surveys (this batch) already use 63 and 47F/15P/1Pv1.1
post-review.

### 17→19 bundled-skills correction

| File | Search → Replace | Notes |
|---|---|---|
| `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md` §4.4 + line 18 | "17 bundled skills × 7 render targets Free" → "19 bundled skills × N render targets Free" | The 7-render-targets number also predates the kimi/codex/gemini adapter splits; verify against `crates/spur-core/src/skills/adapters.rs` before patching |
| `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-registry-expansion.md` | "17 bundled" → "19 bundled" (if present) | |
| `docs/SPUR_PRD.md` | check for any "17 skills" copy in marketing text | Possible ripple |

## Out of scope

- Changing any code. The registry, skill loader, and policy file
  are correct; only documentation drifted.
- Changing tier composition or registry shape. The 47F/15P/1Pv1.1/0T
  composition is the authoritative one and was the working assumption
  during Wave-9 design — only the *running total annotation* is wrong.
- Skill semantics / render-target enumeration. The 7-targets number
  may also be stale (claude-code-acp / kiro / codex / gemini adapters
  shipped after the spec was written) but that's a separate audit.

## Suggested execution

One PR titled `docs(spec): tier-revamp doc-count cascade fix
(63 keys, 19 bundled skills)`. ~20 line touches across 4-5 files.
No reviewer round-trip needed — purely mechanical.

## References

- Plan C survey kimi audit (delegation_id `c4c297dd-...`)
- Plan E survey kimi audit (delegation_id `a2db6c0f-...`)
- Plan C survey grep verification:
  `crates/spur-license/src/policy/feature_key.rs:28-135` contains
  exactly 63 `pub const` declarations.
- Plan E survey grep verification:
  `crates/spur-core/src/skills/mod.rs:19-80` contains 20 HashMap
  insertions of which two (`brain-delegation-claude-code{,-acp}`)
  alias to the same content, yielding 19 distinct skills.
