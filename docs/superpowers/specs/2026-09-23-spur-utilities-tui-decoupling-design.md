# spur-utilities: Refactoring spur-tui Into a Render-Only Frontend

**Status:** Proposed design for review (rev 2 — see Appendix B)
**Date:** 2026-09-23
**Design epic:** `bd-2n81` (spur-mentions reuse family)
**Authoritative artifacts:** this spec + `2026-08-27-spur-mentions-notebook-integration-design.ipynb`
**Solver decision:** `sol_3a65e72044b7461d` (v1, Z3 Optimize, `sat`, termination `complete`, 25/36 soft weight);
derived-property proof `sol_54fb64b48f464bcf` (`unsat`); P2 re-solve `sol_98335c9542b74ecc`.
Reload with `get_solve_result({ solve_id: "<id>" })`. Supersedes `sol_0968671aacea44ff` (Appendix A.4).

## 0. Decision summary

Extract the non-rendering logic inside `crates/spur-tui/src/{mentions,commands}` into a
crate family rooted at `crates/spur-utilities/`, leaving `spur-tui` as the
render/schedule/policy frontend:

- **`spur-utilities`** — facade crate. Re-exports only; zero dependencies of its own.
- **`spur-mentions`** — neutral completion engine (per the 2026-08-27 spec; no ACP types,
  optional `code` feature → `spur-graph`).
- **`spur-commands`** — neutral slash-command model, registry, capability synthesis,
  and pure submit helpers (`spur-acp` is its domain).

Naming is `domain_names_with_facade`: sub-crates keep package names `spur-mentions` /
`spur-commands` (spec continuity, notebook rev-pinning seam, rollback points); the
family grouping is physical, under one directory.

The solve has three kinds of content. Only the third is a solver conclusion.

**Design premises** (encoded as hard rules — asserted, not derived):

| Premise | Encoding |
|---|---|
| Consumers keep behavior | `tui_m ∧ tui_c ∧ nb_m` |
| No shared→frontend edges | all of `m_tui, m_nb, m_u, c_tui, c_nb, c_u, u_tui, u_nb` false |
| Mentions has no *direct* ACP edge | `¬m_acp` |
| Commands owns ACP | `c_acp` |
| No commands→mentions edge | `¬c_m` (TUI composes code-mention expansion itself) |
| Facade is pure | `u_m ∧ u_c ∧ ¬u_acp` |
| Notebook takes no facade and no `code` | `¬nb_u ∧ ¬f_code_nb` |
| `code` feature exists; TUI enables it | `m_g ∧ tui_code` |
| Neutral dispatch + pure submit split | `p_dispatch ∧ p_submit_pure` |
| TUI caps synthesis unified in commands crate (C4) | `p_caps_shared` |
| Registry moves to commands; `spur_local` stays in TUI | `reg_in_commands ∧ spur_local_in_tui` |
| Notebook adopting `spur-commands` needs one spur rev | `nb_c ⇒ nb_lockstep_pins` (catalog proof, §5 P2) |

**Manifest facts** (HEAD `f020728`): `spur-graph → spur-mcp → spur-acp`; `spur-graph` has
no direct `spur-acp` edge; `commands/registry.rs::ensure_cache` reads `SpurLocalSource`.

**Derived — proved forced by premises + facts** (`sol_54fb64b48f464bcf`: every
counterexample is `unsat`):

| Property | Forced value | Why |
|---|---|---|
| Mentions hygiene gate scope | default features only | with `code`, `spur-mentions` reaches `spur-acp` via `spur-graph → spur-mcp` |
| Facade hygiene gate | direct deps only (`--depth 1`) | the full tree contains `spur-acp` through `spur-commands` |
| Registry local layer | injected by the frontend (§3.2) | otherwise the moved registry needs a `spur-commands → spur-tui` edge |
| `--no-default-features` gate for `spur-mentions` | required | `--workspace` builds unify features, so the notebook's configuration is never compiled otherwise |

**Preferences** (v1): `s1_nb_minimal_seam` (`¬nb_c`) w10 ✅, `s2_spec_name_continuity`
w9 ✅, `s3_family_uniformity` w3 ❌, `s4_tui_direct_deps` w6 ✅, `s5_nb_caps_adoption`
(`nb_c`) w8 ❌. Two real trade-offs: naming (w9 beats w3 on the same enum) and
notebook adoption of `spur-commands` (minimal seam w10 beats adoption w8 on `nb_c`).
Phase P2 lifts `s1`; the re-solve then adopts (§5).

## 1. Current-state evidence (re-verified at HEAD `f020728`)

### 1.1 Inventory inside spur-tui

| Path | Lines | Nature |
|---|---|---|
| `src/mentions/registry.rs` | 4,015 | god-file: sources, cache, ranking, worker composition, model probing, picker concerns |
| `src/mentions/code_graph/{source,expansion}.rs` | 1,655 | code discovery/hydration/expansion (`expand`, `ExpandedMention`, `PER_PROMPT_CAP_BYTES`, `CONTEXT_HEADER_CAP_BYTES`) |
| `src/mentions/{entry,file_source}.rs` | 194 | neutral-ish entry + file traversal |
| `src/mentions/{worker,issue,datasource}_source.rs` + `issue_search.rs` | 588 | adapters bound to TUI/session state |
| `src/mentions/hint.rs` | 344 | InputBar policy; imports `components::input_bar::{ProtectedRange, RangeKind}` |
| `src/commands/submit_router.rs` | 1,897 | mixed pure block assembly + InputBar/Action routing |
| `src/commands/advertised.rs` | 1,465 | capability-evidence synthesis over `SpurAgentCaps` |
| `src/commands/registry.rs` | 1,021 | merge/resolve/collision policy; `ensure_cache` reads `SpurLocalSource` |
| `src/commands/{kiro_skills,fuzzy,entry}.rs` | 514 | pure (kiro_skills imports `crate::agents::build_entry`) |
| `src/commands/spur_local.rs` | 210 | TUI-owned meta commands; imports `action::{Action, ViewId}` |
| `src/agents/entry_builder.rs` | 248 | pure fn `build_entry`/`build_static_entry` (spur-acp only; incl. tests) |

### 1.2 TUI-type leaks (complete list)

1. `commands/entry.rs` → `Dispatch::SpurLocal(Action)` embeds `crate::action::Action`.
2. `commands/spur_local.rs` → `Action`, `ViewId`.
3. `commands/submit_router.rs` → `Action`, `input_bar::{ImageAttachment, ProtectedRange, RangeKind}`,
   `query_source::RetrievalAccept`, `mentions::code_graph::expansion`.
4. `commands/kiro_skills.rs` → `crate::agents::build_entry`.
5. `mentions/hint.rs` → `input_bar::{ProtectedRange, RangeKind}`.
6. `commands/registry.rs` → `super::spur_local::SpurLocalSource` (`:6`; read in
   `ensure_cache` at `:229`, `:237` to build the local layer and its exclusive-name
   shadowing), `crate::agents::build_static_entry` (`:103`, `from_configs`), and a
   `Dispatch::SpurLocal(_)` match (`:393`, `available_commands_for_session`).
7. `commands/advertised.rs` → a `Dispatch::SpurLocal(_)` match (`:274`); its in-file tests
   (from `:616`) build `crate::views::session_detail::SessionDetailView` (`:842`) and call
   `submit_router::route_with_caps`.

Test-only: `mentions/registry.rs` tests use `crate::theme::runtime::test_support`; they stay
with the TUI façade.

Other dependencies the two modules use: `spur-acp`, `spur-graph`, `spur-core`
(`agent_profiles` in `mentions/registry.rs::agent_slot_candidates`; `explore` in its
tests), `spur-pm` (issue adapters; registry tests only), `nucleo-matcher`, `ignore`,
`base64`, `serde_json`, `anyhow`, `tracing`, `chrono`. The `spur-core` and `spur-pm`
users stay in the TUI façade/adapters; neither crate may enter `spur-mentions`.

### 1.3 Reverse coupling (TUI consumers — the façade contract)

- `components/query_source.rs` — `MentionQuerySource` scheduling: `MentionQueryWork`,
  `MentionCacheBuildWork/Result`, `prepare_query_work_nonblocking`, `run_query_work`,
  `finalize_worker_slot_pick`, `OpenWorkerSlot`, `CompletionScope`, `MentionEntry`.
- `views/session_detail/state.rs` — constructors `from_configs`, `for_brain_session`,
  `with_code_graph_from_env`, `set_issue_snapshot`, `maybe_merge_kiro_skills`.
- `app/events.rs` — `WorkerMentionDescriptor` snapshot build.
- `components/input_completion.rs`, `palette_sources.rs`, `completion_trigger.rs`,
  `app/overlays.rs` — `CommandRegistry::{new, default, from_configs, ensure_cache, list,
  resolve, available_commands_for_session, canonical_typed_form, arg_picker_spec,
  set_agent_commands, set_advertised_commands}`, `CommandSource`, `Dispatch`,
  `submit_router::{route, route_with_caps, blocks_preview, flatten_prompt_block,
  local_action_from_picker_accept, assemble_blocks_with_code_mentions[_and_caps]}`.
  There are 98 `CommandRegistry::{new, default, from_configs}` call sites across
  `src/` and `tests/`; §3.2 keeps them compiling with identical behavior.
- `app/action_routing/`, `input_history.rs`, `components/react_trace/dispatch.rs` —
  `flatten_prompt_block`, `blocks_preview`.
- Public module paths: 5 integration tests import `spur_tui::mentions::…`
  (`code_graph_expansion`, `dashboard_composer_contract`, `mention_registry`,
  `mentions_v2_picker`, `worker_mention_send_path`); 24 files import `spur_tui::commands::…`.

### 1.4 Known duplication with spur-notebook (re-verified at notebook HEAD `6f163e18`)

- Notebook `src/commands/chat/helpers.inc.rs:887 mention_complete_in_workspace` rescans
  the workspace per request (no cache, no ignore rules, substring, limit 100) — replaced
  wholesale by the engine in phase N.
- Notebook `helpers.inc.rs:410 chat_session_config_options_from_caps` re-implements the
  grok/kiro model+effort catalog synthesis that `commands/advertised.rs` performs over
  the same `SpurAgentCaps` — unified behind `spur-commands` (TUI at C4; notebook at P2).
- Notebook pins `spur-acp/core/graph/solver` at exact rev `8b388195…` with a comment
  anticipating "the notebook-owned shared-crate boundary" — the seam exists. Each git
  rev is a distinct crate instance to Cargo; §5 P2 states the consequence.

## 2. Target architecture

### 2.1 Workspace layout

```
crates/
├── spur-utilities/                 # family root (directory only)
│   ├── facade/                     # package: spur-utilities
│   │   └── src/lib.rs              # re-exports, nothing else
│   ├── mentions/                   # package: spur-mentions
│   │   └── src/{lib,engine,entry,cache,ranker,file_source,profiles}.rs
│   │       + src/code/{source,expansion}.rs          # behind `code` feature
│   └── commands/                   # package: spur-commands
│       └── src/{lib,entry,registry,advertised,fuzzy,kiro_skills,
│                entry_builder,submit,errors}.rs
└── spur-tui/                       # render/schedule/policy only
    └── src/
        ├── mentions/               # façade + adapters + sidecar (module path unchanged)
        ├── commands/               # registry newtype + spur_local (Action table) + submit shell
        └── …                       # unchanged components/, views/, app/
```

The TUI module keeps the name `mentions`. Renaming it would break the public
`spur_tui::mentions::…` paths in §1.3, three of which are M-phase gates, and
`crate::mentions` does not collide with the extern crate `spur_mentions`.

Root `Cargo.toml`: add the three paths to `members` and to `[workspace.dependencies]`
as `path` deps (git-rev publishing is a release concern, not a workspace concern).
Hoist `nucleo-matcher`, `ignore`, and `base64` into `[workspace.dependencies]` (today
they are declared per crate in spur-tui) so the TUI and the family crates cannot drift
to different versions.

### 2.2 Dependency edges (binding)

```
spur-tui ──► spur-mentions (features = ["code"])   # direct, not via facade
spur-tui ──► spur-commands                          # direct
spur-notebook ──► spur-mentions (default-features = false)   # phase N
spur-utilities ──► { spur-mentions, spur-commands } # facade re-export only
spur-mentions ──► { nucleo-matcher, ignore, url } (+ spur-graph iff `code`)
                   with `code`: spur-graph ──► spur-mcp ──► spur-acp   # transitive
spur-commands ──► { spur-acp, nucleo-matcher, base64, serde_json }
```

Forbidden (hard premises, not derived): any shared→frontend edge, a direct
`spur-mentions → spur-acp` edge, `spur-commands → spur-mentions`, and any facade edge
other than the two family crates.

ACP neutrality of `spur-mentions` is a **default-features** property. With `code`,
`spur-mentions` depends on `spur-acp` transitively (proved: `sol_831d5e7c3a7b4fcc` is
`unsat` against a transitive "no `spur-acp` in the tree" gate). This is acceptable: no
ACP type crosses the `spur-mentions` API. It is also why the notebook
(`default-features = false`) stays ACP-free through this crate. No new cycle is possible:
`spur-graph`, `spur-mcp`, and `spur-acp` have no edges into the family or the frontends.

### 2.3 Facade contract (`spur-utilities`)

```rust
pub use spur_commands as commands;
pub use spur_mentions as mentions;
```

No types, no logic, and the facade never enables `code`. Cargo's resolver 2 still unifies
features across a `--workspace` build: spur-tui enables `code`, so every workspace build
compiles `spur-mentions` with `code` on. Only the dedicated gate in §5 compiles the
default-features configuration. New family members join by adding a re-export;
consumers of the facade see them without code changes on their side.

## 3. `spur-commands` detailed design (new)

### 3.1 Neutralized entry model

`Dispatch::SpurLocal(Action)` is replaced by a frontend-agnostic variant; all other
variants (`PromptText`, `VendorExec`, `SetSessionConfigOption`, `SetSessionModel`,
`SetSessionEffort`, `SetSessionMode`) move unchanged:

```rust
pub enum Dispatch {
    /// A frontend-owned meta command, resolved by name at the boundary.
    /// TUI maps `name` (+ optional arg) to its own Action via a static table.
    Local { name: String },           // was SpurLocal(Action)
    PromptText { normalized: String },
    VendorExec { method: String, command: String, args_template: ArgsTemplateKind },
    SetSessionConfigOption { config_id: String },
    SetSessionModel,
    SetSessionEffort,
    SetSessionMode,
}
```

`String` matches the other variants and adds no dependency (`SmolStr` is not in the
workspace). `CommandEntry { name, description, hint, source, dispatch, arg_picker_spec }` and
`CommandSource { Spur, Agent{handle}, Advertised{handle} }` move verbatim (already
UI-neutral). `spur_acp::adapter::arg_picker_hint::ArgPickerSpec` remains in the entry —
allowed because `spur-commands` owns the ACP domain here.

### 3.2 Local layer injection and TUI action table

The registry's merge policy (`ensure_cache`) needs the frontend's meta-commands: they
are listed first, and "exclusive" names shadow same-named static, dynamic, and
advertised entries. The solve forces this layer to be injected (§0), so:

```rust
// spur-commands
pub struct LocalLayer { pub entries: Vec<CommandEntry>, pub exclusive_names: Vec<String> }
impl LocalLayer { pub fn empty() -> Self; }
impl CommandRegistry {
    pub fn new(local: LocalLayer) -> Self;
    pub fn from_configs(configs: &[AgentConfig], local: LocalLayer) -> Self;
}
// no `Default` impl in the shared crate
```

`ensure_cache` reads `self.local` where it reads `SpurLocalSource` today. The shared
constructors *require* the layer on purpose. A zero-argument shared `new()` would let a
TUI path build a registry without `/clear` and the other meta-commands, and it would
still compile.

TUI side (`src/commands/`):

- `CommandRegistry` becomes a newtype over `spur_commands::CommandRegistry` with
  `Deref`/`DerefMut`. Its `new()`, `Default`, and `from_configs()` install
  `SpurLocalSource::layer()`. All 98 existing constructor sites (§1.3) compile unchanged
  and behave identically. Functions in `spur-commands` take
  `&spur_commands::CommandRegistry` and receive the TUI registry through deref coercion.
- `spur_local.rs` keeps `Action`/`ViewId`, exposes `SpurLocalSource::layer() -> LocalLayer`
  (entries carry `Dispatch::Local { name }`), and gains one pure function:

```rust
pub fn local_dispatch(name: &str, arg: Option<&str>) -> Option<Action>;
```

- `local_dispatch("clear", _) == Some(Action::ClearSession)`, … for every entry in
  `SpurLocalSource::entries()`.
- `submit_router::local_action_from_picker_accept` and `SubmitDecision::Local { action }`
  resolve through this table instead of carrying `Action` in shared types.
- Round-trip test (mirrors the ACP envelope convention in
  `crates/spur-acp/tests/executor_events_roundtrip.rs`):
  every `SpurLocalSource::entries()` name resolves via `local_dispatch`, and every
  `local_dispatch` result traces back to exactly one catalog name.
- Injection test: a registry built through each TUI constructor lists every
  `SpurLocalSource` entry, and every exclusive name still shadows a same-named agent entry.
- The registry's shadowing tests (`spur_local_meta_command_shadows_agent_entry_with_same_name`,
  `spur_local_shadows_advertised_with_same_name`,
  `available_commands_for_session_keeps_prompt_text_and_spur_local`) move to
  `spur-commands` against a fixture `LocalLayer`. A TUI copy keeps them against the real catalog.

### 3.3 Module map (file → new home)

| Current | New home | Change |
|---|---|---|
| `commands/entry.rs` | `spur-commands/src/entry.rs` | neutralize `SpurLocal` → `Local{name}` |
| `commands/registry.rs` | `spur-commands/src/registry.rs` | inject `LocalLayer` (§3.2) in place of `SpurLocalSource`; `build_static_entry` → `crate::entry_builder`; `SpurLocal(_)` match → `Local { .. }` |
| `commands/advertised.rs` | `spur-commands/src/advertised.rs` | + public caps-synthesis surface (§3.5); `SpurLocal(_)` match → `Local { .. }`; in-file tests that build `SessionDetailView` or call `route_with_caps` move to `spur-tui/tests/` |
| `commands/fuzzy.rs` | `spur-commands/src/fuzzy.rs` | verbatim |
| `commands/kiro_skills.rs` | `spur-commands/src/kiro_skills.rs` | swap `crate::agents::build_entry` → `crate::entry_builder::build_entry` |
| `agents/entry_builder.rs` | `spur-commands/src/entry_builder.rs` | verbatim (pure) |
| `commands/spur_local.rs` | **stays** `spur-tui/src/commands/spur_local.rs` | + `layer()` + `local_dispatch` table |
| `commands/submit_router.rs` | split — see §3.4 | |
| `commands/mod.rs` | `spur-tui/src/commands/mod.rs` | TUI `CommandRegistry` newtype + re-exports from `spur_commands` + local table |

### 3.4 Submit router split

`submit_router.rs` (1,897 lines) splits along the seam the solve fixed
(`p_submit_pure`):

**Moves to `spur-commands/src/submit.rs` (pure, no TUI types):**
- `flatten_prompt_block(&ContentBlock) -> Option<String>`
- `mention_name_from_uri(uri) -> String`
- `blocks_preview(&[ContentBlock]) -> String`, `blocks_to_text(&[ContentBlock]) -> String`
- block-assembly core: text segmentation around mention positions, `ResourceLink`
  construction, `PromptBlockCaps::from_agent`, capability-route reduction
  (`pinned_route_for_command` usage) — operating on a neutral `MentionSpan` input
  (`{start, end, name, uri}`) rather than `ProtectedRange`.
- `route`/`route_with_caps` decision core → `classify(...) -> SubmitPlan` where
  `SubmitPlan` mirrors `SubmitDecision` with `Local { name, arg }` instead of
  `Local { action: Action }`. (Exact variant decomposition is finalized test-first at C3;
  the integration test `tests/submit_router.rs` is the parity gate.)

**Stays in `spur-tui` (becomes `src/commands/submit_shell.rs`):**
- `ProtectedRange`/`RangeKind`/`ImageAttachment` handling and the conversion
  `ProtectedRange → MentionSpan`.
- `RetrievalAccept` flow and code-mention payload lookup closures
  (`assemble_blocks_with_code_mentions*` wrappers stay TUI because they call
  `mentions::code_graph::expansion` — the `¬c_m` rule).
- `SubmitDecision::Local { action }` production via `local_dispatch`.

**Invariants:** signature-compatible wrappers keep every existing TUI call site
compiling unchanged during migration; the wrappers are deleted only after the parity
gate passes.

### 3.5 Caps synthesis unification (`p_caps_shared`)

`advertised.rs` exposes the model/effort catalog as neutral data:

```rust
pub struct ModelEffortCatalog { pub models: Vec<CatalogChoice>, pub current_model: Option<String>,
                                pub efforts: Vec<CatalogChoice>, pub current_effort: Option<String> }
impl ModelEffortCatalog { pub fn from_caps(caps: &SpurAgentCaps) -> Self; }
```

- Implementation is the grok/kiro/standard-config-options precedence that both
  `advertised.rs` (→ `CommandEntry`) and notebook
  `chat_session_config_options_from_caps` (→ `ChatConfigChoice`) duplicate today.
- TUI maps `ModelEffortCatalog` → `CommandEntry`s (C4). Notebook maps →
  `ChatSessionConfigOptions` (P2). Neither mapping lives in the shared crate.
- `from_caps` takes `spur-commands`' own `spur_acp::SpurAgentCaps`, so any consumer must
  resolve `spur-acp` at the same rev as `spur-commands` (§5 P2).

## 4. `spur-mentions` — TUI-side deltas only

Engine internals (neutral `MentionEntry`, `MentionSource::build → SourceSnapshot`,
`MentionEngine::query`, cache identity `CacheKey = canonical_root + source_key +
profile_fingerprint + source_token`, TTL 600s, `rank_top_k` via
`select_nth_unstable_by`, optional `code` module) are **owned by the 2026-08-27 spec**
and are not restated here. This spec adds the TUI-side obligations:

1. **Façade** `spur-tui/src/mentions/registry.rs` preserves the exact current
   caller surface (`query`, `prepare_query_work[_nonblocking]`, `run_query_work`,
   `apply_cache_build`, `for_direct_session`, `for_brain_session`,
   `with_code_graph[_from_env]`, `set_issue_snapshot`, `set_worker_snapshot_in_place`,
   `set_datasource_snapshot`, `lookup_code_payload`, `retain_code_payload*`,
   `drain_agent_model_catalog_probe_requests`,
   `mark_agent_model_catalog_probe_completed`, `take_worker_open_slot`,
   `clear_cache`, `CODE_GRAPH_INDEX_ENV`) delegating neutral work to the engine.
   Worker-slot composition (`agent_slot_candidates`, which reads
   `spur_core::agent_profiles`) and agent model-catalog probing
   (`spur_acp::agent_model_catalog`) stay in the façade; `spur-mentions` takes neither
   `spur-core` nor `spur-acp`.
2. **Adapters** `worker_source.rs`, `issue_source.rs`, `datasource_source.rs`,
   `issue_search.rs` stay in spur-tui (`src/mentions/`), implementing the shared
   `MentionSource` trait; `TuiMentionMetadata` sidecar (keyed by `MentionId`) carries
   worker agent/model/effort, `AgentKind`, CLI identity, issue previews, tags, atom
   text, unconsumed suffixes.
3. **`hint.rs` relocation**: `src/mentions/hint.rs` →
   `spur-tui/src/components/input_bar_mention_hint.rs`, next to `input_bar_wrap.rs`
   (`input_bar` is a single-file module). It is InputBar policy. Consumers:
   `components/input_completion.rs` (`prepend_worker_hint`) and
   `views/session_detail/input.rs` (`prepend_worker_hint`, `prepend_datasource_hint`).
4. **`MentionQuerySource` scheduling stays in TUI** (`components/query_source.rs`),
   including `spawn_blocking` work items and generation-based stale-result discard —
   respecting existing broadcast sizing and per-frame drain caps.
5. `code_graph/{source,expansion}.rs` move behind `spur-mentions`'s `code` feature;
   spur-tui enables `features = ["code"]`. Expansion stays callable from TUI only
   (see §3.4).

## 5. Migration phases, parity gates, rollback

Every phase: `scripts/spur-cargo clippy --workspace -- -D warnings` (remote) green,
`cargo fmt` local, phase commit + intent-focused message. Failing-test-first for
behavior-bearing changes (`test(...)` commit, then `fix(...)`), per repo rules.

From M1 on, every phase also runs the notebook configuration, which `--workspace`
never compiles (feature unification turns `code` on):
`scripts/spur-cargo clippy -p spur-mentions --no-default-features -- -D warnings` and
`scripts/spur-cargo test -p spur-mentions --no-default-features`.

Every phase is bracketed by a topology solve:
`python3 scripts/solve_topology_conformance.py` before starting and again after the
phase's gates pass. It loads the hard rules of `sol_3a65e72044b7461d`, fixes the crate
edges observed through `cargo metadata` plus the registry-injection and CI-gate facts,
and runs Z3 through `spur solver mcp` (no MCP client needed). Incremental mode must
return `sat`. `unsat` prints the violated rule in `unsat_core`; `unknown`/`timeout` is
inconclusive, never a pass. After the last phase, `--mode final` fixes every observed
fact and must also be `sat`. The pre-plan baseline is `sol_4826907bcf4748c3` (`sat`).

### Phase 0 — scaffold (no behavior)
- Create `crates/spur-utilities/{facade,mentions,commands}` with empty libs; wire
  members + `[workspace.dependencies]` (incl. the hoisted external deps, §2.1); facade
  re-exports compile.
- **Gate:** workspace builds; no spur-tui change yet. **Rollback:** delete dirs.

### Phase M — mentions extraction (follows 2026-08-27 spec §7)
- **M1** neutral `MentionEntry` + sidecar types compile in `spur-mentions`; TUI maps
  internally. **Gate:** all spur-tui tests green.
- **M2** engine/cache/ranker + `file_source` profiles move; TUI registry becomes façade;
  `hint.rs` relocates. **Gates:** `tests/mention_registry.rs`, `tests/mentions_v2_picker.rs`,
  `tests/code_graph_expansion.rs` green; new `rank_top_k` exactness property test
  (vs full-sort) added and green.
- **M3** adapters re-point to shared trait; `query_source.rs` consumes façade.
  **Gates:** `tests/worker_mention_send_path.rs`, `tests/completion_popup_navigation.rs`,
  `tests/input_bar_protected_ranges.rs` green.
- **Rollback:** revert phase commits; crates stay dormant.

### Phase C — commands extraction
- **C1** in place inside spur-tui, before anything moves: `Dispatch::Local{name}`,
  `LocalLayer` injection into `CommandRegistry`, the TUI newtype, `SpurLocalSource::layer()`,
  and `local_dispatch`; round-trip and injection tests added. **Gates:**
  `tests/command_registry.rs`, `tests/static_command_end_to_end.rs` green.
- **C2** registry/advertised/fuzzy/kiro_skills/entry_builder move to `spur-commands`; the
  TUI-dependent `advertised.rs` tests move to `spur-tui/tests/`; TUI `mod.rs` re-exports.
  **Gates:** `tests/advertised_commands_event.rs`,
  `tests/session_detail_commands_integration.rs`, `tests/palette_rerank_bench_smoke.rs`,
  and the relocated advertised tests green.
- **C3** submit split per §3.4 behind wrappers. **Gates:** `tests/submit_router.rs`,
  `tests/worker_mention_send_path.rs`, `tests/review_submission.rs` green; wrappers
  removed in the same phase only after gates pass.
- **C4** `ModelEffortCatalog::from_caps` unifies synthesis; TUI advertises through it.
  **Gates:** `tests/status_bar_model_effort_render.rs`, `tests/codex_model_picker_smoke.rs`
  green.
- **Rollback:** revert phase commits.

### Phase N — notebook integration (owned by 2026-08-27 spec §6)
- Notebook pins `spur-mentions` at exact rev, `default-features = false`; engine state
  `Arc<Mutex<MentionEngine>>` beside `Arc<SidebarChatState>`; only
  `mention_complete_in_workspace`'s body swaps.
- Mixing revs is safe here: the existing `spur-*` pins may stay at `8b388195…` because
  `spur-mentions` without `code` has no `spur-*` dependencies, so no type crosses the
  rev boundary.
- **Gates:** notebook tests `src/commands/chat/tests.rs:60,125,150,186` unchanged and
  green.

### Phase P2 — caps dedup on notebook
- Notebook moves `spur-acp`, `spur-core`, `spur-graph`, `spur-solver` **and** the new
  `spur-commands` to one rev together (lockstep), then
  `chat_session_config_options_from_caps` maps `ModelEffortCatalog`.
- Adding `spur-commands` alone is infeasible. It resolves `spur-acp` at its own rev, so
  the notebook's `SpurAgentCaps` (from `spur-acp@8b388195…`) is a different type.
  Catalog rule `configuration.version_interval`, verify mode: pins as-is `fail` on
  `spur_commands → spur_acp` (`sol_14bc87d774be44cd`); lockstep `pass`
  (`sol_079865386a1f4645`). Bumping `spur-acp` alone breaks `spur-core`'s own
  `spur-acp` requirement instead, hence all four.
- Re-solve with `s1_nb_minimal_seam` lifted (`sol_98335c9542b74ecc`, `sat`, `complete`,
  23/26): adoption (`s5`, w8) is now unopposed, so `nb_c` becomes true and the lockstep
  premise forces the pin bump.
- **Gate:** the notebook builds against a single spur rev (`cargo tree` shows one
  `spur-acp`); notebook chat tests green.

### Continuous hygiene gate
An `xtask` check, run in CI alongside clippy:
- `cargo tree -p spur-mentions -e normal` (default features) contains neither
  `spur-acp` nor `ratatui`. It must not run with `--features code` or `--all-features`:
  that tree contains `spur-acp` by construction (§2.2).
- `cargo tree -p spur-mentions --features code -e normal` contains neither `ratatui` nor
  `spur-tui`.
- `cargo tree -p spur-utilities --depth 1 -e normal` lists only `spur-mentions` and
  `spur-commands`. The full tree contains `spur-acp` through `spur-commands` from C2 on.

## 6. Error handling and observability

- Mentions errors follow the 2026-08-27 spec §11 table (typed root/traversal/build/
  rank/hydrate failures; required sources fail the query, optional ones degrade).
- `spur-commands` errors stay typed at their boundaries: registry resolution never
  panics on unknown names (`resolve -> Option` unchanged); submit classification
  returns `SubmitPlan` decisions, not errors; ACP composition failures remain
  send-time errors owned by each frontend.
- Tracing: engine spans (root hash, source key, profile, N/M/K, cache hit, build/score/
  select/hydrate durations) per the existing spec; commands spans record handle,
  evidence epoch, route decision. Paths/query text stay redacted by default.

## 7. Risks

| Risk | Mitigation |
|---|---|
| Submit-router split regressions (1.9k lines) | wrappers keep call sites frozen; `tests/submit_router.rs` parity gate; test-first variant decomposition |
| Silent loss of meta-commands after the registry moves (a constructor path without the local layer) | shared constructors require `LocalLayer` and have no `Default`; the TUI newtype installs it; injection test over every TUI constructor |
| `MentionEntry` neutralization breaks picker UX (sidecar misses a field) | sidecar keyed by `MentionId`; M1 compiles both representations; M2/M3 gates include picker and send-path integration tests |
| Feature-gate drift (`code` leaking into default) | default-features hygiene `cargo tree` gate; `--no-default-features` clippy/test gate; notebook pins `default-features = false` |
| Notebook rev skew at P2 (two `spur-acp` instances) | lockstep bump of all `spur-*` pins (catalog-verified); P2 gate asserts a single `spur-acp` in the notebook tree |
| Facade becomes a junk drawer | binding rule: facade holds re-exports only (`¬u_acp`, no logic); new members need a spec note |
| Clone churn from neutral types | entries are short-lived per query; sidecar is a map, not per-row clones; existing `large_enum_variant` expectation documented |
| Double maintenance during migration | phases are short-lived; dormant crates are removed on rollback, not maintained |

## 8. Open questions

1. `SubmitDecision` exact variant decomposition (resolve test-first at C3).
2. `issue_search.rs` final home — adapter-owned today; confirm at M3 whether any
   pure part belongs to `spur-mentions`.
3. Facade publishing strategy: path-dep now; whether `spur-utilities` is ever
   published or stays an internal convenience alias.
4. Whether the notebook later consumes `spur-commands`' registry (not just caps) for
   its agent command surface — defer to a notebook-side spec after P2. It would pass
   `LocalLayer::empty()` or its own layer.

## Appendix A — Solver evidence

Per proof discipline: premises are asserted, not proved. `sat` means feasible and
optimized under the encoded preferences, not unique. Re-solve whenever an encoded
premise, fact, or weight changes.

### A.1 Topology model (41 variables, 26 hard rules)

Hard rules: `consumers_keep_behavior`, `no_reverse_edges`, `mentions_acp_neutral_direct`,
`commands_domain_acp`, `no_commands_to_mentions`, `umbrella_facade_only`,
`umbrella_mandated`, `notebook_mentions_only_seam`, `code_feature_shape`,
`tui_boundary_policy`, `tui_caps_unified_c4`, facts `fact_graph_depends_on_mcp`,
`fact_mcp_depends_on_acp`, `fact_graph_no_direct_acp`, `fact_registry_reads_local_catalog`,
reachability definitions `def_reach_g_acp`, `def_reach_m_acp_default`,
`def_reach_m_acp_code`, `def_reach_u_extra`, gate soundness `mentions_gate_passes`,
`facade_gate_passes`, plan premises `plan_registry_moves_to_commands`,
`plan_spur_local_stays_tui`, `feature_config_coverage`, `p2_lockstep_premise`,
`nb_caps_needs_catalog`. The reachability definitions follow the acyclic chain
`spur-mentions → spur-graph → spur-mcp → spur-acp`, so each has a unique fixed point.

| Solve | Query | Result |
|---|---|---|
| `sol_54fb64b48f464bcf` | hard rules ∧ (mentions gate uses all features ∨ facade gate uses full tree ∨ no injection ∨ no `--no-default-features` gate) | `unsat` — all four derived properties are forced |
| `sol_3a65e72044b7461d` | v1: hard + `s1`..`s5` | `sat`, `complete`, 25/36; violated `s3` (w3), `s5` (w8) |
| `sol_98335c9542b74ecc` | P2: hard + `s2`..`s5` (`s1` lifted) | `sat`, `complete`, 23/26; violated `s3` (w3); `nb_c` true, lockstep forced |

In the v1 model `nb_lockstep_pins` is unconstrained (`nb_c` is false); its value there
carries no meaning.

### A.2 Transitive ACP check

`sol_831d5e7c3a7b4fcc` (`unsat`): core = `spec_code_feature_shape`, the two manifest facts,
the reach definitions, and a transitive "no `spur-acp` in `spur-mentions`' tree" gate.
The direct rule `¬m_acp` is not in the core; the conflict is purely transitive.

### A.3 Notebook pin compatibility (catalog `configuration.finite_compatibility`)

Rule `configuration.version_interval`, verify mode, ordering `spur_git_revs`
(rank 0 = `8b388195…`, rank 1 = first rev containing `spur-commands`). A consumer at a rev
requires `spur-acp` at exactly that rev, because the workspace path dependency resolves
within the same checkout and `SpurAgentCaps` must be one type.

| Solve | Pins | Outcome |
|---|---|---|
| `sol_14bc87d774be44cd` | `spur-acp`, `spur-core` at 0; `spur-commands` at 1 | `fail` — binding `spur_commands → spur_acp` |
| `sol_079865386a1f4645` | all at 1 | `pass` |

### A.4 Superseded: `sol_0968671aacea44ff`

The rev-1 model pinned 23 of its 27 variables with unit hard rules. Its only real
decision was naming (w9 vs w3). Its `s5` rewarded `p_caps_shared`, a variable unrelated
to `nb_c`, so the stated "w10 minimal seam beats w8 adoption" trade-off did not exist.
Promoting `s1`, `s2`, `s4`, `s5` to hard stays `sat` (`sol_26aa93a00d0e4c8c`), so after
P2 lifted `s1`, `nb_c` would have been arbitrary. It also encoded only direct edges, so
it could not see the `code` → `spur-acp` path. Rev 2 keeps its premises and weights,
moves `p_caps_shared` into the premises, and points `s5` at notebook adoption (`nb_c`),
which is what the rev-1 prose described (re-target approved in review, 2026-09-23).

## Appendix B — Revision log

**rev 2 (2026-09-23)** — corrections from a code-level and solver audit:

- §1.2: added the missed leaks — `registry.rs` reads `SpurLocalSource` inside
  `ensure_cache` and calls `build_static_entry`; `advertised.rs` matches `SpurLocal` and
  its tests use `SessionDetailView`. Corrected the "only depends on" list (`spur-core`,
  `spur-pm`).
- §3.2/§3.3: `registry.rs` does not move verbatim. Added `LocalLayer` injection, a
  required-layer shared constructor, and a TUI newtype that keeps the 98 constructor
  sites unchanged.
- §2.1/§4: dropped the `mentions → tui_mentions` rename (it broke 5 public-path
  integration tests); `hint.rs` target fixed to `components/input_bar_mention_hint.rs`;
  it has two consumers, not one.
- §2.2/§2.3/§5: ACP neutrality scoped to default features; hygiene gates corrected
  (default-features mentions tree; `--depth 1` facade); added the
  `--no-default-features` gate that workspace feature unification makes necessary.
- §5 P2: lockstep bump of all `spur-*` pins (catalog-verified); P2 re-solve recorded.
- §0/Appendix A: premises, facts, derived properties, and preferences separated;
  soft rules no longer listed as hard; new solve IDs; rev-1 solve marked superseded.
- Minor: `entry_builder.rs` is 248 lines; `SubmitDecision::Local { action }`;
  `Dispatch::Local { name: String }` (no `SmolStr` dependency); hoisted external deps
  into `[workspace.dependencies]`; notebook HEAD re-verified.
