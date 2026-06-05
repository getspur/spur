# Make `function_singleton_safe` language-aware (Tier-1 cross-language phantom fix) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** graph-grounded review of the v8 `method_crate_singleton` commit (live artifact `2256c91d`).
A reviewer flagged the `STD_PRELUDE_METHOD_NAMES` denylist as Rust-overfit; investigation found a
**deeper, pre-existing defect**: `function_singleton_safe` gates on crate scope but **not language**.

**The bug (exact, grounded):** `function_singleton_safe` (`extract/tree_sitter.rs`, currently ~line
1242) returns true when two files share a `crates/<name>` scope. But a single crate tree can hold
**multiple languages** — e.g. `crates/spur-notebook/` contains Rust (`rest-table-gateway`) **and**
TypeScript (`jute-notebook`). So a TS method/function call passes the gate and binds to a Rust
symbol. Live evidence on the v7 graph (these are phantoms that already shipped via the v6 `singleton`
path):

| bind_method | src→dst language | count |
|---|---|---|
| singleton | rust → python | 9 |
| singleton | python → rust | 3 |
| singleton | rust → typescript | 1 |
| macro_body_singleton | rust → python | 1 |

Plus the v8 `method_crate_singleton` path would add method versions of the same bug (verified
candidates: TS `add` → Rust `…/nango-import.rs`; TS `reject` → Rust `…/peer_mailbox/router.rs` —
`Set.add`/`Promise.reject` JS builtins that the Rust-only denylist cannot catch). **v8 is not yet
rebuilt into the live graph, so this fix lands before those method phantoms ever reach it.**

**Why a language gate, not a bigger denylist:** chasing every language's builtin vocabulary is
unbounded and still wouldn't stop cross-language binds. A call can never cross a language boundary,
so the principled invariant is: **a singleton bind is safe only if source and target are the same
language family.** This is one function change that fixes all three resolution paths at once
(`function_singleton_safe` has exactly three production callers: the v6 Function arm + v8 method arm
in `resolve_singleton_bare_target`, the v7 References arm in `resolve_pending_edges`, and the
function drop-guard in `rebind_cross_file_edges`). The `STD_PRELUDE_METHOD_NAMES` denylist correctly
**demotes to a within-Rust std-collision heuristic** and is left as-is.

📌 **Golden artifacts:** the four corpora are each single-language, so within any corpus the language
families always match → the gate changes nothing there → **expect zero golden changes** (including no
regression to the v8 `method_crate_singleton` corpus binds, which are within-language). Bless is run
only to confirm none.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

**Out of scope (explicitly deferred):** the *intra-language, non-Rust* builtin-collision residual
(e.g. a TS `.forEach()` → a lone workspace `forEach` method, same-language). Magnitude ~0 today (TS
has 35 methods total); revisit with small per-language builtin sets when the notebook method surface
grows. This task does NOT address that — it only closes the cross-language class.

---

### Task singleton-language-gate: require same language family in `function_singleton_safe`

**Task ID:** `task-singleton-language-gate`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (add a `language_family` helper; add the
  same-language requirement to `function_singleton_safe`; add unit + behavioral tests in the `tests` module)
- Modify: `crates/spur-graph/src/store/build.rs` (`RESOLVER_VERSION` bump → v9)
- Regenerate (bless, only if changed): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `function_singleton_safe(src_file, tgt_file)` returns true ONLY when (current conditions) AND
      `language_family(src_file) == language_family(tgt_file)` with both `Some(_)`. The
      `src_file == tgt_file` early-return stays (same file ⇒ same language). When the language
      families differ, or either is `None` (unknown extension), return **false**.
- [ ] A new `language_family(path: &str) -> Option<&'static str>` maps file extension → family:
      `rs`→`rust`; `ts|tsx|js|jsx|mjs|cjs`→`js`; `py|pyi`→`python`;
      `cpp|cc|cxx|c|h|hpp|hxx`→`cpp`; `md|markdown`→`markdown`; unknown → `None`. (Extract the
      extension as the substring after the last `.`; a path with no `.` ⇒ `None`.)
- [ ] The four existing `function_singleton_safe_*` unit tests still pass unchanged (they all use
      `.rs` paths ⇒ same family). Add `function_singleton_safe_cross_language_blocks`: assert
      `!function_singleton_safe("crates/spur-notebook/jute-notebook/x.ts", "crates/spur-notebook/rest-table-gateway/y.rs")`,
      and a positive `function_singleton_safe_same_language_family_allows`: assert
      `function_singleton_safe("crates/foo/src/a.ts", "crates/foo/web/b.tsx")` (ts/tsx same family,
      same crate ⇒ true).
- [ ] A behavioral `build_facts` test (beside the v8 `method_crate_singleton` test) proving the gate
      reaches the resolver end-to-end: a two-language, same-crate fixture where a `.ts` file calls a
      bare function name whose ONLY workspace definition is a Rust function in the same crate; assert
      the resulting `calls` edge is **unresolved** (`target_node_id = None`). (If a bare TS→Rust
      *function* call is awkward to extract, use a method-call form mirroring the v8 fixture but with
      the def in `.rs` and the call in `.ts`; assert unresolved.)
- [ ] No change to `STD_PRELUDE_METHOD_NAMES`, the Function/Method/References arm bodies, the rebind
      drop-guard, `path_scope`/`path_crate`, or any caller — they all call `function_singleton_safe`
      and inherit the fix.
- [ ] Goldens re-blessed if changed; **expect none** (single-language corpora). If any golden
      changes, STOP and emit `risk` (a within-corpus change would mean the language map is wrong).
- [ ] `RESOLVER_VERSION` bumped to `"2026-06-05-singleton-language-gate-v9"`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** the new tests pass; whether any golden changed (expect none); v9 confirmation.
      (Live effect — ~14 cross-language `singleton` phantoms drop + the v8 method cross-language
      candidates never bind — is verified post-rebuild by the brain.)

**Suggested Worker:** codex.

**Scope Boundary:** IN: `language_family` + the `function_singleton_safe` same-language clause +
`RESOLVER_VERSION` + the unit/behavioral tests + conditional bless. OUT: the denylist, the resolver
arm bodies, the rebind drop-guard logic, `path_scope`/`path_crate`, the references/method/function
bind logic, import/qualified/dyn paths, other relations, other crates, `schema.rs`, and the
intra-language non-Rust builtin-collision residual (separate, deferred).

**Implementation:**

- [ ] **Step 1: Failing tests.** Add `function_singleton_safe_cross_language_blocks` +
  `function_singleton_safe_same_language_family_allows` unit tests, and the two-language `build_facts`
  behavioral test described above. Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
  function_singleton_safe` → expect the cross-language test to FAIL (current code binds across
  languages).

- [ ] **Step 2: Add `language_family` + the gate.**

```rust
pub(crate) fn function_singleton_safe(src_file: &str, tgt_file: &str) -> bool {
    if src_file == tgt_file {
        return true;
    }
    let src_crate = path_scope(src_file);
    let tgt_crate = path_scope(tgt_file);
    if src_crate.is_none() || src_crate != tgt_crate {
        return false;
    }
    // Same crate scope is not enough: one crates/<name> tree can hold multiple
    // languages (spur-notebook = Rust rest-table-gateway + TS jute-notebook).
    // A call never crosses a language boundary, so require the same family —
    // this is what stops cross-language singleton phantoms (e.g. TS Set.add /
    // Promise.reject binding to a same-named Rust method).
    matches!(
        (language_family(src_file), language_family(tgt_file)),
        (Some(a), Some(b)) if a == b
    )
}

/// File-extension → language family. `None` for unknown extensions (treated as unsafe
/// for cross-file singleton binds).
fn language_family(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    if ext == path {
        return None; // no extension
    }
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "js",
        "py" | "pyi" => "python",
        "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hxx" => "cpp",
        "md" | "markdown" => "markdown",
        _ => return None,
    })
}
```

  Run Step 1 → expect PASS (and the 4 existing tests stay green).

- [ ] **Step 3: Bump `RESOLVER_VERSION`** (`build.rs:29`) → `"2026-06-05-singleton-language-gate-v9"`.

- [ ] **Step 4: Bless goldens (expect NONE).**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Expect no fixture diff (single-language corpora). If any appears → STOP and emit `risk`.

- [ ] **Step 5: Broad gate + commit** (green except flaky `incremental_ingest`):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test artifact_range_invariants --test calls_range_resolution_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/fixtures/
git commit -m "fix(spur-graph): require same language family for singleton bind safety"
```

  Report: new tests pass, golden status (expect none), v9 confirmation.

## Self-Review
- **Coverage:** closes the cross-language singleton-phantom class across all three resolution paths
  (v6 function, v7 references, v8 method) with one shared-predicate change; fixes the ~14 phantoms
  already live and prevents the v8 method ones before rebuild.
- **Placeholder scan:** concrete gate + concrete `language_family` + concrete unit/behavioral tests;
  bless conditional and expected-empty.
- **Type consistency:** `path_scope`, `function_singleton_safe` (pub(crate)) already present;
  `language_family` is a new private free function beside them.
- **DAG:** single task.
- **Risk:** strictly tightens an existing safety predicate (drops phantoms, never adds binds);
  same-file/within-Rust behavior unchanged (1,928 v8 Rust binds + all v6 within-Rust binds survive);
  single-language corpora ⇒ zero golden churn; the four existing predicate tests use `.rs` paths and
  stay green. The denylist demotes cleanly to a within-Rust heuristic; the intra-language non-Rust
  residual is explicitly deferred.
