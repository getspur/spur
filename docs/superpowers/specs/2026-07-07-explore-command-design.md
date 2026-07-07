# `/explore` — Ecosystem Skills & Agent-Persona Discovery, Pool, and On-the-Fly Materialization

Date: 2026-07-07
Status: **Designed** — approved via brainstorm (interactive journey mockup:
`docs/superpowers/design/2026-07-07-explore-command-journey-mockup.html`)
Related: `2026-07-07-skills-agent-persona-alignment-decision.md` (policy W5 gate),
`2026-04-19-skills-installer-design.md` (adapters this design reuses),
`2026-07-04-agent-profile-design.md` (persona materialization this design reuses)

## 1. Problem

SPUR ships first-party spurpower skills and two bespoke personas, but has no
way to adopt the ecosystem's assets: curated catalogs like
`hesreallyhim/awesome-claude-code`, `wshobson/agents` (194 agents / 158
skills), and `VoltAgent/awesome-claude-code-subagents` (154+ agents) hold
thousands of reusable SKILL.md skills and Claude-markdown personas. Users want
to browse those catalogs from inside SPUR, pick favourites for a project, and
have SPUR manage the selected skills/agents across its heterogeneous workers
— the **polyglot coding agent** concept: one pool, harness-native delivery to
whichever worker gets dispatched.

The alignment decision's W5 policy is a hard gate on this feature: it IS the
third-party ingestion path, and in-the-wild research (Snyk ToxicSkills) found
prompt injection in ~36% of sampled public skills. Ingestion without pinning
and review is not an option.

## 2. Validated user journey (six stages)

Validated interactively via the journey mockup. Command is `/explore` (chat
slash command) with a Ctrl+K palette entry opening the same full-screen view.

1. **Entry** — `/explore` or palette → Explore view.
2. **Browse** — tabs `Skills` / `Agent personas`; panes Sources → Catalog
   list → Preview (frontmatter, pinned sha, license, body excerpt, scan and
   conflict badges) — all readable *before* anything is adopted.
3. **Select** — space toggles ★; the selection is the project's **pool**.
   No target assignment exists anywhere in the journey: who loads what is
   decided per dispatch by orchestration.
4. **Gate** — per-item review cards: pin verification, license, deterministic
   scan verdict, conflicts vs bundled spurpower skills. Flagged items require
   an explicit resolution (block / override-with-justification;
   replace-bundled / skip). Unresolved items are excluded from Apply.
5. **Apply** — vendors pinned bodies into the pool, writes the manifest,
   renders pool *personas* into `.spur/agents/`. Writes **no** harness skill
   files — materialization belongs to dispatch, not install.
6. **Manage** — pool lifecycle: drift vs upstream, update (re-enters the
   gate), remove; lenses `Pool` and `Last materialization` (what each recent
   dispatch actually loaded).

An "Orchestrate" window was prototyped and **cut**: on-the-fly materialization
needs no UI of its own; its only surface is the Manage lens.

## 3. Locked decisions (defaults accepted at brainstorm)

| Question | Decision |
|---|---|
| Entry surface | `/explore` slash command + palette, full-screen view |
| Catalog freshness | Offline pinned index, explicit `sync` refreshes (sandbox/VM-safe, deterministic) |
| Pool manifest | Committed `.spur/explore.toml` |
| Gate strictness | Warn + explicit override, recorded in the manifest |
| Body storage | Vendor full bodies into the repo (reviewable diffs, offline dispatch) |
| Materialization placement | Ephemeral: rendered into the worker worktree at dispatch, never committed |
| Subset per dispatch | Brain picks a task-relevant subset (explicit override supported) |
| Drift checking | Manual `sync` + staleness badge in the TUI |

## 4. Architecture

New engine module `crates/spur-core/src/explore/`, sibling of `skills/`:

- `catalog.rs` — normalized index model + sync (parse SKILL.md frontmatter
  for skills, Claude-markdown frontmatter for personas, from pinned source
  checkouts).
- `pool.rs` — vendored body store + `.spur/explore.toml` manifest
  (round-trip, repair).
- `gate.rs` — pin/sha verification, license check, deterministic heuristic
  scan, bundled-conflict detection, verdict + override records.
- `materialize.rs` — dispatch-time rendering. **Delegates to the existing
  `skills::adapters` render functions** for skills and to the agent-profile
  per-kind materialization for personas. No second render path.

Surfaces over the engine:

- **CLI**: `spur explore sync | list | add | remove | status` (spur-cli) —
  the engine layer, also used headless/CI.
- **TUI**: `ExploreView` (spur-tui), entered via `/explore` slash command and
  a palette entry; follows existing list/preview component and keybinding
  conventions (j/k, tab, space, enter).
- **Brain**: reads pool metadata through spur-core when composing dispatch
  subsets.

## 5. Data model (all committed)

- **Index** — `.spur/explore/index/`: entries
  `{kind: skill|agent, name, source, path, pinned_commit, description,
  license, content_sha}`. Committed so Browse works fully offline; `sync`
  regenerates it and reports upstream drift.
- **Pool** — `.spur/explore/pool/<owner>/<name>@<short-sha>/`: vendored
  bodies (SKILL.md directory or persona markdown).
- **Manifest** — `.spur/explore.toml`: per item — source, pin, content
  sha256, license, gate verdict (`clean | overridden | replaced-bundled`),
  override justification, who/when. The manifest is the audit trail W5
  requires.
- **Personas** — on Apply, rendered into `.spur/agents/<name>.md` with the
  `SPUR-MANAGED` sha256 marker; they become delegation targets immediately
  via the existing agent-profile system. Skills never install at Apply.

## 6. Sync and gate

`spur explore sync` runs host-side (network allowed there and only there):
shallow-fetch each source at a pinned ref, rebuild the index, compute
upstream drift. Workers, worker sandboxes, and the remote build VM never
fetch.

Gate checks run at add-time and re-run on every update:

1. **Pin/sha verification** — vendored content must hash to the manifest's
   sha256.
2. **License** — present and recognized; `unknown` is a warn.
3. **Deterministic scan** — regex/static rules only in v1 (reproducible
   verdicts): injection-imperative patterns in bodies ("ignore previous
   constraints" class), unpinned network calls and opaque blobs in bundled
   scripts.
4. **Conflict detection** — name/near-description collision against bundled
   spurpower skills (duplicate activation ambiguity); resolution is
   replace-bundled or skip.

No LLM in the v1 scanner. Unresolved gate items are always excluded from
Apply — never silently included.

## 7. Dispatch integration — on-the-fly materialization

**Timing constraint (load-bearing):** harnesses discover skills and agents at
session startup — that is when the name+description discovery pass loads.
Materialization therefore runs in the dispatch pipeline **after worktree
provisioning and before the worker's ACP session spawns** (between
spur-worktree provisioning and spur-acp session start in the reconciler's
dispatch path).

**Delivery constraint:** materialization follows each coding agent's native
skills/agents configuration — the worker finds its assets exactly the way its
harness always loads them; nothing is injected mid-session:

| Worker kind | Skills | Personas |
|---|---|---|
| claude | `<wt>/.claude/skills/<name>/SKILL.md` | `<wt>/.claude/agents/<name>.md` |
| codex | `<wt>/.codex/skills/<name>/SKILL.md` | `<wt>/.codex/agents/<name>.toml` (developer_instructions, model, effort, sandbox mapping) |
| gemini | `<wt>/.gemini/skills/<name>/SKILL.md` | `<wt>/.gemini/agents/<name>.md` |
| kiro | `<wt>/.kiro/skills/<name>/SKILL.md` + steering pointer | per agent-profile mapping |
| opencode / kimi | `<wt>/.<kind>/skills/<name>/SKILL.md` | per agent-profile mapping |

Mechanics:

- The brain picks a **task-relevant subset** of the pool per delegation
  (description-vs-task matching), overridable with an explicit `skills:`
  param on the delegation. Bundled spurpower skills are always present and
  unaffected.
- Rendered files carry the `SPUR-MANAGED` marker and are appended to the
  worktree's `.git/info/exclude`, so ephemeral materialization can never
  leak into worker commits.
- **Loops**: subset re-evaluated between generations, never within one — a
  generation runs against a consistent skill set.
- **Failure mode**: if materialization fails, dispatch proceeds with bundled
  skills only and emits a worker signal — degraded, never silent.

## 8. TUI view

`ExploreView` implements the six-stage journey per the mockup: Skills/Agents
tabs; Sources / Catalog / Preview panes; badges (`in pool`, `conflict`,
`scan ⚠`, `new`, staleness banner); gate cards with explicit resolution
buttons; apply log; Manage with `Pool` and `Last materialization` lenses.
Keybindings register in the app-level keybinding map and follow existing
conventions. Offline open serves the committed index with a "synced Xh ago"
banner.

## 9. Error handling

- `sync` unavailable/offline → Browse serves the committed index, staleness
  banner shown; nothing blocks.
- Manifest/pool inconsistency (entry without body, sha mismatch) →
  `spur explore status` reports; Apply repairs by re-vendoring at the pinned
  sha.
- Gate-blocked items are visibly excluded in Apply output and Manage.
- Materialization failure → see §7 failure mode.

## 10. Testing

- **Engine unit tests** (spur-core): catalog parsing from fixture
  SKILL.md/persona files; gate heuristics against fixture malicious bodies
  (injection patterns, unpinned curl); manifest round-trip and repair;
  pool→worktree materialization against a temp git repo, asserting
  `.git/info/exclude` hygiene. No network in any test; sync is tested
  against local fixture git repos.
- **TUI tests**: in-process `TestBackend` + golden files for ExploreView
  (existing `UPDATE_GOLDEN` idiom).
- **E2E**: one vhs tape (looks) + one shell-use journey (does) on the
  e2e layer, per the authoring rule in `scripts/e2e/JOURNEYS.md`.

## 11. Phasing

- **Phase 1 — engine + CLI + delegate materialization**: catalog, pool,
  gate, manifest, `spur explore` subcommands, dispatch-time rendering on the
  delegate path. Full value headless, before any TUI work.
- **Phase 2 — TUI + plan/loop**: ExploreView, palette/slash entry,
  plan-task and loop-generation materialization, Manage lenses.

## 12. Non-goals (v1)

- LLM-assisted scanning (deterministic rules only; revisit later).
- Auto-update PRs / scheduled drift jobs (manual `sync` + badge only).
- Publishing skills back to the ecosystem.
- Distributing pool items to human IDE surfaces (Cursor etc.) — the
  committed spurpower fanout continues to serve humans; pool materialization
  targets worker dispatches only.
- Palette quick-add without the full view.

## 13. Relationship to prior decisions

- Triggers and satisfies **W5** from the alignment decision: pinned sha +
  recorded review is a blocking gate in the ingestion path.
- Reuses the marker-guarded installer adapters (`skills::adapters`) and the
  agent-profile per-kind persona materialization — no parallel render
  stacks.
- Makes **W4** (bundled-duplicate hygiene) more urgent: conflict detection
  assumes the bundled set itself is duplicate-free.
