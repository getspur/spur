# Structured-language extraction (JSON, TOML, YAML) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-23-structured-language-extraction-design.ipynb`
**Formal @spec cells:** `EXT-OWNER`, `ADAPTER-PROFILE`, `NAMED-KEY`
**Design epic:** `bd-2l2m` (closed)

**Goal:** Add JSON, TOML, and YAML as tree-sitter structured languages that emit named-key Field/Module/Constant symbols without stealing Jupyter `.ipynb` or indexing lockfiles.

**Architecture:** Follow the HCL adapter contract: registry row + `LanguageConfig` + `queries/<lang>/tags.scm`. No new `NodeKind`. Relations are `{contains, defines}` only. Lockfile skip is a matcher basename denylist. Pre-solve catalog models (`sol_5fc0e7e624f84ff2`, `sol_cd0123ca19864fd5`, `sol_56a05280e1504db3`) are the TDD witness; post-solve re-checks the landed registry snapshot.

**Tech Stack:** `tree-sitter` 0.25, `tree-sitter-json` 0.24.8, `tree-sitter-toml-ng` 0.7.0, `tree-sitter-yaml` 0.7.2.

**TDD + solve:** Every task is catalog-first. SOLVE PRE already proved the adapter profile. RED uses those predicates (unique extensions, structured+tree_sitter, named-key kinds). GREEN bakes them. SOLVE POST re-runs the same family verifies against the shipped registry.

---

### Task 1: Path routing + registry + grammars

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-graph/Cargo.toml`
- Modify: `crates/spur-graph/src/extract/languages.rs`
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (`language_family`)
- Create: `crates/spur-graph/tests/structured_language_extract.rs`
- Create: `crates/spur-graph/queries/json/tags.scm`
- Create: `crates/spur-graph/queries/toml/tags.scm`
- Create: `crates/spur-graph/queries/yaml/tags.scm`
- Modify: `crates/spur-graph/src/store/build.rs` (`MANIFEST_QUERY_BYTES`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `Language::from_path` maps `.json`→Json, `.toml`→Toml, `.yaml`/`.yml`→Yaml
- [ ] `.ipynb` stays JupyterNotebook; `.json` ≠ `.ipynb`
- [ ] lockfile basenames (`package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`) return `None`
- [ ] `.tf.json` returns Json (update the HCL test that currently asserts `None`)
- [ ] `every_registered_language_satisfies_query_contract` passes
- [ ] SOLVE PRE/POST unique-extension snapshot still pass/fail as specified

**Suggested Worker:** claude-code-acp (this session)

**Scope Boundary:**
- IN: registry, matchers, denylist, Language match arms, tags.scm, MANIFEST_QUERY_BYTES, routing tests
- OUT: README coverage matrices (task-2), YAML alias references, new NodeKind

**Implementation:**
1. SOLVE PRE (already sat): unique extensions; json vs ipynb exclusive; structured+tree_sitter required.
2. RED: `json_toml_yaml_paths_route_to_structured_languages` asserting `from_path` is `Some` for `foo.json` / `Cargo.toml` / `x.yaml` / `x.yml`.
3. Watch fail (`None` today).
4. GREEN: enum variants, crates, matchers, denylist, configs, tags, gate rows.
5. RED: lockfile paths stay `None` (fails if matcher is extension-only).
6. GREEN: basename denylist.
7. SOLVE POST: re-run unique + mutually_consistent against landed extensions.

---

### Task 2: Named-key fixtures + docs

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-graph/tests/structured_language_extract.rs`
- Modify: `crates/spur-graph/queries/README.md`
- Modify: `crates/spur-graph/README.md`
- Modify: `crates/spur-graph/tests/hcl_definition_query.rs` (`.tf.json` expectation)

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] `package.json` emits Constant `name`, Module `dependencies`, Field nested package keys
- [ ] `Cargo.toml` emits Module `package` / `dependencies` and named scalars
- [ ] GitHub Actions YAML emits Module `jobs` (and nested job name)
- [ ] `package-lock.json` produces zero extracted symbols
- [ ] `.ipynb` does not emit JSON pair symbols
- [ ] Coverage matrices updated; no `TODO` gaps for these families
- [ ] SOLVE POST named-key classification still pass

**Suggested Worker:** claude-code-acp (this session)

**Scope Boundary:**
- IN: query capture correctness, fixture extract tests, docs, HCL `.tf.json` note
- OUT: JSON Pointer, YAML anchors as references, lockfile parsing
