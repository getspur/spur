# JavaScript source extraction via TSX grammar reuse Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** cross-language evaluation against `anywidget` (Python+JS). The graph indexed py/ts/tsx
but produced **zero nodes for 18 `.js`/`.mjs`/`.cjs` files** — including real framework-integration
source (`packages/react/index.js`, `packages/vue/index.js`, `packages/svelte/src/index.js`,
`packages/vite/index.js`, `packages/anywidget/src/{index,plugin}.js`). For a "Python + JS" knowledge
graph this is the most fundamental coverage hole: half the JS surface is silently invisible.

**Root cause (exact, grounded):** `language_registry()`
(`crates/spur-graph/src/extract/languages.rs`) registers six descriptors — Rust/Python/TypeScript/
Tsx/Cpp/Markdown — with exact extensions (`ts` only, `tsx` only). `.js`/`.jsx`/`.mjs`/`.cjs` match no
descriptor, so `Language::from_path` returns `None` and the files are skipped entirely. (The
`language_family` helper in `tree_sitter.rs` already *anticipates* a `js` family — `js|jsx|mjs|cjs` —
that the extractor never emits: a standing internal inconsistency this plan resolves.)

**Why grammar REUSE, not a new dependency:** TypeScript is a syntactic superset of JavaScript, and the
existing `tree-sitter-typescript` crate ships the **TSX** grammar (`LANGUAGE_TSX`), which parses
plain JS *and* JSX. The TSX query set (`TSX_QUERIES`, incl. `queries/typescript/jsx-edges.scm`)
already covers JS/JSX call/construct/import edges. So JavaScript extraction needs **no new crate
dependency and no new `.scm` authoring** — a new `Language::Javascript` variant reuses the TSX
grammar + TSX queries, with an honest `"javascript"` label and the JS extension set. (React `.js`
files frequently contain JSX, so the TSX grammar — not the bare TypeScript grammar — is the correct
reuse target.)

📌 This is an **extraction** change, so it bumps **`EXTRACTOR_VERSION`** (not `RESOLVER_VERSION`), and
it adds new fixture content → **expect golden churn** confined to the new JS fixture.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

**Out of scope:** the relational language gate (separate plan, R2), TypeScript `.ts`/`.tsx` behavior
(unchanged), the closed-world calls/imports tail (Tier-2), and authoring a dedicated
`tree-sitter-javascript` grammar (explicitly rejected in favor of TSX reuse unless Step-1 testing
proves the TSX grammar mis-parses plain JS, in which case STOP and emit `risk` with the failing
sample rather than silently adding a dependency).

---

### Task javascript-extraction: register a JavaScript language via TSX grammar reuse

**Task ID:** `task-javascript-extraction`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/languages.rs` — add `Language::Javascript` variant,
  `javascript_config()` (reusing `LANGUAGE_TSX` + `TSX_QUERIES` via `typescript_config_for`),
  `javascript_matcher`, a registry descriptor with extensions `["js","jsx","mjs","cjs"]`,
  `builtin_method_names` → `TS_BUILTIN_METHODS`, and the `label()`/any `match self` arms that must
  stay exhaustive (e.g. the `references`/edge-relation arms near lines 1735/1774).
- Modify: `crates/spur-graph/src/store/build.rs` — `EXTRACTOR_VERSION` bump (line 27).
- Add: a small JavaScript fixture (a `.js` file with a function, a class with a method, a
  construction, and an import) under an existing corpus (e.g. `typescript_corpus`) **or** a new
  `javascript_corpus`, plus its blessed `expected_graph_index.json`.

**Depends on:** none

**Acceptance Criteria:**
- [ ] `Language::from_path` returns `Some(Language::Javascript)` for `.js`, `.jsx`, `.mjs`, `.cjs`
      (case-insensitive), and `javascript_config()` parses them with the TSX grammar + TSX queries.
- [ ] `Language::Javascript.label()` is `"javascript"`; `builtin_method_names()` returns
      `TS_BUILTIN_METHODS`; the variant is wired into every exhaustive `match self` in the file so it
      compiles without a wildcard that would silently mishandle it.
- [ ] `all_supported_extensions()` includes `js`, `jsx`, `mjs`, `cjs`. No change to the `ts`/`tsx`/
      `py`/`rs`/`cpp`/`md` descriptors.
- [ ] **Unit test** `javascript_files_route_to_javascript_language`: asserts `from_path` for each of
      `a.js`, `b.jsx`, `c.mjs`, `d.cjs` → `Language::Javascript`, and a `.ts` file is still
      `Language::TypeScript`.
- [ ] **Behavioral/fixture test:** the new JS fixture extracts the expected nodes (function, class,
      method) and edges (calls, constructs, imports) — proven by the blessed golden. A plain-JS file
      with JSX must parse without error (TSX grammar).
- [ ] Verify the previously-invisible class is now indexed: the JS fixture includes a top-level
      function + a class with a method, and the golden shows them as `function`/`class`/`method`
      nodes labeled language `javascript`.
- [ ] No cross-language binds introduced: with the v9 language gate live, a JS construction/call must
      not bind to a Rust/Python symbol. (Confirm in the golden: the JS fixture's edges resolve only
      within the JS family or stay unresolved.)
- [ ] `EXTRACTOR_VERSION` bumped (`build.rs:27`) to `"2026-06-05-javascript-extraction-v2"`.
- [ ] Goldens re-blessed; **expect churn only in the new JS fixture** (plus any deterministic
      re-extraction). If a NON-JS corpus's symbol set changes, STOP and emit `risk`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** new tests pass; node/edge counts for the JS fixture; confirmation `.js/.mjs/.cjs/.jsx`
      now extract; EXTRACTOR_VERSION bump confirmed; whether the TSX grammar parsed plain JS cleanly.

**Suggested Worker:** codex.

**Scope Boundary:** IN: `Language::Javascript` + `javascript_config`/`javascript_matcher` + registry
descriptor + builtin/label/match wiring + JS fixture & golden + `EXTRACTOR_VERSION`. OUT: a new
tree-sitter-javascript dependency (use TSX reuse), new `.scm` query files, the relational language
gate, the resolver arms, `RESOLVER_VERSION`, other crates, `schema.rs`.

**Implementation:**

- [ ] **Step 1: Verify TSX-grammar JS parsing + failing test.** Add
  `javascript_files_route_to_javascript_language` (will fail to compile/route until the variant
  exists) and a tiny JS fixture. Confirm the TSX grammar parses a plain-JS-with-JSX sample without
  error. If it mis-parses, STOP and emit `risk` (do not add a new grammar dependency unilaterally).

- [ ] **Step 2: Add the variant + config + matcher + descriptor**, mirroring `tsx_config`/
  `tsx_matcher`/the tsx descriptor. `javascript_config()` = `typescript_config_for(LANGUAGE_TSX.into(),
  TSX_QUERIES)`. Wire `label()`, `builtin_method_names()`, and all exhaustive `match self` arms.

- [ ] **Step 3: Bump `EXTRACTOR_VERSION`** (`build.rs:27`) → `"2026-06-05-javascript-extraction-v2"`.

- [ ] **Step 4: Bless goldens (expect JS-fixture churn only).**
```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

- [ ] **Step 5: Broad gate + commit:**
```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/languages.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/queries/ crates/spur-graph/tests/fixtures/
git commit -m "feat(spur-graph): extract JavaScript (.js/.jsx/.mjs/.cjs) via TSX grammar reuse"
```

## Self-Review
- **Coverage:** closes the JS extraction hole (zero → indexed) for `.js/.jsx/.mjs/.cjs`; resolves the
  `language_family`-anticipates-js inconsistency; reuses the TSX grammar + queries (no new dep, no new
  query authoring).
- **Risk:** TSX grammar is the proven superset for JS/JSX; v9 language gate already prevents
  cross-language binds for the new family; churn confined to the new fixture; honest `"javascript"`
  label keeps language attribution correct.
- **DAG:** single task.
- **Escape hatch:** if TSX mis-parses plain JS in Step 1, the worker emits `risk` rather than adding a
  dependency without review.
