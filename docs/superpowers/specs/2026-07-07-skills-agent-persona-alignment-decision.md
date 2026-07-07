# Skills & Agent-Persona Ecosystem Alignment — Decision

Date: 2026-07-07
Status: **Decided** — status quo confirmed with 5 surgical hardening items;
both big-ticket research recommendations refuted
Related: `2026-04-19-skills-installer-design.md`,
`2026-07-04-agent-profile-design.md`, `crates/spur-core/src/skills/`

## Problem

Deep research on the July-2026 skills / agent-persona ecosystem (Agent Skills
SKILL.md standard at agentskills.io, ~40 adopting tools; AGENTS.md under the
Agentic AI Foundation; no cross-tool persona standard; ToxicSkills-class
malicious-skill research) produced three recommendations for SPUR:

- R1: validate spec conformance in CI via the external `skills-ref` tool
- R2: keep the bespoke `.spur/agents` persona schema, borrow ecosystem
  conventions
- R3: adopt an AWS-style LLM-as-judge harness to verify adapter fidelity of
  the multi-tool skill fanout

This decision record is the result of a first-principles MCTS deliberation
challenging those recommendations against verified ground truth.

## Ground truth the deliberation rested on

- The skill fanout is **generative, not adaptive**: deterministic Rust
  adapters (`crates/spur-core/src/skills/adapters.rs`) render the canonical
  in-crate skills into 7 targets (`.claude/.codex/.gemini/.kiro/.opencode/
  .kimi/skills` + `.cursor/rules/*.mdc`), every emitted file carries a
  `<!-- SPUR-MANAGED v=1 skill=X sha256=... -->` marker, and 37 unit tests
  assert exact rendered output including determinism.
- Personas (`.spur/agents/*.md`) already use canonical Claude-markdown
  frontmatter extended with cross-vendor values (`model: gpt-5-codex`,
  `effort: high`) per the 2026-07-04 agent-profile design — the same
  compile-from-Claude-md architecture the ecosystem's flagship
  (wshobson/agents, 37.6k★) converged on independently.
- `AGENTS.md` exists at repo root; `GEMINI.md` does not.
- Cursor renders use `alwaysApply: true` — every spurpower rule body is
  forced into every Cursor request.
- `.claude/skills` contains unmanaged strata (marketing-*, unprefixed
  duplicates of managed skills).
- There is **no third-party skill ingestion path** today; all skills are
  first-party.

## Decision

### Refuted

- **R3 (LLM-as-judge fidelity harness): killed.** The AWS well-architected
  repo needs it because its per-tool adapters are hand-written and can drift
  semantically. SPUR's fanout is a deterministic transform with sha256
  provenance — the failure mode the judge detects cannot occur by
  construction, while the judge would add CI nondeterminism, API cost, and
  false confidence. The residual risk ("does the rendered format still
  satisfy the consuming tool") is a conformance question, not a fidelity one.
- **R1 (external `skills-ref` in CI): demoted.** Replaced by an in-repo Rust
  conformance test module (W1 below). An external Node-toolchain binary would
  add a provisioning surface to the remote build VM and worker sandboxes for
  semantics we can assert in ~50 dependency-free lines against what SPUR
  actually emits. `skills-ref` remains available for occasional manual local
  runs; it is not a standing process.
- **Also considered and rejected: migrating the installer to ruler/rulesync.**
  The adopt-don't-build principle (which decided the e2e layer) applies to
  undifferentiated infrastructure. The skills installer is differentiated
  product code: marker-guarded overwrites, skills compiled into the `spur`
  binary so `spur skills init` works with no Node present, role filtering,
  the Kiro steering pointer. Migration would add a dependency and lose
  capability.

### Confirmed

- **R2 (persona layer): confirmed.** No cross-tool persona standard exists
  (the one layer that never standardized), so canonical-Claude-markdown-plus-
  extensions materialized per worker kind is the correct architecture — the
  research validates the existing 2026-07-04 design rather than challenging
  it. Refinements: keep extension fields within the de facto union vocabulary
  (`name`/`description`/`tools`/`model`/`effort` — value-domain extensions,
  never field inventions, so conversion stays mechanical); least-privilege
  `tools:` on every persona; write descriptions routing-ready even though the
  brain routes explicitly today. Explicitly rejected: building a persona
  catalog ahead of need (persona-bloat anti-pattern; 2 personas is the right
  number until a metric or incident says otherwise).

## Surviving work package (all small)

| # | Item | Shape |
|---|---|---|
| W1 | Spec-conformance test module in `crates/spur-core/src/skills/` asserting the frozen spec core against canonical + rendered skills: name regex `^[a-z0-9]+(-[a-z0-9]+)*$`, name ≤64 chars, name == parent dir, description non-empty ≤1024, no non-spec top-level keys in rendered frontmatter (regression-guard the `role:` strip) | test-only, zero deps |
| W2 | Installer emits a marker-guarded 3-line `GEMINI.md` pointer to `AGENTS.md` (gemini workers read `GEMINI.md` by default and currently get no repo guidance) | small adapter |
| W3 | **Spike**: migrate the Cursor adapter from `.cursor/rules/*.mdc` + `alwaysApply: true` to native `.cursor/skills/` SKILL.md (Cursor ≥2.4 supports the standard). Hypothesis: kills the always-loaded context tax AND deletes the Cursor special-case transform (Cursor joins `render_agentskills`). Fallback for older Cursor: `.mdc` with `alwaysApply: false` + description-based activation | spike first, behavior change |
| W4 | Hygiene: reconcile unmanaged/duplicate strata in `.claude/skills` (e.g. `test-driven-development` vs `spurpower-test-driven-development` compete for activation) — adopt into the managed set or delete stale copies | cleanup |
| W5 | Standing policy: any future third-party skill ingestion feature requires pinned sha + human review as a hard design gate (ToxicSkills: 36% prompt-injection rate in sampled in-the-wild skills; SPUR-MANAGED sha256 markers already provide tamper-evidence) | docs/policy only |

## Watch triggers

1. **Conformance test fails after a spec bump** → the Agent Skills spec core
   moved (e.g., under AAIF governance); re-sync assertions by hand (~half
   day).
2. **Cursor deprecates `.cursor/rules` `.mdc`** → execute W3 immediately.
3. **Claude Code adopts AGENTS.md natively** → optionally collapse CLAUDE.md
   into an `@AGENTS.md` import; no urgency, nothing breaks.
4. **A skill-import/marketplace feature is proposed** → W5 policy becomes a
   blocking design-review gate.

## Why record a mostly-null decision

The refutations are the value: the AWS eval-harness and ruler/rulesync
patterns are exactly what the next reader of the ecosystem research would
pattern-match onto SPUR, and both are wrong here for structural reasons
(generative fanout; product-code installer). This record prevents
re-litigating them.
